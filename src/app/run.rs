//! Run flow (spec §5.2–5.4): params form → confirmation → run view, stop, questions.

use std::cell::Cell;
use std::ffi::OsString;
use std::path::{Path, PathBuf};

use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

use super::{App, BpftraceState, Level, Overlay, Screen, ValidationState};
use crate::bpftrace::command::{self, CommandError, NamedArg, RunArgs};
use crate::bpftrace::runner::{RunEvent, RunExit};
use crate::bpftrace::validate::Verdict;
use crate::keymap::Action;
use crate::model::form::ParamForm;
use crate::model::run_state::{ExitInfo, Run};
use crate::msg::Cmd;

/// Everything the confirmation dialog shows and needs to build the command line.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Confirm {
    pub script_id: String,
    pub path: PathBuf,
    pub probes: Vec<String>,
    pub positional: Vec<String>,
    pub named: Vec<NamedArg>,
    /// Unsafe builtins the script calls (metadata hint).
    pub unsafe_calls: Vec<String>,
    /// bpftrace will refuse without `--unsafe` (metadata hint or validation result).
    pub needs_unsafe: bool,
    /// Explicit per-run opt-in (D-009). Only togglable when `needs_unsafe`.
    pub allow_unsafe: bool,
    pub error: Option<String>,
}

