pub mod theme;

mod browser;
mod editor_view;
mod help;
mod modals;
mod results;
mod run_view;
mod source_view;
mod widgets;

use ratatui::Frame;
use ratatui::layout::{Constraint, Flex, Layout, Rect};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Paragraph};

use crate::app::{App, BpftraceState, Level, Overlay, Screen};
use crate::keymap::Context;
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
        .target()
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

    // Browser on top, results tabs at the bottom: small until there is something to show.
    // The inline editor gets the whole browser area (and the results pane stays small).
    let editing = app.editor.is_some();
    let results_open = !editing && (app.active_run().is_some() || app.screen == Screen::Run);
    if app.full_width && results_open {
        results::draw(frame, main, app);
    } else {
        let bottom = if results_open {
            Constraint::Percentage(58)
        } else {
            Constraint::Length(3)
        };
        let [top, results_area] = Layout::vertical([Constraint::Min(5), bottom]).areas(main);
        if editing {
            browser::draw_detail(frame, top, app);
        } else {
            let [list, detail] =
                Layout::horizontal([Constraint::Percentage(35), Constraint::Percentage(65)]).areas(top);
            browser::draw_list(frame, list, app);
            browser::draw_detail(frame, detail, app);
        }
        results::draw(frame, results_area, app);
    }
    draw_status(frame, status, app);

    match &app.overlay {
        Some(Overlay::Help) => help::draw(frame, area, app),
        Some(Overlay::Params { script_id, form }) => modals::draw_form(frame, main, script_id, form),
        Some(Overlay::Confirm(confirm)) => modals::draw_confirm(frame, main, app, confirm),
        Some(Overlay::Ask(ask)) => modals::draw_ask(frame, main, ask),
        Some(Overlay::Connect(form)) => modals::draw_connect(frame, main, form),
        None => {}
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
            let context = app.context();
            let mut hints: Vec<(&str, &str)> = crate::keymap::hints(context).collect();
            // `d` matters only on a remote tab, so it is not a static hint.
            if matches!(context, Context::Browser | Context::Run) && app.target().is_remote() {
                hints.insert(1.min(hints.len()), ("d", "disconnect"));
            }
            for (keys, text) in hints {
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
    let t = app.target();
    if t.is_remote() {
        spans.push(Span::styled(format!("{} · ", t.label), Theme::status_bar()));
    }
    match &t.bpftrace {
        BpftraceState::Detecting => spans.push(Span::styled("detecting bpftrace…", Theme::status_bar())),
        BpftraceState::Ready { info, .. } => {
            let version = info
                .version
                .map_or_else(|| info.version_raw.clone(), |v| v.to_string());
            spans.push(Span::styled(format!("bpftrace {version}"), Theme::status_bar()));
        }
        BpftraceState::Missing(_) => spans.push(Span::styled("no bpftrace", Theme::badge_error())),
    }
    if let Some(host) = &t.host {
        // Remote kernel releases are long and already in the results tab title.
        let kernel = if t.is_remote() {
            " · ".to_string()
        } else {
            format!(" · {} · ", host.kernel_release)
        };
        spans.push(Span::styled(kernel, Theme::status_bar()));
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
    use crate::app::target::LOCAL;
    use crate::bpftrace::coalesce::one;
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
        // The pane is 18 lines high (results pane below): scrolling stops when the last line is on screen.
        let lines = app.selected().map_or(0, |e| e.script.content.lines().count());
        assert_eq!(usize::from(app.scroll.get()), lines - 18);
        insta::assert_snapshot!(backend);
    }

    #[test]
    fn connect_dialog_checking_80x24() {
        let mut app = ready();
        let attempt = start_connect(&mut app, "ops@10.0.3.14");
        for check in checks() {
            app.update(Msg::ConnectCheck { attempt, check });
        }
        insta::assert_snapshot!(render(&app, 80, 24));
    }

    #[test]
    fn connect_dialog_password_and_failure_120x40() {
        let mut app = ready();
        keys(&mut app, &[KeyCode::Char('c')]);
        type_text(&mut app, "db-02");
        keys(&mut app, &[KeyCode::Tab, KeyCode::Tab, KeyCode::Left]);
        keys(&mut app, &[KeyCode::Tab]);
        type_text(&mut app, "secret");
        let attempt = match press(&mut app, KeyCode::Enter).as_slice() {
            [crate::msg::Cmd::Connect { attempt, .. }] => *attempt,
            other => panic!("{other:?}"),
        };
        let mut checks = checks();
        checks.truncate(3);
        checks.push(crate::remote::connect::Check {
            status: crate::remote::connect::CheckStatus::Fail,
            text: "root: the sudo password was not accepted".into(),
        });
        for check in checks {
            app.update(Msg::ConnectCheck { attempt, check });
        }
        app.update(Msg::ConnectFailed {
            attempt,
            reason: None,
        });
        insta::assert_snapshot!(render(&app, 120, 40));
    }

    #[test]
    fn remote_confirm_80x24() {
        let mut app = ready();
        connect(&mut app, "ops@10.0.3.14");
        keys(&mut app, &[KeyCode::Char('/')]);
        type_text(&mut app, "sysc");
        keys(&mut app, &[KeyCode::Enter, KeyCode::Enter]);
        insta::assert_snapshot!(render(&app, 80, 24));
    }

    #[test]
    fn inline_editor_80x24() {
        let mut app = ready();
        keys(&mut app, &[KeyCode::Char('/')]);
        type_text(&mut app, "sysc");
        keys(&mut app, &[KeyCode::Enter, KeyCode::Char('i')]);
        keys(&mut app, &[KeyCode::Down, KeyCode::Down, KeyCode::End]);
        type_text(&mut app, " (edited)");
        let backend = render(&app, 80, 24);
        insta::assert_snapshot!(backend);
        keys(&mut app, &[KeyCode::Esc]);
        insta::assert_snapshot!("inline_edited_info_80x24", {
            keys(&mut app, &[KeyCode::Char('1')]);
            render(&app, 80, 24)
        });
    }

    #[test]
    fn disconnect_hint_on_remote_tabs() {
        let mut app = ready();
        let bar = |app: &App| {
            let backend = render(app, 120, 40);
            let buf = backend.buffer();
            (0..120)
                .map(|x| buf[(x, 39)].symbol().to_string())
                .collect::<String>()
        };
        assert!(!bar(&app).contains("d disconnect"));
        connect(&mut app, "ops@10.0.3.14");
        app.notice = None;
        assert!(bar(&app).contains(" d disconnect"), "{}", bar(&app));
    }

    #[test]
    fn remote_tab_active_120x40() {
        let mut app = ready();
        connect(&mut app, "ops@10.0.3.14");
        insta::assert_snapshot!(render(&app, 120, 40));
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
            target: LOCAL,
            host: host(Privilege::Root, Lockdown::None),
            bpftrace: Err("cannot run bpftrace: No such file or directory (os error 2)".into()),
        });
        insta::assert_snapshot!("failed_no_bpftrace_80x24", render(&app, 80, 24));
    }

    fn select(app: &mut App, id: &str) {
        keys(app, &[KeyCode::Char('/')]);
        keys(app, &id.chars().map(KeyCode::Char).collect::<Vec<_>>());
        keys(app, &[KeyCode::Esc]);
    }

    /// vfs_latency_demo running for 12 s with the mixed session replayed.
    fn running_app() -> (App, u64, std::time::Instant) {
        use crate::bpftrace::json::parse_line;
        use crate::bpftrace::runner::RunEvent;
        let mut app = ready();
        select(&mut app, "vfs_latency_demo.bt");
        keys(&mut app, &[KeyCode::Enter]);
        let run_id = match app
            .update(Msg::Key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE)))
            .as_slice()
        {
            [crate::msg::Cmd::StartRun { run_id, .. }] => *run_id,
            other => panic!("{other:?}"),
        };
        let t0 = std::time::Instant::now();
        app.update(Msg::RunStarted { run_id, at: t0 });
        let session = std::fs::read_to_string(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/tests/fixtures/json/session_mixed.ndjson"
        ))
        .expect("fixture");
        for msg in session.lines().flat_map(parse_line) {
            app.update(Msg::Run {
                run_id,
                at: t0,
                batch: one(RunEvent::Output(msg)),
            });
        }
        app.update(Msg::Run {
            run_id,
            at: t0,
            batch: one(RunEvent::Stderr(
                "WARNING: could not resolve symbol 0xffffffff81000000".into(),
            )),
        });
        app.update(Msg::Tick(t0 + std::time::Duration::from_secs(12)));
        (app, run_id, t0)
    }

    #[test]
    fn params_form_80x24() {
        let mut app = ready();
        select(&mut app, "params_demo.bt");
        keys(
            &mut app,
            &[
                KeyCode::Enter,
                KeyCode::Char('4'),
                KeyCode::Char('2'),
                KeyCode::Tab,
            ],
        );
        insta::assert_snapshot!(render(&app, 80, 24));
    }

    #[test]
    fn confirm_unsafe_no_priv_120x40() {
        let mut app = ready_app(host(Privilege::None, Lockdown::None));
        select(&mut app, "unsafe_demo.bt");
        keys(&mut app, &[KeyCode::Enter]);
        insta::assert_snapshot!(render(&app, 120, 40));
    }

    #[test]
    fn confirm_partial_80x24() {
        let mut app = ready();
        select(&mut app, "vfs_latency_demo.bt");
        keys(&mut app, &[KeyCode::Enter]);
        insta::assert_snapshot!(render(&app, 80, 24));
    }

    #[test]
    fn run_view_running_120x40() {
        let (app, _, _) = running_app();
        insta::assert_snapshot!(render(&app, 120, 40));
    }

    #[test]
    fn run_view_stopped_80x24() {
        use crate::bpftrace::json::parse_line;
        use crate::bpftrace::runner::{RunEvent, RunExit};
        let (mut app, run_id, t0) = running_app();
        keys(&mut app, &[KeyCode::Char('x')]);
        let dump = r#"{"type": "hist", "data": {"@usecs": [{"min": 16, "max": 31, "count": 421}]}}"#;
        for msg in parse_line(dump) {
            app.update(Msg::Run {
                run_id,
                at: t0,
                batch: one(RunEvent::Output(msg)),
            });
        }
        let exit = RunExit {
            code: Some(0),
            signal: None,
            forced: None,
            error: None,
        };
        app.update(Msg::Run {
            run_id,
            at: t0 + std::time::Duration::from_secs(13),
            batch: one(RunEvent::Exited(exit)),
        });
        insta::assert_snapshot!(render(&app, 80, 24));
    }

    #[test]
    fn run_view_full_width_paused_filtered_120x40() {
        let (mut app, _, _) = running_app();
        render(&app, 120, 40); // the renderer reports what is on screen
        keys(
            &mut app,
            &[KeyCode::Char('z'), KeyCode::Char('p'), KeyCode::Char('/')],
        );
        keys(&mut app, &"map".chars().map(KeyCode::Char).collect::<Vec<_>>());
        insta::assert_snapshot!(render(&app, 120, 40));
    }

    #[test]
    fn ask_stop_for_new_run_80x24() {
        let (mut app, _, _) = running_app();
        keys(&mut app, &[KeyCode::Esc]);
        select(&mut app, "syscount_demo.bt");
        keys(&mut app, &[KeyCode::Enter]);
        insta::assert_snapshot!(render(&app, 80, 24));
    }

    #[test]
    fn tree_dir_selected_80x24() {
        let mut app = ready();
        keys(&mut app, &[KeyCode::Char('j')]);
        insta::assert_snapshot!(render(&app, 80, 24));
    }

    #[test]
    fn tree_collapsed_and_flat_120x40() {
        let mut app = ready();
        keys(&mut app, &[KeyCode::Char('j'), KeyCode::Left]);
        insta::assert_snapshot!("tree_collapsed_120x40", render(&app, 120, 40));
        keys(&mut app, &[KeyCode::Char('t')]);
        insta::assert_snapshot!("flat_list_120x40", render(&app, 120, 40));
    }

    #[test]
    fn too_small() {
        insta::assert_snapshot!(render(&ready(), 79, 24));
    }
}
