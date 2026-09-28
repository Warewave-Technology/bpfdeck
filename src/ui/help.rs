//! `?` help modal, generated from the keymap table (spec §5.6).

use ratatui::Frame;
use ratatui::layout::{Constraint, Flex, Layout, Rect};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, BorderType, Clear, Paragraph};

use super::theme::Theme;
use crate::keymap::{self, Context};

const KEY_WIDTH: usize = 10;

fn section(context: Context) -> Vec<Line<'static>> {
    let mut lines = vec![Line::styled(context.title(), Theme::label())];
    lines.extend(keymap::bindings(context).map(|b| {
        Line::from(vec![
            Span::styled(format!("  {:<KEY_WIDTH$}", b.label), Theme::key_hint()),
            Span::raw(b.help),
        ])
    }));
    lines
}

pub fn draw(frame: &mut Frame, area: Rect) {
    // Two columns: browser keys | filter + help keys. Fits 80×24.
    let left = section(Context::Browser);
    let mut right = section(Context::Filter);
    right.push(Line::raw(""));
    right.extend(section(Context::Help));

    let rows = left.len().max(right.len());
    let height = u16::try_from(rows + 2).unwrap_or(u16::MAX).min(area.height);
    let width = 76.min(area.width);
    let [popup] = Layout::vertical([Constraint::Length(height)])
        .flex(Flex::Center)
        .areas(area);
    let [popup] = Layout::horizontal([Constraint::Length(width)])
        .flex(Flex::Center)
        .areas(popup);

    frame.render_widget(Clear, popup);
    let block = Block::bordered()
        .border_type(BorderType::Rounded)
        .border_style(Theme::border_focused())
        .style(Theme::popup_bg())
        .title(Span::styled(" Keys ", Theme::title()));
    let inner = block.inner(popup);
    frame.render_widget(block, popup);
    let [l, r] = Layout::horizontal([Constraint::Percentage(55), Constraint::Percentage(45)]).areas(inner);
    frame.render_widget(Paragraph::new(left), l);
    frame.render_widget(Paragraph::new(right), r);
}