impl Confirm {
    pub fn argv(&self, bpftrace: &Path) -> Result<Vec<OsString>, CommandError> {
        command::run_argv(
            bpftrace,
            &RunArgs {
                script: &self.path,
                positional: &self.positional,
                named: &self.named,
                allow_unsafe: self.allow_unsafe,
            },
        )
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Ask {
    /// Enter on another script while a run is active (D-010: one run at a time).
    StopForNewRun {
        running: String,
    },
    QuitWhileRunning {
        running: String,
    },
}

impl Ask {
    pub fn question(&self) -> String {
        match self {
            Self::StopForNewRun { running } => format!("{running} is still running. Stop it?"),
            Self::QuitWhileRunning { running } => format!("{running} is still running. Stop it and quit?"),
        }
    }
}

/// Log panel state. `visible_top` and `page` are written by the renderer.
#[derive(Debug, Default)]
pub struct LogView {
    /// Stick to the newest line.
    pub follow: bool,
    /// First shown line (by sequence number) while not following.
    pub top_seq: u64,
    pub filter: String,
    pub editing: bool,
    pub visible_top: Cell<Option<u64>>,
    pub page: Cell<usize>,
}

impl LogView {
    pub fn following() -> Self {
        Self {
            follow: true,
            ..Self::default()
        }
    }
}

impl App {
    pub fn bpftrace_path(&self) -> Option<&Path> {
        match &self.bpftrace {
            BpftraceState::Ready { info, .. } => Some(&info.path),
            _ => None,
        }
    }

    /// Enter in the browser.
    pub(super) fn request_run(&mut self) -> Vec<Cmd> {
        let Some(entry) = self.selected() else {
            return Vec::new();
        };
        let id = entry.id().to_string();
        if let Some(run) = self.run.as_ref().filter(|r| r.is_active()) {
            if run.script_id == id {
                self.screen = Screen::Run;
            } else {
                self.overlay = Some(Overlay::Ask(Ask::StopForNewRun {
                    running: run.script_id.clone(),
                }));
            }
            return Vec::new();
        }
        match &self.bpftrace {
            BpftraceState::Missing(reason) => {
                self.notify(Level::Error, format!("cannot run: {reason}"));
                return Vec::new();
            }
            BpftraceState::Detecting => {
                self.notify(Level::Info, "still detecting bpftrace…".into());
                return Vec::new();
            }
            BpftraceState::Ready { .. } => {}
        }
        match ParamForm::new(&entry.script.meta) {
            Some(form) => self.overlay = Some(Overlay::Params { script_id: id, form }),
            None => self.open_confirm(&id, Vec::new(), Vec::new()),
        }
        Vec::new()
    }

    fn open_confirm(&mut self, script_id: &str, positional: Vec<String>, named: Vec<NamedArg>) {
        let Some(entry) = self.entries.iter().find(|e| e.id() == script_id) else {
            return;
        };
        let meta = &entry.script.meta;
        let validation_says_unsafe =
            matches!(&entry.validation, ValidationState::Done(v) if v.verdict == Verdict::NeedsUnsafe);
        self.overlay = Some(Overlay::Confirm(Confirm {
            script_id: script_id.to_string(),
            path: entry.script.file.path.clone(),
            probes: meta.probes.iter().map(|p| p.spec.clone()).collect(),
            positional,
            named,
            unsafe_calls: meta.unsafe_calls.clone(),
            needs_unsafe: meta.needs_unsafe() || validation_says_unsafe,
            allow_unsafe: false,
            error: None,
        }));
    }

    pub(super) fn form_key(&mut self, key: KeyEvent) {
        let Some(Overlay::Params { form, .. }) = &mut self.overlay else {
            return;
        };
        match key.code {
            KeyCode::Char(c)
                if !key
                    .modifiers
                    .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT) =>
            {
                form.input(c)
            }
            KeyCode::Backspace => form.backspace(),
            _ => {}
        }
    }

    pub(super) fn form_action(&mut self, action: Action) -> Vec<Cmd> {
        let bpftrace = self.bpftrace_path().map(Path::to_path_buf);
        let Some(Overlay::Params { script_id, form }) = &mut self.overlay else {
            return Vec::new();
        };
        match action {
            Action::NextField => form.next(),
            Action::PrevField => form.prev(),
            Action::Close => self.overlay = None,
            Action::Submit => {
                let (positional, named) = form.args();
                // Catch values bpftrace cannot take before showing the confirmation.
                let check = RunArgs {
                    script: Path::new("script.bt"),
                    positional: &positional,
                    named: &named,
                    allow_unsafe: false,
                };
                let bpftrace = bpftrace.unwrap_or_else(|| PathBuf::from("bpftrace"));
                if let Err(e) = command::run_argv(&bpftrace, &check) {
                    form.error = Some(e.to_string());
                    return Vec::new();
                }
                let script_id = script_id.clone();
                self.open_confirm(&script_id, positional, named);
            }
            _ => {}
        }
        Vec::new()
    }

    pub(super) fn confirm_action(&mut self, action: Action) -> Vec<Cmd> {
        let Some(Overlay::Confirm(confirm)) = &mut self.overlay else {
            return Vec::new();
        };
        match action {
            Action::ToggleUnsafe if confirm.needs_unsafe => confirm.allow_unsafe = !confirm.allow_unsafe,
            Action::Close => self.overlay = None,
            Action::Submit => return self.start_run(),
            _ => {}
        }
        Vec::new()
    }

    fn start_run(&mut self) -> Vec<Cmd> {
        let Some(bpftrace) = self.bpftrace_path().map(Path::to_path_buf) else {
            return Vec::new();
        };
        let Some(Overlay::Confirm(confirm)) = &mut self.overlay else {
            return Vec::new();
        };
        let argv = match confirm.argv(&bpftrace) {
            Ok(argv) => argv,
            Err(e) => {
                confirm.error = Some(e.to_string());
                return Vec::new();
            }
        };
        self.next_run_id += 1;
        let run_id = self.next_run_id;
        self.run = Some(Run::new(run_id, &confirm.script_id, &command::display(&argv)));
        self.overlay = None;
        self.screen = Screen::Run;
        self.log_view = LogView::following();
        vec![Cmd::StartRun { run_id, argv }]
    }

    pub(super) fn ask_action(&mut self, action: Action) -> Vec<Cmd> {
        let Some(Overlay::Ask(ask)) = self.overlay.take() else {
            return Vec::new();
        };
        if action != Action::Yes {
            return Vec::new();
        }
        let cmds = self.stop_run();
        match ask {
            Ask::StopForNewRun { running } => self.notify(
                Level::Info,
                format!("stopping {running}; press Enter again once it has exited"),
            ),
            Ask::QuitWhileRunning { .. } => {
                self.quit_after_run = true;
                // Already exited meanwhile, or never started.
                if !self.run.as_ref().is_some_and(|r| r.is_active()) {
                    self.should_quit = true;
                }
            }
        }
        cmds
    }

    pub(super) fn quit_or_ask(&mut self) {
        match self.run.as_ref().filter(|r| r.is_active()) {
            Some(run) => {
                self.overlay = Some(Overlay::Ask(Ask::QuitWhileRunning {
                    running: run.script_id.clone(),
                }))
            }
            None => self.should_quit = true,
        }
    }

    fn stop_run(&mut self) -> Vec<Cmd> {
        match self.run.as_mut() {
            Some(run) => {
                if run.stopping() {
                    vec![Cmd::StopRun { run_id: run.id }]
                } else {
                    Vec::new()
                }
            }
            None => Vec::new(),
        }
    }

    pub(super) fn log_filter_key(&mut self, key: KeyEvent) {
        match key.code {
            KeyCode::Char(c)
                if !key
                    .modifiers
                    .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT) =>
            {
                self.log_view.filter.push(c)
            }
            KeyCode::Backspace => {
                self.log_view.filter.pop();
            }
            _ => {}
        }
    }

