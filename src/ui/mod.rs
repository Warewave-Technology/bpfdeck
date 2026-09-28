pub mod theme;

mod browser;
mod help;
mod source_view;

use ratatui::Frame;
use ratatui::layout::{Constraint, Flex, Layout, Rect};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Paragraph};

use crate::app::{App, BpftraceState, Level, Mode};
use crate::sys::{Lockdown, Privilege};
use theme::Theme;

pub const MIN_WIDTH: u16 = 80;
pub const MIN_HEIGHT: u16 = 24;

pub fn draw(frame: &mut Frame, app: &App) {
    let area = frame.area();
    frame.render_widget(Block::new().style(Theme::base()), area);
    if area.width < MIN_WIDTH || area.height < MIN_HEIGHT {
        draw_too_small(frame, area);
        return;
    }

    let lockdown = app
        .host
        .as_ref()
        .map(|h| h.lockdown)
        .filter(|l| l.blocks_bpftrace());
    let [banner, main, status] = Layout::vertical([
        Constraint::Length(u16::from(lockdown.is_some())),
        Constraint::Min(1),
        Constraint::Length(1),
    ])
    .areas(area);
    if let Some(mode) = lockdown {
        draw_lockdown_banner(frame, banner, mode);
    }

    let [list, detail] =
        Layout::horizontal([Constraint::Percentage(35), Constraint::Percentage(65)]).areas(main);
    browser::draw_list(frame, list, app);
    browser::draw_detail(frame, detail, app);
    draw_status(frame, status, app);

    if app.mode == Mode::Help {
        help::draw(frame, area);
    }
}

fn draw_too_small(frame: &mut Frame, area: Rect) {
    let text = format!(
        "terminal too small: {}×{} (need {MIN_WIDTH}×{MIN_HEIGHT})",
        area.width, area.height
    );
    let [row] = Layout::vertical([Constraint::Length(1)])
        .flex(Flex::Center)
        .areas(area);
    frame.render_widget(Paragraph::new(Line::styled(text, Theme::warn()).centered()), row);
}

fn draw_lockdown_banner(frame: &mut Frame, area: Rect, mode: Lockdown) {
    let mode = match mode {
        Lockdown::Confidentiality => "confidentiality",
        _ => "integrity",
    };
    let text = format!("Kernel lockdown ({mode}): bpftrace cannot load BPF programs on this host");
    frame.render_widget(
        Paragraph::new(Line::raw(text).centered()).style(Theme::banner_error()),
        area,
    );
}

fn draw_status(frame: &mut Frame, area: Rect, app: &App) {
    let right = env_spans(app);
    let right_width = u16::try_from(right.iter().map(Span::width).sum::<usize>()).unwrap_or(0);
    let [left_area, right_area] =
        Layout::horizontal([Constraint::Min(1), Constraint::Length(right_width)]).areas(area);

    let left: Vec<Span> = match &app.notice {
        Some(n) => {
            let style = match n.level {
                Level::Info => Theme::notice_info(),
                Level::Warn => Theme::notice_warn(),
                Level::Error => Theme::notice_error(),
            };
            vec![Span::styled(format!(" {}", n.text), style)]
        }
        None => {
            // Whole hints only: skip one that doesn't fit, a shorter later one may.
            let mut room = usize::from(left_area.width);
            let mut spans = Vec::new();
            for (keys, text) in crate::keymap::hints(app.context()) {
                let keys = format!(" {keys} ");
                let width = keys.chars().count() + text.chars().count();
                if width <= room {
                    room -= width;
                    spans.push(Span::styled(keys, Theme::key_hint()));
                    spans.push(Span::styled(text, Theme::status_bar()));
                }
            }
            spans
        }
    };
    frame.render_widget(
        Paragraph::new(Line::from(left)).style(Theme::status_bar()),
        left_area,
    );
    frame.render_widget(
        Paragraph::new(Line::from(right)).style(Theme::status_bar()),
        right_area,
    );
}

