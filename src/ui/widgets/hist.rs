//! Histogram panel: bpftrace-style labels, counts, eighth-block bars.

use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;

use super::{fit, key_line};
use crate::model::hist;
use crate::model::panels::Panel;
use crate::ui::theme::Theme;

pub fn render(frame: &mut Frame, area: Rect, panel: &Panel) {
    let mut lines: Vec<Line> = key_line(panel).into_iter().collect();
    let buckets = hist::trimmed(panel.selected_hist().unwrap_or_default());
    if buckets.is_empty() {
        lines.push(Line::styled("(empty)", Theme::muted()));
        frame.render_widget(Paragraph::new(lines), area);
        return;
    }
    let labels = hist::labels(buckets);
    let label_w = labels.iter().map(|l| l.chars().count()).max().unwrap_or(0);
    let count_w = buckets
        .iter()
        .map(|b| b.count.to_string().len())
        .max()
        .unwrap_or(1);
    let bar_w = usize::from(area.width).saturating_sub(label_w + count_w + 3);
    let max = buckets.iter().map(|b| b.count).max().unwrap_or(0);

    let room = usize::from(area.height).saturating_sub(lines.len());
    // All buckets when they fit (bpftrace-like); otherwise collapse long empty runs so
    // far-out outliers stay on screen.
    let rows = if buckets.len() <= room {
        hist::rows(buckets, usize::MAX)
    } else {
        hist::rows(buckets, 3)
    };
    let truncated = rows.len() > room;
    let shown = if truncated {
        room.saturating_sub(1)
    } else {
        rows.len()
    };
    for row in rows.iter().take(shown) {
        lines.push(match *row {
            hist::Row::Bucket(i) => Line::from(vec![
                Span::styled(fit(&labels[i], label_w), Theme::label()),
                Span::raw(format!(" {:>count_w$} ", buckets[i].count)),
                Span::styled(hist::bar(buckets[i].count, max, bar_w), Theme::hist_bar()),
            ]),
            hist::Row::Gap(n) => {
                Line::styled(format!("{:>label_w$}  {n} empty buckets", "⋮"), Theme::muted())
            }
        });
    }
    if truncated {
        lines.push(Line::styled(
            format!("… {} more rows", rows.len() - shown),
            Theme::muted(),
        ));
    }
    let total: u64 = buckets.iter().map(|b| b.count).sum();
    if lines.len() < usize::from(area.height) {
        lines.push(Line::styled(format!("total {total}"), Theme::muted()));
    }
    frame.render_widget(Paragraph::new(lines), area);
}