    pub(super) fn run_action(&mut self, action: Action) -> Vec<Cmd> {
        let page = isize::try_from(self.log_view.page.get().max(1)).unwrap_or(10);
        match action {
            Action::Stop => return self.stop_run(),
            Action::ToggleFollow => {
                if self.log_view.follow {
                    self.log_view.follow = false;
                    self.log_view.top_seq = self.log_view.visible_top.get().unwrap_or(0);
                } else {
                    self.log_view.follow = true;
                }
            }
            Action::OpenFilter => self.log_view.editing = true,
            Action::AcceptFilter => self.log_view.editing = false,
            Action::ClearFilter => {
                self.log_view.editing = false;
                self.log_view.filter.clear();
            }
            Action::Down => self.scroll_log(1),
            Action::Up => self.scroll_log(-1),
            Action::ScrollDown => self.scroll_log(page),
            Action::ScrollUp => self.scroll_log(-page),
            Action::Top => {
                if let Some(first) = self
                    .run
                    .as_ref()
                    .and_then(|r| r.log.matching(&self.log_view.filter).first().map(|l| l.seq))
                {
                    self.log_view.follow = false;
                    self.log_view.top_seq = first;
                }
            }
            Action::Bottom => self.log_view.follow = true,
            Action::ToggleFullWidth => self.full_width = !self.full_width,
            Action::Close => self.screen = Screen::Browser,
            Action::Help => self.overlay = Some(Overlay::Help),
            _ => {}
        }
        Vec::new()
    }

    /// Move the log view by `delta` lines. Scrolling up pauses; reaching the bottom
    /// resumes following.
    fn scroll_log(&mut self, delta: isize) {
        let Some(run) = &self.run else { return };
        let lines = run.log.matching(&self.log_view.filter);
        if lines.is_empty() {
            return;
        }
        let page = self.log_view.page.get().max(1);
        let bottom_top = lines.len().saturating_sub(page);
        let current_seq = if self.log_view.follow {
            self.log_view.visible_top.get()
        } else {
            Some(self.log_view.top_seq)
        };
        let current = current_seq
            .and_then(|seq| lines.iter().position(|l| l.seq >= seq))
            .unwrap_or(bottom_top);
        let target = current.saturating_add_signed(delta).min(lines.len() - 1);
        if delta > 0 && target >= bottom_top {
            self.log_view.follow = true;
        } else {
            self.log_view.follow = false;
            self.log_view.top_seq = lines[target].seq;
        }
    }

    pub(super) fn on_run_started(&mut self, run_id: u64, at: std::time::Instant) {
        if let Some(run) = self.run.as_mut().filter(|r| r.id == run_id) {
            run.started(at);
        }
    }

    pub(super) fn on_run_failed(&mut self, run_id: u64, reason: &str) {
        if let Some(run) = self.run.as_mut().filter(|r| r.id == run_id) {
            run.failed(reason);
        }
        if self.quit_after_run {
            self.should_quit = true;
        }
    }

    pub(super) fn on_run_event(&mut self, run_id: u64, at: std::time::Instant, event: RunEvent) {
        let Some(run) = self.run.as_mut().filter(|r| r.id == run_id) else {
            return;
        };
        match event {
            RunEvent::Output(msg) => run.output(msg),
            RunEvent::Stderr(line) => run.stderr(&line),
            RunEvent::Exited(exit) => {
                run.exited(exit_info(exit), at);
                if self.quit_after_run {
                    self.should_quit = true;
                }
            }
        }
    }
}

fn exit_info(exit: RunExit) -> ExitInfo {
    ExitInfo {
        code: exit.code,
        signal: exit.signal,
        forced: exit.forced.map(|s| s.as_str().to_string()),
        error: exit.error,
    }
}