/// `bpftrace v0.21.2 · 6.1.0-18-amd64 · root`
fn env_spans(app: &App) -> Vec<Span<'static>> {
    let mut spans = vec![Span::raw("  ")];
    match &app.bpftrace {
        BpftraceState::Detecting => spans.push(Span::styled("detecting bpftrace…", Theme::status_bar())),
        BpftraceState::Ready { info, .. } => {
            let version = info
                .version
                .map_or_else(|| info.version_raw.clone(), |v| v.to_string());
            spans.push(Span::styled(format!("bpftrace {version}"), Theme::status_bar()));
        }
        BpftraceState::Missing(_) => spans.push(Span::styled("no bpftrace", Theme::badge_error())),
    }
    if let Some(host) = &app.host {
        spans.push(Span::styled(
            format!(" · {} · ", host.kernel_release),
            Theme::status_bar(),
        ));
        spans.push(match host.privilege {
            Privilege::Root => Span::styled("root", Theme::badge_ok()),
            Privilege::Caps => Span::styled("caps", Theme::badge_warn()),
            Privilege::None => Span::styled("NO PRIV", Theme::badge_error()),
        });
    }
    spans.push(Span::raw(" "));
    spans
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::fixtures::*;
    use crate::msg::Msg;
    use crate::sys::{Lockdown, Privilege};
    use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
    use ratatui::{Terminal, backend::TestBackend};

    fn render(app: &App, width: u16, height: u16) -> TestBackend {
        let mut terminal = Terminal::new(TestBackend::new(width, height)).expect("terminal");
        terminal.draw(|f| draw(f, app)).expect("draw");
        terminal.backend().clone()
    }

    fn keys(app: &mut App, keys: &[KeyCode]) {
        for &k in keys {
            app.update(Msg::Key(KeyEvent::new(k, KeyModifiers::NONE)));
        }
    }

    fn ready() -> App {
        ready_app(host(Privilege::Root, Lockdown::None))
    }

    #[test]
    fn browser_info_80x24() {
        insta::assert_snapshot!(render(&ready(), 80, 24));
    }

    #[test]
    fn browser_info_120x40() {
        let mut app = ready();
        keys(&mut app, &[KeyCode::Char('G')]); // vfs_latency_demo: partial, probe listing
        insta::assert_snapshot!(render(&app, 120, 40));
    }

    #[test]
    fn params_info_120x40() {
        let mut app = ready();
        keys(
            &mut app,
            &[
                KeyCode::Char('/'),
                KeyCode::Char('p'),
                KeyCode::Char('a'),
                KeyCode::Char('r'),
            ],
        );
        keys(&mut app, &[KeyCode::Enter]);
        insta::assert_snapshot!(render(&app, 120, 40));
    }

    #[test]
    fn source_tab_120x40() {
        let mut app = ready();
        keys(&mut app, &[KeyCode::Char('G'), KeyCode::Char('2')]); // vfs_latency_demo
        insta::assert_snapshot!(render(&app, 120, 40));
    }

    #[test]
    fn source_tab_scroll_is_clamped_80x24() {
        let mut app = ready();
        keys(&mut app, &[KeyCode::Char('G'), KeyCode::Char('2')]);
        for _ in 0..5 {
            keys(&mut app, &[KeyCode::PageDown]);
        }
        let backend = render(&app, 80, 24);
        // The pane is 21 lines high: scrolling stops when the last line is on screen.
        let lines = app.selected().map_or(0, |e| e.script.content.lines().count());
        assert_eq!(usize::from(app.scroll.get()), lines - 21);
        insta::assert_snapshot!(backend);
    }

    #[test]
    fn validation_tab_80x24() {
        let mut app = ready();
        keys(&mut app, &[KeyCode::Char('3')]); // missing_probe_demo: failed
        insta::assert_snapshot!(render(&app, 80, 24));
    }

    #[test]
    fn filter_editing_80x24() {
        let mut app = ready();
        keys(
            &mut app,
            &[
                KeyCode::Char('/'),
                KeyCode::Char('d'),
                KeyCode::Char('e'),
                KeyCode::Char('m'),
            ],
        );
        insta::assert_snapshot!(render(&app, 80, 24));
    }

    #[test]
    fn help_80x24() {
        let mut app = ready();
        keys(&mut app, &[KeyCode::Char('?')]);
        insta::assert_snapshot!(render(&app, 80, 24));
    }

    #[test]
    fn help_120x40() {
        let mut app = ready();
        keys(&mut app, &[KeyCode::Char('?')]);
        insta::assert_snapshot!(render(&app, 120, 40));
    }

    #[test]
    fn lockdown_and_no_privileges_80x24() {
        insta::assert_snapshot!(render(
            &ready_app(host(Privilege::None, Lockdown::Integrity)),
            80,
            24
        ));
    }

    #[test]
    fn loading_and_failed_80x24() {
        let mut app = App::new("https://github.com/bpftrace/bpftrace".into());
        app.init();
        insta::assert_snapshot!("loading_80x24", render(&app, 80, 24));
        app.update(Msg::Loaded(Err(
            "git fetch failed for https://github.com/bpftrace/bpftrace: \
             fatal: unable to access: Could not resolve host: github.com"
                .into(),
        )));
        app.update(Msg::EnvDetected {
            host: host(Privilege::Root, Lockdown::None),
            bpftrace: Err("cannot run bpftrace: No such file or directory (os error 2)".into()),
        });
        insta::assert_snapshot!("failed_no_bpftrace_80x24", render(&app, 80, 24));
    }

    #[test]
    fn too_small() {
        insta::assert_snapshot!(render(&ready(), 79, 24));
    }
}
