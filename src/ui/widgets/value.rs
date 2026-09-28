//! Scalar map value (`@x = 5`): the value, large and centered, plus when it last changed.

use ratatui::Frame;
use ratatui::layout::{Constraint, Flex, Layout, Rect};
use ratatui::text::Line;
use ratatui::widgets::{Paragraph, Wrap};

use crate::model::panels::{Panel, PanelData, display};
use crate::ui::run_view::elapsed;
use crate::ui::theme::Theme;

pub fn render(frame: &mut Frame, area: Rect, panel: &Panel) {
    let PanelData::Value { value, changed_at } = &panel.data else {
        return;
    };
    let text = display(value);
    let lines = vec![
        Line::styled(text, Theme::title()).centered(),
        Line::raw(""),
        Line::styled(
            format!("changed at {} · {} updates", elapsed(*changed_at), panel.updates),
            Theme::muted(),
        )
        .centered(),
    ];
    let height = u16::try_from(lines.len()).unwrap_or(3);
    let [middle] = Layout::vertical([Constraint::Length(height)])
        .flex(Flex::Center)
        .areas(area);
    frame.render_widget(Paragraph::new(lines).wrap(Wrap { trim: false }), middle);
}