#[cfg(test)]
mod tests {
    use std::time::{Duration, Instant};

    use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

    use super::super::fixtures::*;
    use super::*;
    use crate::bpftrace::json::OutputMsg;
    use crate::model::run_state::Phase;
    use crate::msg::Msg;
    use crate::sys::{Lockdown, Privilege};
    use pretty_assertions::assert_eq;

    fn key(code: KeyCode) -> Msg {
        Msg::Key(KeyEvent::new(code, KeyModifiers::NONE))
    }

    fn ch(c: char) -> Msg {
        key(KeyCode::Char(c))
    }

    fn ctrl_c() -> Msg {
        Msg::Key(KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL))
    }

    fn typed(app: &mut App, s: &str) {
        for c in s.chars() {
            app.update(ch(c));
        }
    }

    fn app() -> App {
        ready_app(host(Privilege::Root, Lockdown::None))
    }

    /// Select `id` via the filter, then clear the filter (selection is kept).
    fn select(app: &mut App, id: &str) {
        app.update(ch('/'));
        typed(app, id);
        app.update(key(KeyCode::Esc));
        assert_eq!(app.selected().map(|e| e.id()), Some(id));
    }

    fn start_cmd(cmds: &[Cmd]) -> (u64, Vec<String>) {
        match cmds {
            [Cmd::StartRun { run_id, argv }] => (
                *run_id,
                argv.iter().map(|a| a.to_string_lossy().into_owned()).collect(),
            ),
            other => panic!("expected StartRun, got {other:?}"),
        }
    }

    fn exited(code: i32) -> RunEvent {
        RunEvent::Exited(RunExit {
            code: Some(code),
            signal: None,
            forced: None,
            error: None,
        })
    }

    #[test]
    fn script_without_params_goes_straight_to_confirmation() {
        let mut app = app();
        select(&mut app, "syscount_demo.bt");
        assert!(app.update(key(KeyCode::Enter)).is_empty());
        let Some(Overlay::Confirm(confirm)) = &app.overlay else {
            panic!("{:?}", app.overlay)
        };
        assert_eq!(
            confirm.probes,
            vec!["tracepoint:raw_syscalls:sys_enter", "interval:s:1"]
        );
        assert!(!confirm.needs_unsafe);

        let (run_id, argv) = start_cmd(&app.update(key(KeyCode::Enter)));
        assert_eq!(run_id, 1);
        assert_eq!(
            argv,
            vec![
                "/usr/bin/bpftrace",
                "-f",
                "json",
                "-B",
                "line",
                "--",
                "/srv/bpf/syscount_demo.bt"
            ]
        );
        assert_eq!(app.overlay, None);
        assert_eq!(app.screen, Screen::Run);
        let run = app.run.as_ref().expect("run");
        assert_eq!(run.phase, Phase::Starting);
        assert_eq!(
            run.log.matching("").first().map(|l| l.text.as_str()),
            Some("$ /usr/bin/bpftrace -f json -B line -- /srv/bpf/syscount_demo.bt")
        );
    }

    #[test]
    fn params_form_then_confirmation() {
        let mut app = app();
        select(&mut app, "params_demo.bt");
        app.update(key(KeyCode::Enter));
        assert!(matches!(app.overlay, Some(Overlay::Params { .. })));
        typed(&mut app, "1234"); // $1
        app.update(key(KeyCode::Tab));
        app.update(ch(' ')); // --verbose
        app.update(ch('q')); // text/checkbox input, not a command
        assert!(!app.should_quit);
        app.update(key(KeyCode::Enter));
        let Some(Overlay::Confirm(confirm)) = &app.overlay else {
            panic!("{:?}", app.overlay)
        };
        assert_eq!(confirm.positional, vec!["1234"]);
        let (_, argv) = start_cmd(&app.update(key(KeyCode::Enter)));
        assert_eq!(&argv[5..], ["--", "/srv/bpf/params_demo.bt", "1234", "--verbose"]);
    }

    #[test]
    fn invalid_form_values_stay_in_the_form() {
        let mut app = app();
        select(&mut app, "params_demo.bt");
        app.update(key(KeyCode::Enter));
        typed(&mut app, "--unsafe");
        app.update(key(KeyCode::Enter));
        let Some(Overlay::Params { form, .. }) = &app.overlay else {
            panic!("{:?}", app.overlay)
        };
        assert!(
            form.error
                .as_deref()
                .is_some_and(|e| e.contains("cannot start with")),
            "{:?}",
            form.error
        );
        app.update(key(KeyCode::Esc));
        assert_eq!(app.overlay, None);
        assert!(app.run.is_none());
    }

    #[test]
    fn unsafe_is_off_until_toggled() {
        let mut app = app();
        select(&mut app, "unsafe_demo.bt");
        app.update(key(KeyCode::Enter));
        let Some(Overlay::Confirm(confirm)) = &app.overlay else {
            panic!("{:?}", app.overlay)
        };
        assert!(confirm.needs_unsafe && !confirm.allow_unsafe);
        assert_eq!(confirm.unsafe_calls, vec!["system"]);
        app.update(ch('u'));
        app.update(ch('u'));
        app.update(ch('u'));
        let (_, argv) = start_cmd(&app.update(key(KeyCode::Enter)));
        let unsafe_at = argv
            .iter()
            .position(|a| a == "--unsafe")
            .expect("--unsafe after toggle");
        assert!(unsafe_at < argv.iter().position(|a| a == "--").expect("--"));

        // Not offered for scripts that don't need it.
        let mut app = super::super::fixtures::ready_app(host(Privilege::Root, Lockdown::None));
        select(&mut app, "syscount_demo.bt");
        app.update(key(KeyCode::Enter));
        app.update(ch('u'));
        let (_, argv) = start_cmd(&app.update(key(KeyCode::Enter)));
        assert!(!argv.contains(&"--unsafe".to_string()));
    }

    #[test]
    fn run_lifecycle_stop_and_navigation() {
        let mut app = app();
        select(&mut app, "vfs_latency_demo.bt");
        app.update(key(KeyCode::Enter));
        let (run_id, _) = start_cmd(&app.update(key(KeyCode::Enter)));
        let t0 = Instant::now();
        app.update(Msg::RunStarted { run_id, at: t0 });
        app.update(Msg::Run {
            run_id,
            at: t0,
            event: RunEvent::Output(OutputMsg::AttachedProbes(4)),
        });
        app.update(Msg::Tick(t0 + Duration::from_secs(3)));
        // Events of another run are ignored.
        app.update(Msg::Run {
            run_id: 99,
            at: t0,
            event: exited(1),
        });
        let run = app.run.as_ref().expect("run");
        assert_eq!(
            (run.phase, run.attached_probes, run.elapsed),
            (Phase::Running, Some(4), Duration::from_secs(3))
        );

        // Esc leaves the run going; Enter on the same script shows it again; `o` too.
        app.update(key(KeyCode::Esc));
        assert_eq!(app.screen, Screen::Browser);
        app.update(key(KeyCode::Enter));
        assert_eq!(app.screen, Screen::Run);
        assert_eq!(app.overlay, None);

        // Ctrl-C in the run view stops the run, it does not quit bpfdeck.
        assert_eq!(app.update(ctrl_c()), vec![Cmd::StopRun { run_id }]);
        assert!(!app.should_quit);
        assert!(app.update(ch('x')).is_empty(), "already stopping");
        assert_eq!(app.run.as_ref().map(|r| r.phase), Some(Phase::Stopping));
        app.update(Msg::Run {
            run_id,
            at: t0 + Duration::from_secs(4),
            event: exited(0),
        });
        let run = app.run.as_ref().expect("run");
        assert!(run.succeeded());
        assert_eq!(run.elapsed, Duration::from_secs(4));

        app.update(key(KeyCode::Esc));
        app.update(ch('o'));
        assert_eq!(app.screen, Screen::Run, "finished output stays viewable");
        app.update(key(KeyCode::Esc));
        // A new Enter starts a new run.
        app.update(key(KeyCode::Enter));
        assert!(matches!(app.overlay, Some(Overlay::Confirm(_))));
    }

    #[test]
    fn one_run_at_a_time() {
        let mut app = app();
        select(&mut app, "syscount_demo.bt");
        app.update(key(KeyCode::Enter));
        let (run_id, _) = start_cmd(&app.update(key(KeyCode::Enter)));
        app.update(Msg::RunStarted {
            run_id,
            at: Instant::now(),
        });
        app.update(key(KeyCode::Esc));
        select(&mut app, "no_header.bt");
        app.update(key(KeyCode::Enter));
        assert!(matches!(
            app.overlay,
            Some(Overlay::Ask(Ask::StopForNewRun { .. }))
        ));
        assert!(app.update(ch('n')).is_empty());
        assert_eq!(app.overlay, None);
        app.update(key(KeyCode::Enter));
        assert_eq!(app.update(ch('y')), vec![Cmd::StopRun { run_id }]);
        assert!(app.notice.is_some());
    }

    #[test]
    fn quitting_with_an_active_run_stops_it_first() {
        let mut app = app();
        select(&mut app, "syscount_demo.bt");
        app.update(key(KeyCode::Enter));
        let (run_id, _) = start_cmd(&app.update(key(KeyCode::Enter)));
        app.update(Msg::RunStarted {
            run_id,
            at: Instant::now(),
        });
        app.update(key(KeyCode::Esc));
        app.update(ch('q'));
        assert!(matches!(
            app.overlay,
            Some(Overlay::Ask(Ask::QuitWhileRunning { .. }))
        ));
        assert_eq!(app.update(ch('y')), vec![Cmd::StopRun { run_id }]);
        assert!(!app.should_quit, "waits for the exit-time dump");
        app.update(Msg::Run {
            run_id,
            at: Instant::now(),
            event: exited(0),
        });
        assert!(app.should_quit);
    }

    #[test]
    fn spawn_failure_and_missing_bpftrace() {
        let mut app = app();
        select(&mut app, "syscount_demo.bt");
        app.update(key(KeyCode::Enter));
        let (run_id, _) = start_cmd(&app.update(key(KeyCode::Enter)));
        app.update(Msg::RunFailed {
            run_id,
            reason: "cannot run /usr/bin/bpftrace: No such file".into(),
        });
        let run = app.run.as_ref().expect("run");
        assert_eq!(run.phase, Phase::Failed);
        assert!(!run.is_active());

        let mut app = App::new("x".into());
        app.init();
        app.update(Msg::Loaded(Ok(catalog())));
        app.update(Msg::EnvDetected {
            host: host(Privilege::Root, Lockdown::None),
            bpftrace: Err("bpftrace not found".into()),
        });
        app.update(key(KeyCode::Enter));
        assert_eq!(app.overlay, None);
        assert_eq!(app.notice.as_ref().map(|n| n.level), Some(Level::Error));
    }

    #[test]
    fn log_follow_pause_scroll_and_filter() {
        let mut app = app();
        select(&mut app, "syscount_demo.bt");
        app.update(key(KeyCode::Enter));
        let (run_id, _) = start_cmd(&app.update(key(KeyCode::Enter)));
        for i in 0..50 {
            app.update(Msg::Run {
                run_id,
                at: Instant::now(),
                event: RunEvent::Stderr(format!("line {i}")),
            });
        }
        // What the renderer would report for a 10-line pane following the tail.
        app.log_view.page.set(10);
        app.log_view.visible_top.set(Some(41));
        assert!(app.log_view.follow);

        app.update(ch('k'));
        assert!(!app.log_view.follow, "scrolling up pauses");
        assert_eq!(app.log_view.top_seq, 40);
        app.update(key(KeyCode::PageUp));
        assert_eq!(app.log_view.top_seq, 30);
        app.update(ch('g'));
        assert_eq!(app.log_view.top_seq, 0);
        app.update(key(KeyCode::PageDown));
        assert_eq!(app.log_view.top_seq, 10);
        app.update(ch('G'));
        assert!(app.log_view.follow);

        app.update(ch('p'));
        assert!(!app.log_view.follow);
        assert_eq!(app.log_view.top_seq, 41, "pause keeps what is on screen");
        app.update(ch('p'));
        assert!(app.log_view.follow);

        app.update(ch('/'));
        assert_eq!(app.context(), crate::keymap::Context::LogFilter);
        typed(&mut app, "line 4");
        app.update(key(KeyCode::Enter));
        assert_eq!(app.log_view.filter, "line 4");
        assert_eq!(
            app.run
                .as_ref()
                .map(|r| r.log.matching(&app.log_view.filter).len()),
            Some(11)
        );
        app.update(ch('/'));
        app.update(key(KeyCode::Esc));
        assert_eq!(app.log_view.filter, "");

        app.update(ch('z'));
        assert!(app.full_width);
    }
}
