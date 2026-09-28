//! `stats` messages (and unknown shapes) as a plain table.

use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;

use super::fit;
use crate::model::panels::{Panel, PanelData, stats_table};
use crate::ui::theme::Theme;

pub fn render(frame: &mut Frame, area: Rect, panel: &Panel) {
    let PanelData::Stats(value) = &panel.data else {
        return;
    };
    let (headers, rows) = stats_table(value);
    let widths: Vec<usize> = (0..headers.len())
        .map(|i| {
            rows.iter()
                .filter_map(|r| r.get(i))
                .chain(std::iter::once(&headers[i]))
                .map(|c| c.chars().count())
                .max()
                .unwrap_or(0)
                .min(40)
        })
        .collect();
    let row = |cells: &[String], style| {
        Line::from(
            cells
                .iter()
                .zip(&widths)
                .enumerate()
                .map(|(i, (c, w))| {
                    // Keys left, numbers right.
                    let cell = if i == 0 && headers[0] == "key" {
                        fit(c, *w)
                    } else {
                        format!("{:>w$}", fit(c, *w).trim_end())
                    };
                    Span::styled(format!("{cell}  "), style)
                })
                .collect::<Vec<_>>(),
        )
    };
    let room = usize::from(area.height).saturating_sub(1);
    let mut lines = vec![row(&headers, Theme::label())];
    lines.extend(rows.iter().take(room).map(|r| row(r, Theme::base())));
    frame.render_widget(Paragraph::new(lines), area);
}
