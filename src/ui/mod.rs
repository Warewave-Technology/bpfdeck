pub mod theme;

use ratatui::{
    Frame,
    layout::{Constraint, Layout},
    text::{Line, Span},
    widgets::{Block, BorderType, Paragraph},
};

use crate::app::App;
use theme::Theme;

/// M0 placeholder layout: script list | detail pane, status bar at the bottom.
/// Replace piece by piece following docs/milestones.md.
pub fn draw(frame: &mut Frame, app: &App) {
    frame.render_widget(Block::new().style(Theme::base()), frame.area());

    let [main, status] = Layout::vertical([Constraint::Min(1), Constraint::Length(1)]).areas(frame.area());
    let [list, detail] =
        Layout::horizontal([Constraint::Percentage(35), Constraint::Percentage(65)]).areas(main);

    let list_block = Block::bordered()
        .border_type(BorderType::Rounded)
        .border_style(Theme::border_focused())
        .title(Span::styled(" Scripts ", Theme::title()));
    frame.render_widget(
        Paragraph::new(Line::styled(format!("source: {}", app.source), Theme::muted())).block(list_block),
        list,
    );

    let detail_block = Block::bordered()
        .border_type(BorderType::Rounded)
        .border_style(Theme::border())
        .title(Span::styled(" Detail ", Theme::title()));
    frame.render_widget(
        Paragraph::new(Line::styled("nothing selected", Theme::muted())).block(detail_block),
        detail,
    );

    let hints = Line::from(vec![
        Span::styled(" q ", Theme::key_hint()),
        Span::styled("quit ", Theme::status_bar()),
    ]);
    frame.render_widget(Paragraph::new(hints).style(Theme::status_bar()), status);
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::{Terminal, backend::TestBackend};

    #[test]
    fn m0_layout_80x24() {
        let app = App::new("tests/fixtures/scripts".into());
        let mut terminal = Terminal::new(TestBackend::new(80, 24)).unwrap();
        terminal.draw(|f| draw(f, &app)).unwrap();
        insta::assert_snapshot!(terminal.backend());
    }
}
