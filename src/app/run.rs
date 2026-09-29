//! Run flow (spec §5.2–5.4): params form → confirmation → run view, stop, questions.

use std::cell::Cell;
use std::ffi::OsString;
use std::path::{Path, PathBuf};

use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

use super::target::{Conn, TargetId};
use super::{App, BpftraceState, Level, Overlay, Screen, ValidationState};
use crate::bpftrace::coalesce::Batch;
use crate::bpftrace::command::{self, CommandError, NamedArg, RunArgs};
use crate::bpftrace::runner::{RunEvent, RunExit};
use crate::bpftrace::validate::Verdict;
use crate::keymap::Action;
use crate::model::export;
use crate::model::form::ParamForm;
use crate::model::log::LogKind;
use crate::model::run_state::{ExitInfo, Run};
use crate::msg::Cmd;
use crate::remote::REMOTE_SCRIPT;

/// Everything the confirmation dialog shows and needs to build the command line.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Confirm {
    pub script_id: String,
    /// The file bpftrace gets: the script, or the private copy of its edited version.
    pub path: PathBuf,
    /// Runs the inline-edited version (D-025).
    pub edited: bool,
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
    /// Target checklist (F3); empty when only one target can run scripts.
    pub targets: Vec<TargetChoice>,
    /// Row of `targets` under the cursor.
    pub cursor: usize,
}

/// One row of the confirmation's target checklist.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TargetChoice {
    pub id: TargetId,
    pub label: String,
    pub checked: bool,
    /// Why it cannot run this now (busy, no bpftrace, connection lost).
    pub unavailable: Option<String>,
    /// The script's validation there.
    pub validation: ValidationState,
}

impl TargetChoice {
    /// Validated as runnable there (● or `!`): what `a` checks.
    fn validated(&self) -> bool {
        matches!(&self.validation, ValidationState::Done(v) if matches!(v.verdict, Verdict::Ok | Verdict::NeedsUnsafe))
    }
}

/// A script started on several targets at once (docs/design-fleet.md).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FleetRun {
    pub script_id: String,
    /// `(target, run id)` of each member.
    pub members: Vec<(TargetId, u64)>,
}

impl Confirm {
    /// The command line for `target`: a remote run refers to the copied `script.bt` in the
    /// runner's temp dir (docs/design-remote.md); a local one to the file itself.
    pub fn argv(&self, bpftrace: &Path, remote: bool) -> Result<Vec<OsString>, CommandError> {
        let script = if remote {
            Path::new(REMOTE_SCRIPT)
        } else {
            self.path.as_path()
        };
        command::run_argv(
            bpftrace,
            &RunArgs {
                script,
                positional: &self.positional,
                named: &self.named,
                allow_unsafe: self.allow_unsafe,
            },
        )
    }
}

/// Name of the script inside the remote runner's temp dir.

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Ask {
    /// Enter on another script while a run is active there (one run per target, D-010).
    StopForNewRun {
        running: String,
    },
    QuitWhileRunning {
        running: String,
    },
    DisconnectWhileRunning {
        target: TargetId,
        running: String,
    },
}

