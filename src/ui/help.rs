//! `?` help modal, generated from the keymap table (spec §5.6). All contexts, in two
//! balanced columns; scrolls with j/k when it doesn't fit.

use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, BorderType, Clear, Paragraph};

use super::theme::Theme;
use crate::app::App;
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
    lines.push(Line::raw(""));
    lines
}

/// Sections in keymap order, split into two columns of about equal height.
fn columns() -> (Vec<Line<'static>>, Vec<Line<'static>>) {
    let sections: Vec<Vec<Line>> = Context::ALL.into_iter().map(section).collect();
    let total: usize = sections.iter().map(Vec::len).sum();
    let (mut left, mut right) = (Vec::new(), Vec::new());
    for s in sections {
        if left.len() < total / 2 {
            left.extend(s);
        } else {
            right.extend(s);
        }
    }
    (left, right)
}

pub fn draw(frame: &mut Frame, area: Rect, app: &App) {
    let (left, right) = columns();
    let rows = left.len().max(right.len());
    let popup = Rect {
        x: area.x + area.width.saturating_sub(96.min(area.width)) / 2,
        y: area.y + 1.min(area.height),
        width: 96.min(area.width),
        height: area
            .height
            .saturating_sub(2)
            .min(u16::try_from(rows + 2).unwrap_or(u16::MAX)),
    };
    frame.render_widget(Clear, popup);
    let block = Block::bordered()
        .border_type(BorderType::Rounded)
        .border_style(Theme::border_focused())
        .style(Theme::popup_bg())
        .title(Span::styled(" Keys ", Theme::title()));
    let overflow = rows > usize::from(popup.height.saturating_sub(2));
    let block = if overflow {
        block.title(Line::styled(" j/k scroll ", Theme::muted()).right_aligned())
    } else {
        block
    };
    let inner = block.inner(popup);
    frame.render_widget(block, popup);

    let max = u16::try_from(rows.saturating_sub(usize::from(inner.height))).unwrap_or(0);
    let scroll = app.help_scroll.get().min(max);
    app.help_scroll.set(scroll);
    let [l, r] = Layout::horizontal([Constraint::Percentage(50), Constraint::Percentage(50)]).areas(inner);
    frame.render_widget(Paragraph::new(left).scroll((scroll, 0)), l);
    frame.render_widget(Paragraph::new(right).scroll((scroll, 0)), r);
}
