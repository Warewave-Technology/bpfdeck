//! Top table for keyed maps: tuple keys split into columns, value, inline bar, deltas.

use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::Style;
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;

use super::fit;
use crate::model::hist::bar_f64;
use crate::model::panels::{Delta, Panel, PanelData, table_rows};
use crate::ui::theme::Theme;

/// Longest a key column may get before it is cut.
const KEY_MAX: usize = 32;

fn delta(d: Delta) -> (&'static str, Style) {
    match d {
        Delta::None | Delta::Same => (" ", Theme::muted()),
        Delta::New => ("+", Theme::delta_new()),
        Delta::Up => ("↑", Theme::delta_up()),
        Delta::Down => ("↓", Theme::delta_down()),
    }
}

pub fn render(frame: &mut Frame, area: Rect, panel: &Panel) {
    let PanelData::Table { rows, prev } = &panel.data else {
        return;
    };
    let rows = table_rows(rows, prev.as_ref(), panel.sort_by_key);
    if rows.is_empty() {
        frame.render_widget(Paragraph::new(Line::styled("(empty)", Theme::muted())), area);
        return;
    }
    let ncols = rows.iter().map(|r| r.cols.len()).max().unwrap_or(1);
    let (sort_key, sort_value) = if panel.sort_by_key {
        ("▲", "")
    } else {
        ("", "▼")
    };
    let names: Vec<String> = (0..ncols)
        .map(|i| match (ncols, i) {
            (1, _) => format!("key{sort_key}"),
            (_, 0) => format!("key.0{sort_key}"),
            _ => format!("key.{i}"),
        })
        .collect();
    let mut key_w: Vec<usize> = (0..ncols)
        .map(|i| {
            let widest = rows
                .iter()
                .filter_map(|r| r.cols.get(i))
                .map(|c| c.chars().count())
                .max()
                .unwrap_or(0);
            widest.max(names[i].chars().count()).min(KEY_MAX)
        })
        .collect();
    let value_header = format!("value{sort_value}");
    let value_w = rows
        .iter()
        .map(|r| r.value.chars().count())
        .max()
        .unwrap_or(0)
        .max(value_header.chars().count());
    let fixed: usize = key_w.iter().map(|w| w + 1).sum::<usize>() + value_w + 3;
    let width = usize::from(area.width);
    // Narrow pane: shrink the key columns before giving up the bars.
    if fixed > width {
        let over = fixed - width;
        if let Some(w) = key_w.first_mut() {
            *w = w.saturating_sub(over).max(3);
        }
    }
    let fixed: usize = key_w.iter().map(|w| w + 1).sum::<usize>() + value_w + 3;
    let bar_w = width.saturating_sub(fixed);
    let max = rows.iter().filter_map(|r| r.num).fold(0.0_f64, f64::max);

    let mut header: Vec<Span> = key_w
        .iter()
        .zip(&names)
        .map(|(w, name)| Span::styled(format!("{} ", fit(name, *w)), Theme::label()))
        .collect();
    header.push(Span::styled(format!("{value_header:>value_w$}"), Theme::label()));
    let mut lines = vec![Line::from(header)];

    let room = usize::from(area.height).saturating_sub(1);
    let truncated = rows.len() > room;
    let shown = if truncated {
        room.saturating_sub(1)
    } else {
        rows.len()
    };
    for r in rows.iter().take(shown) {
        let mut spans: Vec<Span> = key_w
            .iter()
            .enumerate()
            .map(|(i, w)| Span::raw(format!("{} ", fit(r.cols.get(i).map_or("", String::as_str), *w))))
            .collect();
        let (mark, mark_style) = delta(r.delta);
        spans.push(Span::raw(format!("{:>value_w$}", r.value)));
        spans.push(Span::styled(format!(" {mark} "), mark_style));
        spans.push(Span::styled(
            r.num.map(|n| bar_f64(n, max, bar_w)).unwrap_or_default(),
            Theme::hist_bar(),
        ));
        lines.push(Line::from(spans));
    }
    if truncated {
        lines.push(Line::styled(
            format!("… {} more rows", rows.len() - shown),
            Theme::muted(),
        ));
    }
    frame.render_widget(Paragraph::new(lines), area);
}