impl Ask {
    pub fn question(&self) -> String {
        match self {
            Self::StopForNewRun { running } => format!("{running} is still running. Stop it?"),
            Self::QuitWhileRunning { running } => format!("{running} still running. Stop and quit?"),
            Self::DisconnectWhileRunning { running, .. } => {
                format!("{running} is still running. Stop it and disconnect?")
            }
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
    /// bpftrace on the active target.
    pub fn bpftrace_path(&self) -> Option<&Path> {
        match &self.target().bpftrace {
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
        let target = self.target();
        if let Some(run) = target.active_run() {
            if run.script_id == id {
                self.screen = Screen::Run;
            } else {
                self.overlay = Some(Overlay::Ask(Ask::StopForNewRun {
                    running: format!("{} on {}", run.script_id, target.label),
                }));
            }
            return Vec::new();
        }
        if let Conn::Lost(reason) = &target.conn {
            let text = format!(
                "{}: connection lost ({reason}); press c to reconnect",
                target.label
            );
            self.notify(Level::Error, text);
            return Vec::new();
        }
        match &target.bpftrace {
            BpftraceState::Missing(reason) => {
                let text = format!("cannot run on {}: {reason}", target.label);
                self.notify(Level::Error, text);
                return Vec::new();
            }
            BpftraceState::Detecting => {
                self.notify(Level::Info, "still detecting bpftrace…".into());
                return Vec::new();
            }
            BpftraceState::Ready { .. } => {}
        }
        let Some(entry) = self.selected() else {
            return Vec::new();
        };
        if entry.run_path().is_none() {
            self.notify(
                Level::Info,
                "saving your edits; press Enter again in a moment".into(),
            );
            return Vec::new();
        }
        match ParamForm::new(&entry.shown().meta) {
            Some(form) => self.overlay = Some(Overlay::Params { script_id: id, form }),
            None => self.open_confirm(&id, Vec::new(), Vec::new()),
        }
        Vec::new()
    }

    /// The checklist rows: every target, checkable where it can run now. Empty when fewer
    /// than two targets can (then the confirmation is the single-target one).
    fn target_choices(&self, entry: &super::Entry) -> Vec<TargetChoice> {
        let active = self.target().id;
        let choices: Vec<TargetChoice> = self
            .targets
            .iter()
            .map(|t| {
                let unavailable = if t.lost() {
                    Some("connection lost".to_string())
                } else if let Some(run) = t.active_run() {
                    Some(format!("busy: {} is running", run.script_id))
                } else {
                    match &t.bpftrace {
                        BpftraceState::Ready { .. } => None,
                        BpftraceState::Missing(_) => Some("no bpftrace".to_string()),
                        BpftraceState::Detecting => Some("detecting bpftrace".to_string()),
                    }
                };
                TargetChoice {
                    id: t.id,
                    label: t.label.clone(),
                    checked: t.id == active && unavailable.is_none(),
                    unavailable,
                    validation: entry.validation(t.id).clone(),
                }
            })
            .collect();
        if choices.iter().filter(|c| c.unavailable.is_none()).count() < 2 {
            return Vec::new();
        }
        choices
    }

    fn open_confirm(&mut self, script_id: &str, positional: Vec<String>, named: Vec<NamedArg>) {
        let target = self.target().id;
        let Some(entry) = self.entries.iter().find(|e| e.id() == script_id) else {
            return;
        };
        let targets = self.target_choices(entry);
        let cursor = targets.iter().position(|c| c.checked).unwrap_or(0);
        let (meta, Some(path)) = (&entry.shown().meta, entry.run_path()) else {
            return;
        };
        let validation_says_unsafe =
            matches!(entry.validation(target), ValidationState::Done(v) if v.verdict == Verdict::NeedsUnsafe);
        self.overlay = Some(Overlay::Confirm(Confirm {
            script_id: script_id.to_string(),
            path: path.to_path_buf(),
            edited: entry.edited(),
            probes: meta.probes.iter().map(|p| p.spec.clone()).collect(),
            positional,
            named,
            unsafe_calls: meta.unsafe_calls.clone(),
            needs_unsafe: meta.needs_unsafe() || validation_says_unsafe,
            allow_unsafe: false,
            error: None,
            targets,
            cursor,
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
        let remote = self.target().is_remote();
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
                    script: Path::new(REMOTE_SCRIPT),
                    positional: &positional,
                    named: &named,
                    allow_unsafe: false,
                };
                let bpftrace = bpftrace.unwrap_or_else(|| PathBuf::from("bpftrace"));
                if let Err(e) = command::run_argv(&bpftrace, &check) {
                    form.error = Some(e.to_string());
                    return Vec::new();
                }
                // The remote runner frames arguments one per line.
                if remote && positional.iter().any(|v| v.contains('\n')) {
                    form.error = Some("values sent to a remote host cannot contain line breaks".into());
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
            Action::Down if !confirm.targets.is_empty() => {
                confirm.cursor = (confirm.cursor + 1).min(confirm.targets.len() - 1)
            }
            Action::Up => confirm.cursor = confirm.cursor.saturating_sub(1),
            Action::ToggleTarget => {
                if let Some(c) = confirm.targets.get_mut(confirm.cursor) {
                    if let Some(why) = &c.unavailable {
                        confirm.error = Some(format!("{}: {why}", c.label));
                    } else {
                        c.checked = !c.checked;
                        confirm.error = None;
                    }
                }
            }
            Action::SelectValidated => {
                for c in &mut confirm.targets {
                    c.checked = c.unavailable.is_none() && c.validated();
                }
                confirm.error = None;
            }
            Action::Submit => return self.start_run(),
            _ => {}
        }
        Vec::new()
    }

    /// Enter in the confirmation: start the run on the checked targets (or the active one).
    fn start_run(&mut self) -> Vec<Cmd> {
        let Some(Overlay::Confirm(confirm)) = &self.overlay else {
            return Vec::new();
        };
        let confirm = confirm.clone();
        let chosen: Vec<TargetId> = if confirm.targets.is_empty() {
            vec![self.target().id]
        } else {
            confirm
                .targets
                .iter()
                .filter(|c| c.checked)
                .map(|c| c.id)
                .collect()
        };
        // Build every command line first: all targets start, or none does.
        let mut plans = Vec::new();
        let mut problem = None;
        for &id in &chosen {
            let Some(t) = self.targets.iter().find(|t| t.id == id) else {
                continue;
            };
            let BpftraceState::Ready { info, .. } = &t.bpftrace else {
                problem = Some(format!("{}: bpftrace is not ready", t.label));
                break;
            };
            if t.is_remote() && confirm.positional.iter().any(|v| v.contains('\n')) {
                problem = Some(format!(
                    "{}: values sent to a remote host cannot contain line breaks",
                    t.label
                ));
                break;
            }
            match confirm.argv(&info.path, t.is_remote()) {
                Ok(argv) => plans.push((id, argv)),
                Err(e) => {
                    problem = Some(e.to_string());
                    break;
                }
            }
        }
        if plans.is_empty() && problem.is_none() {
            problem = Some("check at least one target (Space)".into());
        }
        if let Some(problem) = problem {
            if let Some(Overlay::Confirm(c)) = &mut self.overlay {
                c.error = Some(problem);
            }
            return Vec::new();
        }
        let edits = confirm
            .edited
            .then(|| self.entries.iter().find(|e| e.id() == confirm.script_id))
            .flatten()
            .and_then(|e| e.draft.as_ref().map(|d| d.diff.clone()));
        let fleet = (plans.len() > 1).then_some(plans.len());
        let mut cmds = Vec::new();
        let mut members = Vec::new();
        for (target, argv) in plans {
            self.next_run_id += 1;
            let run_id = self.next_run_id;
            let mut run = Run::new(run_id, &confirm.script_id, &command::display(&argv));
            if let Some(diff) = &edits {
                run.log.push_line(
                    LogKind::System,
                    &format!("✎ edited script ({} lines vs the source file)", diff.summary()),
                );
            }
            run.edits = edits.clone();
            run.fleet = fleet;
            if let Some(t) = self.target_by_id(target) {
                t.run = Some(run);
                t.log_view = LogView::following();
            }
            members.push((target, run_id));
            cmds.push(Cmd::StartRun {
                target,
                run_id,
                argv,
                script: confirm.path.clone(),
            });
        }
        // Stay on the selected tab if it runs the script, else show the first that does.
        if !members.iter().any(|(t, _)| *t == self.target().id)
            && let Some(i) = self.targets.iter().position(|t| t.id == members[0].0)
        {
            self.active = i;
        }
        if fleet.is_some() {
            self.fleet = Some(FleetRun {
                script_id: confirm.script_id.clone(),
                members,
            });
        }
        self.overlay = None;
        self.screen = Screen::Run;
        cmds
    }

    /// `X`: stop the selected tab's run on every target of its fleet run.
    fn stop_fleet(&mut self) -> Vec<Cmd> {
        let t = self.target();
        let run_id = t.run.as_ref().map(|r| r.id);
        let members = match (&self.fleet, run_id) {
            (Some(f), Some(run_id)) if f.members.contains(&(t.id, run_id)) => f.members.clone(),
            _ => return self.stop_run(self.active),
        };
        let mut cmds = Vec::new();
        for (target, run_id) in members {
            if let Some(i) = self
                .targets
                .iter()
                .position(|t| t.id == target && t.run.as_ref().is_some_and(|r| r.id == run_id))
            {
                cmds.extend(self.stop_run(i));
            }
        }
        self.notify(Level::Info, format!("stopping on {} targets", cmds.len()));
        cmds
    }

    pub(super) fn ask_action(&mut self, action: Action) -> Vec<Cmd> {
        let Some(Overlay::Ask(ask)) = self.overlay.take() else {
            return Vec::new();
        };
        if action != Action::Yes {
            return Vec::new();
        }
        match ask {
            Ask::StopForNewRun { running } => {
                let cmds = self.stop_run(self.active);
                self.notify(
                    Level::Info,
                    format!("stopping {running}; press Enter again once it has exited"),
                );
                cmds
            }
            Ask::DisconnectWhileRunning { target, .. } => self.disconnect(target),
            Ask::QuitWhileRunning { .. } => {
                self.quit_after_run = true;
                let cmds: Vec<Cmd> = (0..self.targets.len()).flat_map(|i| self.stop_run(i)).collect();
                self.quit_if_idle();
                cmds
            }
        }
    }

    pub(super) fn quit_or_ask(&mut self) {
        let running: Vec<String> = self
            .targets
            .iter()
            .filter_map(|t| t.active_run().map(|r| format!("{} on {}", r.script_id, t.label)))
            .collect();
        if running.is_empty() {
            self.should_quit = true;
        } else {
            self.overlay = Some(Overlay::Ask(Ask::QuitWhileRunning {
                running: format!(
                    "{} {}",
                    running.join(", "),
                    if running.len() == 1 { "is" } else { "are" }
                ),
            }));
        }
    }

    /// After "stop and quit": quit once no target has an active run.
    fn quit_if_idle(&mut self) {
        if self.quit_after_run && self.targets.iter().all(|t| t.active_run().is_none()) {
            self.should_quit = true;
        }
    }

    /// Stop the run of the target at index `i`.
    fn stop_run(&mut self, i: usize) -> Vec<Cmd> {
        let Some(t) = self.targets.get_mut(i) else {
            return Vec::new();
        };
        let target = t.id;
        match t.run.as_mut() {
            Some(run) => {
                if run.stopping() {
                    vec![Cmd::StopRun {
                        target,
                        run_id: run.id,
                    }]
                } else {
                    Vec::new()
                }
            }
            None => Vec::new(),
        }
    }

    pub(super) fn log_filter_key(&mut self, key: KeyEvent) {
        let view = &mut self.target_mut().log_view;
        match key.code {
            KeyCode::Char(c)
                if !key
                    .modifiers
                    .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT) =>
            {
                view.filter.push(c)
            }
            KeyCode::Backspace => {
                view.filter.pop();
            }
            _ => {}
        }
    }

    pub(super) fn run_action(&mut self, action: Action) -> Vec<Cmd> {
        let page = isize::try_from(self.target().log_view.page.get().max(1)).unwrap_or(10);
        match action {
            Action::Stop => return self.stop_run(self.active),
            Action::StopAll => return self.stop_fleet(),
            Action::ToggleFollow => {
                let view = &mut self.target_mut().log_view;
                if view.follow {
                    view.follow = false;
                    view.top_seq = view.visible_top.get().unwrap_or(0);
                } else {
                    view.follow = true;
                }
            }
            Action::OpenFilter => self.target_mut().log_view.editing = true,
            Action::AcceptFilter => self.target_mut().log_view.editing = false,
            Action::ClearFilter => {
                let view = &mut self.target_mut().log_view;
                view.editing = false;
                view.filter.clear();
            }
            Action::Down => self.scroll_log(1),
            Action::Up => self.scroll_log(-1),
            Action::ScrollDown => self.scroll_log(page),
            Action::ScrollUp => self.scroll_log(-page),
            Action::Top => {
                let t = self.target_mut();
                if let Some(first) = t
                    .run
                    .as_ref()
                    .and_then(|r| r.log.matching(&t.log_view.filter).first().map(|l| l.seq))
                {
                    t.log_view.follow = false;
                    t.log_view.top_seq = first;
                }
            }
            Action::Bottom => self.target_mut().log_view.follow = true,
            Action::ToggleFullWidth => self.full_width = !self.full_width,
            Action::Export => {
                let t = self.target();
                if let Some(run) = &t.run {
                    let cmd = Cmd::ExportRun {
                        run_id: run.id,
                        script_id: run.script_id.clone(),
                        host: t.is_remote().then(|| t.label.clone()),
                        text: export::render_text(run, t.is_remote().then_some(t.label.as_str()), None),
                    };
                    self.notify(Level::Info, "exporting…".into());
                    return vec![cmd];
                }
            }
            Action::NextTab | Action::PrevTab => {
                let delta = if action == Action::NextTab { 1 } else { -1 };
                if let Some(run) = &mut self.target_mut().run {
                    run.panels.cycle_focus(delta);
                }
            }
            Action::PrevKey | Action::NextKey => {
                let delta = if action == Action::NextKey { 1 } else { -1 };
                if let Some(panel) = self
                    .target_mut()
                    .run
                    .as_mut()
                    .and_then(|r| r.panels.focused_mut())
                {
                    panel.cycle_key(delta);
                }
            }
            Action::ToggleSort => {
                if let Some(panel) = self
                    .target_mut()
                    .run
                    .as_mut()
                    .and_then(|r| r.panels.focused_mut())
                {
                    panel.sort_by_key = !panel.sort_by_key;
                }
            }
            Action::PrevTarget => self.switch_target(-1),
            Action::NextTarget => self.switch_target(1),
            Action::Connect => self.open_connect(),
            Action::Disconnect => return self.request_disconnect(),
            Action::Close => self.screen = Screen::Browser,
            Action::Help => self.overlay = Some(Overlay::Help),
            _ => {}
        }
        Vec::new()
    }

    /// Move the log view by `delta` lines. Scrolling up pauses; reaching the bottom
    /// resumes following.
    fn scroll_log(&mut self, delta: isize) {
        let t = self.target_mut();
        let Some(run) = &t.run else { return };
        let view = &mut t.log_view;
        let lines = run.log.matching(&view.filter);
        if lines.is_empty() {
            return;
        }
        let page = view.page.get().max(1);
        let bottom_top = lines.len().saturating_sub(page);
        let current_seq = if view.follow {
            view.visible_top.get()
        } else {
            Some(view.top_seq)
        };
        let current = current_seq
            .and_then(|seq| lines.iter().position(|l| l.seq >= seq))
            .unwrap_or(bottom_top);
        let target = current.saturating_add_signed(delta).min(lines.len() - 1);
        if delta > 0 && target >= bottom_top {
            view.follow = true;
        } else {
            view.follow = false;
            view.top_seq = lines[target].seq;
        }
    }

    pub(super) fn on_run_started(&mut self, run_id: u64, at: std::time::Instant) {
        if let Some(run) = self.target_by_run(run_id).and_then(|t| t.run.as_mut()) {
            run.started(at);
        }
    }

    pub(super) fn on_run_failed(&mut self, run_id: u64, reason: &str) {
        if let Some(run) = self.target_by_run(run_id).and_then(|t| t.run.as_mut()) {
            run.failed(reason);
        }
        self.quit_if_idle();
    }

    pub(super) fn on_run_batch(&mut self, run_id: u64, at: std::time::Instant, batch: Batch) {
        let Some(target) = self.target_by_run(run_id) else {
            return;
        };
        let remote = target.is_remote();
        let Some(run) = target.run.as_mut() else {
            return;
        };
        run.dropped += batch.dropped;
        // Ticks are skipped while the channel is full; batches keep the clock moving.
        run.tick(at);
        for event in batch.events {
            match event {
                RunEvent::Output(msg) => run.output(msg, at),
                RunEvent::Stderr(line) => run.stderr(&line),
                RunEvent::Exited(exit) => {
                    // ssh's own failure status: the session, not bpftrace, ended.
                    if remote && exit.code == Some(SSH_FAILED) {
                        run.log.push_line(
                            LogKind::System,
                            "ssh session ended (exit 255): the connection dropped or ssh failed; \
                             the host stops bpftrace when its session closes",
                        );
                    }
                    run.exited(exit_info(exit), at)
                }
            }
        }
        self.quit_if_idle();
    }
}

/// ssh exits with 255 when it fails itself (connection lost, auth, …).
const SSH_FAILED: i32 = 255;

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
    use crate::app::target::LOCAL;
    use std::time::{Duration, Instant};

    use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

    use super::super::fixtures::*;
    use super::*;
    use crate::bpftrace::coalesce::one;
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
            [Cmd::StartRun { run_id, argv, .. }] => (
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
        let run = app.target().run.as_ref().expect("run");
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
        assert!(app.target().run.is_none());
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
            batch: one(RunEvent::Output(OutputMsg::AttachedProbes(4))),
        });
        app.update(Msg::Tick(t0 + Duration::from_secs(3)));
        // Events of another run are ignored.
        app.update(Msg::Run {
            run_id: 99,
            at: t0,
            batch: one(exited(1)),
        });
        let run = app.target().run.as_ref().expect("run");
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
        assert_eq!(
            app.update(ctrl_c()),
            vec![Cmd::StopRun {
                target: LOCAL,
                run_id
            }]
        );
        assert!(!app.should_quit);
        assert!(app.update(ch('x')).is_empty(), "already stopping");
        assert_eq!(app.target().run.as_ref().map(|r| r.phase), Some(Phase::Stopping));
        app.update(Msg::Run {
            run_id,
            at: t0 + Duration::from_secs(4),
            batch: one(exited(0)),
        });
        let run = app.target().run.as_ref().expect("run");
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
        assert_eq!(
            app.update(ch('y')),
            vec![Cmd::StopRun {
                target: LOCAL,
                run_id
            }]
        );
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
        assert_eq!(
            app.update(ch('y')),
            vec![Cmd::StopRun {
                target: LOCAL,
                run_id
            }]
        );
        assert!(!app.should_quit, "waits for the exit-time dump");
        app.update(Msg::Run {
            run_id,
            at: Instant::now(),
            batch: one(exited(0)),
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
        let run = app.target().run.as_ref().expect("run");
        assert_eq!(run.phase, Phase::Failed);
        assert!(!run.is_active());

        let mut app = App::new("x".into());
        app.init();
        app.update(Msg::Loaded(Ok(catalog())));
        app.update(Msg::EnvDetected {
            target: LOCAL,
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
                batch: one(RunEvent::Stderr(format!("line {i}"))),
            });
        }
        // What the renderer would report for a 10-line pane following the tail.
        app.target().log_view.page.set(10);
        app.target().log_view.visible_top.set(Some(41));
        assert!(app.target().log_view.follow);

        app.update(ch('k'));
        assert!(!app.target().log_view.follow, "scrolling up pauses");
        assert_eq!(app.target().log_view.top_seq, 40);
        app.update(key(KeyCode::PageUp));
        assert_eq!(app.target().log_view.top_seq, 30);
        app.update(ch('g'));
        assert_eq!(app.target().log_view.top_seq, 0);
        app.update(key(KeyCode::PageDown));
        assert_eq!(app.target().log_view.top_seq, 10);
        app.update(ch('G'));
        assert!(app.target().log_view.follow);

        app.update(ch('p'));
        assert!(!app.target().log_view.follow);
        assert_eq!(app.target().log_view.top_seq, 41, "pause keeps what is on screen");
        app.update(ch('p'));
        assert!(app.target().log_view.follow);

        app.update(ch('/'));
        assert_eq!(app.context(), crate::keymap::Context::LogFilter);
        typed(&mut app, "line 4");
        app.update(key(KeyCode::Enter));
        assert_eq!(app.target().log_view.filter, "line 4");
        assert_eq!(
            app.target()
                .run
                .as_ref()
                .map(|r| r.log.matching(&app.target().log_view.filter).len()),
            Some(11)
        );
        app.update(ch('/'));
        app.update(key(KeyCode::Esc));
        assert_eq!(app.target().log_view.filter, "");

        app.update(ch('z'));
        assert!(app.full_width);
    }

    #[test]
    fn panel_keys() {
        let mut app = app();
        select(&mut app, "vfs_latency_demo.bt");
        app.update(key(KeyCode::Enter));
        let (run_id, _) = start_cmd(&app.update(key(KeyCode::Enter)));
        let t0 = Instant::now();
        app.update(Msg::RunStarted { run_id, at: t0 });
        let lines = [
            r#"{"type":"map","data":{"@m":{"a":1,"b":2}}}"#,
            r#"{"type":"hist","data":{"@h":{"x":[{"min":0,"max":0,"count":1}],"y":[{"min":0,"max":0,"count":2}]}}}"#,
        ];
        for line in lines {
            for msg in crate::bpftrace::json::parse_line(line) {
                app.update(Msg::Run {
                    run_id,
                    at: t0,
                    batch: one(RunEvent::Output(msg)),
                });
            }
        }
        let focused = |app: &App| {
            app.target()
                .run
                .as_ref()
                .and_then(|r| r.panels.focused())
                .map(|p| p.name.clone())
        };
        assert_eq!(focused(&app).as_deref(), Some("@m"));
        app.update(ch('s'));
        assert!(
            app.target()
                .run
                .as_ref()
                .is_some_and(|r| r.panels.list[0].sort_by_key)
        );
        app.update(key(KeyCode::Tab));
        assert_eq!(focused(&app).as_deref(), Some("@h"));
        app.update(ch(']'));
        assert_eq!(
            app.target()
                .run
                .as_ref()
                .and_then(|r| r.panels.list[1].key.clone())
                .as_deref(),
            Some("y")
        );
        app.update(ch('['));
        assert_eq!(
            app.target()
                .run
                .as_ref()
                .and_then(|r| r.panels.list[1].key.clone())
                .as_deref(),
            Some("x")
        );
        app.update(key(KeyCode::BackTab));
        assert_eq!(focused(&app).as_deref(), Some("@m"));
    }

    #[test]
    fn export_key_and_result() {
        let mut app = app();
        assert!(app.update(ch('w')).is_empty(), "w means nothing in the browser");
        select(&mut app, "syscount_demo.bt");
        app.update(key(KeyCode::Enter));
        let (run_id, _) = start_cmd(&app.update(key(KeyCode::Enter)));
        match app.update(ch('w')).as_slice() {
            [
                Cmd::ExportRun {
                    run_id: id,
                    script_id,
                    text,
                    ..
                },
            ] => {
                assert_eq!((*id, script_id.as_str()), (run_id, "syscount_demo.bt"));
                assert!(
                    text.starts_with("bpfdeck run export\nscript:   syscount_demo.bt\n"),
                    "{text}"
                );
            }
            other => panic!("{other:?}"),
        }
        app.update(Msg::Exported(Ok(vec![
            "/tmp/a.txt".into(),
            "/tmp/a.ndjson".into(),
        ])));
        assert_eq!(
            app.notice.as_ref().map(|n| n.text.as_str()),
            Some("exported: /tmp/a.txt, /tmp/a.ndjson")
        );
        app.update(Msg::Exported(Err("export dir /nope: No such file".into())));
        assert_eq!(app.notice.as_ref().map(|n| n.level), Some(Level::Error));
    }
}
