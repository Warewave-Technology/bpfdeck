//! `tseries` as a sparkline with last/min/max and the time range.

use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Paragraph, Sparkline};

use super::key_line;
use crate::model::panels::Panel;
use crate::ui::theme::Theme;

fn num(v: f64) -> String {
    if v.fract() == 0.0 && v.abs() < 1e15 {
        format!("{}", v as i64)
    } else {
        format!("{v:.3}")
    }
}

pub fn render(frame: &mut Frame, area: Rect, panel: &Panel) {
    let points = panel.selected_tseries().unwrap_or_default();
    let mut lines: Vec<Line> = key_line(panel).into_iter().collect();
    let (Some(first), Some(last)) = (points.first(), points.last()) else {
        lines.push(Line::styled("(empty)", Theme::muted()));
        frame.render_widget(Paragraph::new(lines), area);
        return;
    };
    let min = points.iter().map(|p| p.value).fold(f64::INFINITY, f64::min);
    let max = points.iter().map(|p| p.value).fold(f64::NEG_INFINITY, f64::max);
    lines.push(Line::from(vec![
        Span::styled("last ", Theme::label()),
        Span::styled(num(last.value), Theme::title()),
        Span::styled("  min ", Theme::label()),
        Span::raw(num(min)),
        Span::styled("  max ", Theme::label()),
        Span::raw(num(max)),
        Span::styled(format!("  {} points", points.len()), Theme::muted()),
    ]));
    lines.push(Line::styled(
        format!("{} → {}", first.interval_start, last.interval_start),
        Theme::muted(),
    ));

    let header_h = u16::try_from(lines.len()).unwrap_or(3);
    let [header, chart] = Layout::vertical([Constraint::Length(header_h), Constraint::Min(1)]).areas(area);
    frame.render_widget(Paragraph::new(lines), header);

    // Newest points on the right; values shifted so the minimum sits at the baseline.
    // A flat series is drawn at half height rather than as an empty chart.
    let width = usize::from(chart.width);
    let tail = &points[points.len().saturating_sub(width)..];
    let data: Vec<u64> = tail
        .iter()
        .map(|p| {
            let span = max - min;
            if span <= f64::EPSILON {
                500
            } else {
                1 + ((p.value - min) / span * 999.0).round() as u64
            }
        })
        .collect();
    frame.render_widget(
        Sparkline::default()
            .data(&data)
            .max(1001)
            .style(Theme::hist_bar()),
        chart,
    );
}
