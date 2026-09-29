//! Connect dialog (`c`/`+`) and disconnect (`d`): docs/design-remote.md, "Connecting".

use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

use super::target::{LOCAL, Target, TargetId, TargetKind};
use super::{App, Level, Overlay, Screen};
use crate::bpftrace::BpftraceInfo;
use crate::bpftrace::validate::Strategy;
use crate::keymap::Action;
use crate::msg::Cmd;
use crate::remote::Dest;
use crate::remote::connect::{Check, CheckStatus, SudoChoice};
use crate::remote::facts::RemoteInfo;
use crate::remote::session::Secret;
use crate::sys::SystemInfo;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Field {
    Host,
    Port,
    Sudo,
    Password,
    Bpftrace,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SudoMode {
    Auto,
    Root,
    Password,
}

impl SudoMode {
    pub const ALL: [SudoMode; 3] = [Self::Auto, Self::Root, Self::Password];

    pub fn label(self) -> &'static str {
        match self {
            Self::Auto => "automatic",
            Self::Root => "root login",
            Self::Password => "sudo with password",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Phase {
    Editing,
    /// Checks are running for `attempt`.
    Checking,
    /// SSH needs a person; Enter continues in the terminal. ssh's message.
    NeedsAuth(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConnectForm {
    pub host: String,
    pub port: String,
    pub sudo: SudoMode,
    pub password: Secret,
    /// bpftrace path on the host; empty = look it up in root's PATH.
    pub bpftrace: String,
    pub focus: Field,
    pub phase: Phase,
    /// Id of the current attempt; results of older (cancelled) ones are ignored.
    pub attempt: u64,
    pub checks: Vec<Check>,
    pub error: Option<String>,
}

impl ConnectForm {
    fn new() -> Self {
        Self {
            host: String::new(),
            port: String::new(),
            sudo: SudoMode::Auto,
            password: Secret::default(),
            bpftrace: String::new(),
            focus: Field::Host,
            phase: Phase::Editing,
            attempt: 0,
            checks: Vec::new(),
            error: None,
        }
    }

    /// Fields in focus order (the password only with "sudo with password").
    pub fn fields(&self) -> Vec<Field> {
        let mut f = vec![Field::Host, Field::Port, Field::Sudo];
        if self.sudo == SudoMode::Password {
            f.push(Field::Password);
        }
        f.push(Field::Bpftrace);
        f
    }

    fn step(&mut self, delta: isize) {
        let fields = self.fields();
        let i = fields.iter().position(|f| *f == self.focus).unwrap_or(0) as isize;
        self.focus = fields[(i + delta).rem_euclid(fields.len() as isize) as usize];
    }

    fn cycle_sudo(&mut self, delta: isize) {
        let i = SudoMode::ALL.iter().position(|m| *m == self.sudo).unwrap_or(0) as isize;
        self.sudo = SudoMode::ALL[(i + delta).rem_euclid(3) as usize];
    }

    fn type_char(&mut self, c: char) {
        self.error = None;
        match self.focus {
            Field::Host => self.host.push(c),
            Field::Port if c.is_ascii_digit() => self.port.push(c),
            Field::Port => {}
            Field::Sudo if c == ' ' => self.cycle_sudo(1),
            Field::Sudo => {}
            Field::Password => self.password.push(c),
            Field::Bpftrace => self.bpftrace.push(c),
        }
    }

    fn backspace(&mut self) {
        self.error = None;
        match self.focus {
            Field::Host => {
                self.host.pop();
            }
            Field::Port => {
                self.port.pop();
            }
            Field::Sudo => {}
            Field::Password => self.password.pop(),
            Field::Bpftrace => {
                self.bpftrace.pop();
            }
        }
    }

    fn checking(&self) -> bool {
        self.phase == Phase::Checking
    }
}

impl App {
    fn connect_form(&mut self) -> Option<&mut ConnectForm> {
        match &mut self.overlay {
            Some(Overlay::Connect(form)) => Some(form),
            _ => None,
        }
    }

    pub(super) fn open_connect(&mut self) {
        self.overlay = Some(Overlay::Connect(ConnectForm::new()));
    }

    /// Typing into the connect dialog (unbound keys).
    pub(super) fn connect_key(&mut self, key: KeyEvent) {
        let Some(form) = self.connect_form() else {
            return;
        };
        if form.checking() {
            return;
        }
        // Editing after "ssh needs you": the next Enter tries without the terminal again.
        form.phase = Phase::Editing;
        match key.code {
            KeyCode::Char(c)
                if !key
                    .modifiers
                    .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT) =>
            {
                form.type_char(c);
            }
            KeyCode::Backspace => form.backspace(),
            _ => {}
        }
    }

    pub(super) fn connect_action(&mut self, action: Action) -> Vec<Cmd> {
        let Some(form) = self.connect_form() else {
            return Vec::new();
        };
        match action {
            Action::Close => {
                let attempt = form.attempt;
                let cancelled = form.checking();
                self.overlay = None;
                if cancelled {
                    self.notify(Level::Info, "connect cancelled".into());
                    return vec![Cmd::CancelConnect { attempt }];
                }
            }
            _ if form.checking() => {}
            Action::NextField => form.step(1),
            Action::PrevField => form.step(-1),
            Action::PrevChoice if form.focus == Field::Sudo => form.cycle_sudo(-1),
            Action::NextChoice if form.focus == Field::Sudo => form.cycle_sudo(1),
            Action::Submit => {
                let interactive = matches!(form.phase, Phase::NeedsAuth(_));
                return self.submit_connect(interactive);
            }
            _ => {}
        }
        Vec::new()
    }

    fn submit_connect(&mut self, interactive: bool) -> Vec<Cmd> {
        let labels: Vec<String> = self.targets.iter().map(|t| t.label.clone()).collect();
        let attempt = self.next_attempt;
        let target = self.next_target_id;
        let Some(form) = self.connect_form() else {
            return Vec::new();
        };
        let dest = match Dest::parse(&form.host, &form.port) {
            Ok(dest) => dest,
            Err(e) => {
                form.error = Some(e.to_string());
                form.focus = Field::Host;
                return Vec::new();
            }
        };
        if labels.contains(&dest.label()) {
            form.error = Some(format!(
                "already connected to {}: switch tabs with < >",
                dest.label()
            ));
            return Vec::new();
        }
        let sudo = match form.sudo {
            SudoMode::Auto => SudoChoice::Auto,
            SudoMode::Root => SudoChoice::Root,
            SudoMode::Password if form.password.is_empty() => {
                form.error = Some("enter the sudo password".into());
                form.focus = Field::Password;
                return Vec::new();
            }
            SudoMode::Password => SudoChoice::Password(form.password.clone()),
        };
        let bpftrace = Some(form.bpftrace.trim().to_string()).filter(|p| !p.is_empty());
        form.attempt = attempt;
        form.phase = Phase::Checking;
        form.checks.clear();
        form.error = None;
        self.next_attempt += 1;
        self.next_target_id += 1;
        vec![Cmd::Connect {
            attempt,
            target,
            dest,
            sudo,
            bpftrace,
            interactive,
        }]
    }

    /// The form of `attempt`, if it is still the one on screen.
    fn attempt_form(&mut self, attempt: u64) -> Option<&mut ConnectForm> {
        self.connect_form()
            .filter(|f| f.attempt == attempt && f.phase == Phase::Checking)
    }

    pub(super) fn on_connect_check(&mut self, attempt: u64, check: Check) {
        if let Some(form) = self.attempt_form(attempt) {
            form.checks.push(check);
        }
    }

    pub(super) fn on_connect_needs_auth(&mut self, attempt: u64, message: String) {
        if let Some(form) = self.attempt_form(attempt) {
            form.phase = Phase::NeedsAuth(message);
        }
    }

    pub(super) fn on_connect_failed(&mut self, attempt: u64, reason: Option<String>) {
        if let Some(form) = self.attempt_form(attempt) {
            form.phase = Phase::Editing;
            let failed = form
                .checks
                .iter()
                .rev()
                .find(|c| c.status == CheckStatus::Fail)
                .map(|c| c.text.clone())
                .unwrap_or_default();
            if failed.starts_with("bpftrace") {
                form.focus = Field::Bpftrace;
            } else if failed.starts_with("root") {
                form.focus = if form.sudo == SudoMode::Password {
                    Field::Password
                } else {
                    Field::Sudo
                };
            }
            form.error =
                Some(reason.unwrap_or_else(|| "fix the failed check, then Enter tries again".into()));
        }
    }

    pub(super) fn on_connected(
        &mut self,
        attempt: u64,
        id: TargetId,
        dest: Dest,
        info: RemoteInfo,
        host: SystemInfo,
        bpftrace: (BpftraceInfo, Strategy),
    ) -> Vec<Cmd> {
        if self.attempt_form(attempt).is_none() {
            // Cancelled meanwhile: close what was opened.
            return vec![Cmd::Disconnect { target: id }];
        }
        self.overlay = None;
        let label = dest.label();
        let privilege = info.privilege.clone();
        let mut target = Target::new(id, label.clone(), TargetKind::Ssh(dest));
        target.remote = Some(info);
        self.targets.push(target);
        self.active = self.targets.len() - 1;
        self.screen = Screen::Browser;
        self.scroll.set(0);
        let cmds = self.on_env(id, host, Ok(bpftrace));
        self.notify(
            Level::Info,
            format!(
                "connected to {label} ({privilege}); validating {} scripts there",
                self.entries.len()
            ),
        );
        cmds
    }

    /// `d`: close the active target's connection (asks first if a run is active there).
    pub(super) fn request_disconnect(&mut self) -> Vec<Cmd> {
        let t = self.target();
        if t.id == LOCAL {
            self.notify(Level::Info, "local stays; d disconnects a remote tab".into());
            return Vec::new();
        }
        match t.active_run() {
            Some(run) => {
                self.overlay = Some(Overlay::Ask(super::Ask::DisconnectWhileRunning {
                    target: t.id,
                    running: format!("{} on {}", run.script_id, t.label),
                }));
                Vec::new()
            }
            None => {
                let id = t.id;
                self.disconnect(id)
            }
        }
    }

    /// Remove the target and its results; the executor stops its run (EOF on the host)
    /// and closes the SSH master.
    pub(super) fn disconnect(&mut self, id: TargetId) -> Vec<Cmd> {
        let Some(i) = self.targets.iter().position(|t| t.id == id) else {
            return Vec::new();
        };
        let t = self.targets.remove(i);
        for entry in &mut self.entries {
            entry.validations.remove(&id);
        }
        if self.active >= i {
            self.active = self.active.saturating_sub(1).min(self.targets.len() - 1);
        }
        if self.target().run.is_none() {
            self.screen = Screen::Browser;
        }
        let stopped = if t.active_run().is_some() {
            "; its run was stopped on the host"
        } else {
            ""
        };
        self.notify(Level::Info, format!("disconnected from {}{stopped}", t.label));
        vec![Cmd::Disconnect { target: id }]
    }
}

#[cfg(test)]
mod tests {
    use ratatui::crossterm::event::KeyCode;

    use super::*;
    use crate::app::fixtures::*;
    use crate::app::{Ask, ValidationState};
    use crate::msg::Msg;
    use crate::sys::{Lockdown, Privilege};
    use pretty_assertions::assert_eq;

    fn ready() -> App {
        ready_app(host(Privilege::Root, Lockdown::None))
    }

    fn form(app: &App) -> &ConnectForm {
        match &app.overlay {
            Some(Overlay::Connect(form)) => form,
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn connecting_opens_an_active_tab_and_validates_there() {
        let mut app = ready();
        press(&mut app, KeyCode::Char('c'));
        type_text(&mut app, "ops@db-02");
        let cmds = press(&mut app, KeyCode::Enter);
        assert_eq!(
            cmds,
            vec![Cmd::Connect {
                attempt: 1,
                target: 1,
                dest: Dest::parse("ops@db-02", "").expect("dest"),
                sudo: SudoChoice::Auto,
                bpftrace: None,
                interactive: false,
            }]
        );
        assert_eq!(form(&app).phase, Phase::Checking);
        // Typing while checking does nothing.
        type_text(&mut app, "x");
        assert_eq!(form(&app).host, "ops@db-02");
        app.update(Msg::ConnectCheck {
            attempt: 1,
            check: checks()[0].clone(),
        });
        assert_eq!(form(&app).checks.len(), 1);
        app = ready();
        let cmds = connect(&mut app, "ops@db-02");
        assert!(app.overlay.is_none());
        assert_eq!(app.targets.len(), 2);
        assert_eq!((app.active, app.target().label.as_str()), (1, "ops@db-02"));
        assert!(app.target().is_remote() && app.target().usable());
        assert_eq!(
            cmds.len(),
            app.entries.len(),
            "every script, once, on the new target only"
        );
        assert!(cmds.iter().all(|c| matches!(c, Cmd::Validate { target: 1, .. })));
        // The list shows the new target's (pending) state; local results are kept.
        let entry = &app.entries[0];
        assert_eq!(app.validation_of(entry), &ValidationState::Pending);
        assert!(matches!(entry.validation(LOCAL), ValidationState::Done(_)));
    }

    #[test]
    fn form_errors() {
        let mut app = ready();
        press(&mut app, KeyCode::Char('+'));
        assert!(press(&mut app, KeyCode::Enter).is_empty());
        assert!(
            form(&app)
                .error
                .as_deref()
                .is_some_and(|e| e.starts_with("enter a host"))
        );
        type_text(&mut app, "-oProxyCommand=x");
        assert!(press(&mut app, KeyCode::Enter).is_empty());
        assert!(
            form(&app)
                .error
                .as_deref()
                .is_some_and(|e| e.starts_with("invalid host"))
        );

        // sudo with password needs one; the field appears in the focus order.
        let mut app = ready();
        press(&mut app, KeyCode::Char('c'));
        type_text(&mut app, "db-02");
        press(&mut app, KeyCode::Tab);
        type_text(&mut app, "22a");
        assert_eq!(form(&app).port, "22", "digits only");
        press(&mut app, KeyCode::Tab);
        press(&mut app, KeyCode::Right);
        press(&mut app, KeyCode::Char(' '));
        assert_eq!(form(&app).sudo, SudoMode::Password);
        assert!(press(&mut app, KeyCode::Enter).is_empty());
        assert_eq!(form(&app).focus, Field::Password);
        type_text(&mut app, "hunter2");
        let cmds = press(&mut app, KeyCode::Enter);
        let dump = format!("{cmds:?}");
        assert!(
            dump.contains("Password(Secret(***))") && !dump.contains("hunter2"),
            "{dump}"
        );

        // The same host twice.
        let mut app = ready();
        connect(&mut app, "db-02");
        press(&mut app, KeyCode::Char('c'));
        type_text(&mut app, "db-02");
        assert!(press(&mut app, KeyCode::Enter).is_empty());
        assert!(
            form(&app)
                .error
                .as_deref()
                .is_some_and(|e| e.contains("already connected"))
        );
    }

    #[test]
    fn failures_auth_and_cancel() {
        let mut app = ready();
        let attempt = start_connect(&mut app, "db-02");
        app.update(Msg::ConnectCheck {
            attempt,
            check: Check {
                status: CheckStatus::Fail,
                text: "bpftrace: not found in root's PATH".into(),
            },
        });
        app.update(Msg::ConnectFailed {
            attempt,
            reason: None,
        });
        assert_eq!(
            (form(&app).phase.clone(), form(&app).focus),
            (Phase::Editing, Field::Bpftrace)
        );
        type_text(&mut app, "/opt/bpftrace");
        let cmds = press(&mut app, KeyCode::Enter);
        assert!(
            matches!(cmds.as_slice(), [Cmd::Connect { attempt: 2, bpftrace: Some(p), .. }] if p == "/opt/bpftrace"),
            "{cmds:?}"
        );

        // SSH needs a person: Enter goes to the terminal.
        app.update(Msg::ConnectNeedsAuth {
            attempt: 2,
            message: "db-02: Permission denied (publickey,password).".into(),
        });
        assert!(matches!(form(&app).phase, Phase::NeedsAuth(_)));
        let cmds = press(&mut app, KeyCode::Enter);
        assert!(
            matches!(
                cmds.as_slice(),
                [Cmd::Connect {
                    attempt: 3,
                    interactive: true,
                    ..
                }]
            ),
            "{cmds:?}"
        );

        // Esc while checking cancels; a late success is closed again.
        let cmds = press(&mut app, KeyCode::Esc);
        assert_eq!(cmds, vec![Cmd::CancelConnect { attempt: 3 }]);
        assert!(app.overlay.is_none());
        let late = app.update(Msg::Connected {
            attempt: 3,
            target: 3,
            dest: Dest::parse("db-02", "").expect("dest"),
            info: RemoteInfo::default(),
            host: host(Privilege::Root, Lockdown::None),
            bpftrace: bpftrace(),
        });
        assert_eq!(late, vec![Cmd::Disconnect { target: 3 }]);
        assert_eq!(app.targets.len(), 1);
    }

    #[test]
    fn disconnecting() {
        let mut app = ready();
        assert!(press(&mut app, KeyCode::Char('d')).is_empty(), "local stays");
        assert_eq!(app.targets.len(), 1);

        connect(&mut app, "db-02");
        connect(&mut app, "db-03");
        press(&mut app, KeyCode::Char('<'));
        assert_eq!(app.target().label, "db-02");
        let cmds = press(&mut app, KeyCode::Char('d'));
        assert_eq!(cmds, vec![Cmd::Disconnect { target: 1 }]);
        assert_eq!(
            app.targets.iter().map(|t| t.label.as_str()).collect::<Vec<_>>(),
            ["local", "db-03"]
        );
        assert_eq!(app.target().label, "local");
        assert!(app.entries.iter().all(|e| !e.validations.contains_key(&1)));

        // With a run there: ask first.
        press(&mut app, KeyCode::Char('>'));
        app.targets[1].run = Some(crate::model::run_state::Run::new(
            7,
            "syscount_demo.bt",
            "bpftrace …",
        ));
        assert!(press(&mut app, KeyCode::Char('d')).is_empty());
        assert!(matches!(
            &app.overlay,
            Some(Overlay::Ask(Ask::DisconnectWhileRunning { target: 2, .. }))
        ));
        assert_eq!(
            press(&mut app, KeyCode::Char('y')),
            vec![Cmd::Disconnect { target: 2 }]
        );
        assert_eq!(app.targets.len(), 1);
    }
}
