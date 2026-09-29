//! Connect dialog (`c`/`+`) and disconnect (`d`): docs/design-remote.md, "Connecting".

use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

use super::target::{Conn, LOCAL, Target, TargetId, TargetKind};
use super::{App, Level, Overlay, Screen, ValidationState};
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
    /// Checks are running for at least one host.
    Checking,
    /// Nothing is checking and SSH needs a person for some hosts: Enter continues in the
    /// terminal, one host after the other. ssh's message for the first of them.
    NeedsAuth(String),
}

/// One host of a connect attempt (docs/design-fleet.md, F1).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HostRow {
    pub dest: Dest,
    /// Id of this host's current attempt; results of older (cancelled) ones are ignored.
    pub attempt: u64,
    /// Tab id the host gets when it connects.
    pub target: TargetId,
    pub checks: Vec<Check>,
    pub state: RowState,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RowState {
    Checking,
    /// `Rocky 9.4 · 5.14.0-427 · bpftrace v0.21.2 · root via sudo`.
    Connected(String),
    Failed,
    NeedsAuth(String),
}

impl HostRow {
    pub fn label(&self) -> String {
        self.dest.label()
    }

    /// The check that failed, if any.
    pub fn failure(&self) -> Option<&Check> {
        self.checks.iter().rev().find(|c| c.status == CheckStatus::Fail)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConnectForm {
    /// One host, or a list: `db-01 db-02`, `db-0{1..4}` (`remote::expand_hosts`).
    pub host: String,
    pub port: String,
    pub sudo: SudoMode,
    pub password: Secret,
    /// bpftrace path on the host; empty = look it up in root's PATH.
    pub bpftrace: String,
    pub focus: Field,
    /// The hosts of the last submit (connected ones stay listed).
    pub rows: Vec<HostRow>,
    pub error: Option<String>,
    /// A tab of this dialog became the active one (the first host that connects).
    activated: bool,
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
            rows: Vec::new(),
            error: None,
            activated: false,
        }
    }

    pub fn phase(&self) -> Phase {
        if self.rows.iter().any(|r| r.state == RowState::Checking) {
            return Phase::Checking;
        }
        match self.rows.iter().find_map(|r| match &r.state {
            RowState::NeedsAuth(m) => Some(m.clone()),
            _ => None,
        }) {
            Some(message) => Phase::NeedsAuth(message),
            None => Phase::Editing,
        }
    }

    /// How many hosts the host field names now (`None`: it does not parse).
    pub fn host_count(&self) -> Option<usize> {
        crate::remote::expand_hosts(&self.host).ok().map(|h| h.len())
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
        self.phase() == Phase::Checking
    }

    fn row(&mut self, attempt: u64) -> Option<&mut HostRow> {
        self.rows
            .iter_mut()
            .find(|r| r.attempt == attempt && r.state == RowState::Checking)
    }
}

impl App {
    fn connect_form(&mut self) -> Option<&mut ConnectForm> {
        match &mut self.overlay {
            Some(Overlay::Connect(form)) => Some(form),
            _ => None,
        }
    }

    /// On a tab whose connection was lost, the form starts with that host (reconnect).
    pub(super) fn open_connect(&mut self) {
        let mut form = ConnectForm::new();
        let t = self.target();
        if let (true, TargetKind::Ssh(dest)) = (t.lost(), &t.kind) {
            form.host = match &dest.user {
                Some(user) => format!("{user}@{}", dest.host),
                None => dest.host.clone(),
            };
            form.port = dest.port.map(|p| p.to_string()).unwrap_or_default();
        }
        self.overlay = Some(Overlay::Connect(form));
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
        for row in &mut form.rows {
            if let RowState::NeedsAuth(message) = &row.state {
                row.checks.push(Check {
                    status: CheckStatus::Fail,
                    text: format!("ssh: {message}"),
                });
                row.state = RowState::Failed;
            }
        }
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
                let cancel: Vec<Cmd> = form
                    .rows
                    .iter()
                    .filter(|r| r.state == RowState::Checking)
                    .map(|r| Cmd::CancelConnect { attempt: r.attempt })
                    .collect();
                self.overlay = None;
                if !cancel.is_empty() {
                    self.notify(Level::Info, "connect cancelled".into());
                }
                return cancel;
            }
            _ if form.checking() => {}
            Action::NextField => form.step(1),
            Action::PrevField => form.step(-1),
            Action::PrevChoice if form.focus == Field::Sudo => form.cycle_sudo(-1),
            Action::NextChoice if form.focus == Field::Sudo => form.cycle_sudo(1),
            Action::Submit => {
                return if matches!(form.phase(), Phase::NeedsAuth(_)) {
                    self.authenticate_in_terminal()
                } else {
                    self.submit_connect()
                };
            }
            _ => {}
        }
        Vec::new()
    }

    /// The sudo mode and bpftrace path of the form, or `None` after setting an error.
    fn connect_options(form: &mut ConnectForm) -> Option<(SudoChoice, Option<String>)> {
        let sudo = match form.sudo {
            SudoMode::Auto => SudoChoice::Auto,
            SudoMode::Root => SudoChoice::Root,
            SudoMode::Password if form.password.is_empty() => {
                form.error = Some("enter the sudo password".into());
                form.focus = Field::Password;
                return None;
            }
            SudoMode::Password => SudoChoice::Password(form.password.clone()),
        };
        let bpftrace = Some(form.bpftrace.trim().to_string()).filter(|p| !p.is_empty());
        Some((sudo, bpftrace))
    }

    /// Enter: check every host of the field that is not connected yet, all at once.
    fn submit_connect(&mut self) -> Vec<Cmd> {
        // A lost target may be connected again; its tab is then replaced.
        let connected: Vec<String> = self
            .targets
            .iter()
            .filter(|t| !t.lost())
            .map(|t| t.label.clone())
            .collect();
        let (mut attempt, mut target) = (self.next_attempt, self.next_target_id);
        let Some(form) = self.connect_form() else {
            return Vec::new();
        };
        let dests = match crate::remote::expand_hosts(&form.host).and_then(|hosts| {
            hosts
                .iter()
                .map(|h| Dest::parse(h, &form.port))
                .collect::<Result<Vec<_>, _>>()
        }) {
            Ok(dests) => dests,
            Err(e) => {
                form.error = Some(e.to_string());
                form.focus = Field::Host;
                return Vec::new();
            }
        };
        let (fresh, already): (Vec<Dest>, Vec<Dest>) =
            dests.into_iter().partition(|d| !connected.contains(&d.label()));
        if fresh.is_empty() {
            form.error = Some(match already.as_slice() {
                [one] => format!("already connected to {}: switch tabs with < >", one.label()),
                _ => "all of these hosts are connected already: switch tabs with < >".into(),
            });
            return Vec::new();
        }
        let Some((sudo, bpftrace)) = Self::connect_options(form) else {
            return Vec::new();
        };
        form.error = None;
        form.activated = false;
        form.rows.retain(|r| matches!(r.state, RowState::Connected(_)));
        let mut cmds = Vec::new();
        for dest in fresh {
            form.rows.push(HostRow {
                dest: dest.clone(),
                attempt,
                target,
                checks: Vec::new(),
                state: RowState::Checking,
            });
            cmds.push(Cmd::Connect {
                attempt,
                target,
                dest,
                sudo: sudo.clone(),
                bpftrace: bpftrace.clone(),
                interactive: false,
            });
            attempt += 1;
            target += 1;
        }
        (self.next_attempt, self.next_target_id) = (attempt, target);
        cmds
    }

    /// Enter after "ssh needs you": open those hosts' masters in the terminal, one after
    /// the other (the executor runs the commands in order), then check them.
    fn authenticate_in_terminal(&mut self) -> Vec<Cmd> {
        let mut attempt = self.next_attempt;
        let Some(form) = self.connect_form() else {
            return Vec::new();
        };
        let Some((sudo, bpftrace)) = Self::connect_options(form) else {
            return Vec::new();
        };
        let mut cmds = Vec::new();
        for row in &mut form.rows {
            if matches!(row.state, RowState::NeedsAuth(_)) {
                row.attempt = attempt;
                row.state = RowState::Checking;
                row.checks.clear();
                cmds.push(Cmd::Connect {
                    attempt,
                    target: row.target,
                    dest: row.dest.clone(),
                    sudo: sudo.clone(),
                    bpftrace: bpftrace.clone(),
                    interactive: true,
                });
                attempt += 1;
            }
        }
        self.next_attempt = attempt;
        cmds
    }

    pub(super) fn on_connect_check(&mut self, attempt: u64, check: Check) {
        if let Some(row) = self.connect_form().and_then(|f| f.row(attempt)) {
            row.checks.push(check);
        }
    }

    pub(super) fn on_connect_needs_auth(&mut self, attempt: u64, message: String) {
        if let Some(row) = self.connect_form().and_then(|f| f.row(attempt)) {
            row.state = RowState::NeedsAuth(message);
        }
    }

    pub(super) fn on_connect_failed(&mut self, attempt: u64, reason: Option<String>) {
        if let Some(row) = self.connect_form().and_then(|f| f.row(attempt)) {
            if let Some(reason) = reason {
                row.checks.push(Check {
                    status: CheckStatus::Fail,
                    text: reason,
                });
            }
            row.state = RowState::Failed;
            self.after_connect_row();
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
        let summary = [
            info.os.clone(),
            host.kernel_release.clone(),
            format!(
                "bpftrace {}",
                bpftrace
                    .0
                    .version
                    .map_or_else(|| bpftrace.0.version_raw.clone(), |v| v.to_string())
            ),
            info.privilege.clone(),
        ]
        .into_iter()
        .filter(|p| !p.is_empty())
        .collect::<Vec<_>>()
        .join(" · ");
        let Some(form) = self.connect_form() else {
            return vec![Cmd::Disconnect { target: id }];
        };
        let Some(row) = form.row(attempt) else {
            // Cancelled meanwhile: close what was opened.
            return vec![Cmd::Disconnect { target: id }];
        };
        row.state = RowState::Connected(summary);
        // The first host of this dialog that connects becomes the active tab.
        let activate = !std::mem::replace(&mut form.activated, true);
        let label = dest.label();
        let mut target = Target::new(id, label.clone(), TargetKind::Ssh(dest));
        target.remote = Some(info);
        let mut cmds = Vec::new();
        let index = match self.targets.iter().position(|t| t.lost() && t.label == label) {
            Some(i) => {
                let old = self.targets[i].id;
                for entry in &mut self.entries {
                    entry.validations.remove(&old);
                }
                self.targets[i] = target;
                cmds.push(Cmd::Disconnect { target: old });
                i
            }
            None => {
                self.targets.push(target);
                self.targets.len() - 1
            }
        };
        if activate {
            self.active = index;
            self.screen = Screen::Browser;
            self.scroll.set(0);
        }
        cmds.extend(self.on_env(id, host, Ok(bpftrace)));
        self.after_connect_row();
        cmds
    }

    /// A host finished: when none is left checking, close the dialog if all connected,
    /// else say what failed and where to fix it.
    fn after_connect_row(&mut self) {
        let scripts = self.entries.len();
        let Some(form) = self.connect_form() else {
            return;
        };
        if form.phase() != Phase::Editing {
            return;
        }
        let total = form.rows.len();
        let connected: Vec<String> = form
            .rows
            .iter()
            .filter(|r| matches!(r.state, RowState::Connected(_)))
            .map(HostRow::label)
            .collect();
        if connected.len() == total {
            let what = match connected.as_slice() {
                [one] => {
                    let privilege = self
                        .targets
                        .iter()
                        .find(|t| &t.label == one)
                        .and_then(|t| t.remote.as_ref())
                        .map(|r| r.privilege.clone())
                        .unwrap_or_default();
                    format!("connected to {one} ({privilege}); validating {scripts} scripts there")
                }
                _ => format!("connected to {total} hosts; validating {scripts} scripts on each"),
            };
            self.overlay = None;
            self.notify(Level::Info, what);
            return;
        }
        let failed = form
            .rows
            .iter()
            .find_map(|r| r.failure())
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
        form.error = Some(if total == 1 {
            "fix the failed check, then Enter tries again".into()
        } else {
            format!(
                "{} of {total} connected; fix the failures, then Enter tries the others again",
                connected.len()
            )
        });
    }

    pub(super) fn on_connection_lost(&mut self, id: TargetId, reason: String) {
        let Some(t) = self.target_by_id(id) else {
            return;
        };
        t.conn = Conn::Lost(reason.clone());
        let label = t.label.clone();
        for entry in &mut self.entries {
            if entry.validation(id) == &ValidationState::Pending {
                entry
                    .validations
                    .insert(id, ValidationState::Skipped("connection lost".into()));
            }
        }
        self.notify(
            Level::Error,
            format!("{label}: connection lost ({reason}); c reconnects, d closes the tab"),
        );
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
        assert_eq!(form(&app).phase(), Phase::Checking);
        // Typing while checking does nothing.
        type_text(&mut app, "x");
        assert_eq!(form(&app).host, "ops@db-02");
        app.update(Msg::ConnectCheck {
            attempt: 1,
            check: checks()[0].clone(),
        });
        assert_eq!(form(&app).rows[0].checks.len(), 1);
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
            (form(&app).phase(), form(&app).focus),
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
        assert!(matches!(form(&app).phase(), Phase::NeedsAuth(_)));
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
    fn lost_connection_and_reconnect() {
        let mut app = ready();
        connect(&mut app, "ops@db-02");
        app.update(Msg::ConnectionLost {
            target: 1,
            reason: "the ssh connection closed".into(),
        });
        assert!(app.target().lost() && !app.target().usable());
        assert!(
            app.notice
                .as_ref()
                .is_some_and(|n| n.text.contains("connection lost"))
        );
        // Enter explains instead of running.
        press(&mut app, KeyCode::Char('G'));
        assert!(press(&mut app, KeyCode::Enter).is_empty());
        assert!(
            app.notice
                .as_ref()
                .is_some_and(|n| n.text.contains("press c to reconnect"))
        );

        // c (from the results pane too) starts with that host; connecting again replaces
        // the tab in place.
        app.screen = Screen::Run;
        press(&mut app, KeyCode::Char('c'));
        assert_eq!(form(&app).host, "ops@db-02");
        let attempt = match press(&mut app, KeyCode::Enter).as_slice() {
            [
                Cmd::Connect {
                    attempt, target: 2, ..
                },
            ] => *attempt,
            other => panic!("{other:?}"),
        };
        let cmds = app.update(Msg::Connected {
            attempt,
            target: 2,
            dest: Dest::parse("ops@db-02", "").expect("dest"),
            info: RemoteInfo::default(),
            host: host(Privilege::Root, Lockdown::None),
            bpftrace: bpftrace(),
        });
        assert_eq!(app.targets.len(), 2);
        assert_eq!((app.active, app.target().id, app.target().lost()), (1, 2, false));
        assert_eq!(cmds[0], Cmd::Disconnect { target: 1 });
        assert!(
            cmds[1..]
                .iter()
                .all(|c| matches!(c, Cmd::Validate { target: 2, .. }))
        );
        assert!(app.entries.iter().all(|e| !e.validations.contains_key(&1)));
    }

    fn connected_msg(attempt: u64, target: TargetId, host_name: &str) -> Msg {
        Msg::Connected {
            attempt,
            target,
            dest: Dest::parse(host_name, "").expect("dest"),
            info: RemoteInfo {
                os: "Rocky Linux 9.4".into(),
                privilege: "root via sudo".into(),
                ..RemoteInfo::default()
            },
            host: host(Privilege::Root, Lockdown::None),
            bpftrace: bpftrace(),
        }
    }

    fn fail(app: &mut App, attempt: u64, text: &str) {
        app.update(Msg::ConnectCheck {
            attempt,
            check: Check {
                status: CheckStatus::Fail,
                text: text.into(),
            },
        });
        app.update(Msg::ConnectFailed {
            attempt,
            reason: None,
        });
    }

    #[test]
    fn several_hosts_connect_in_parallel() {
        let mut app = ready();
        press(&mut app, KeyCode::Char('c'));
        type_text(&mut app, "db-0{1..3}");
        assert_eq!(form(&app).host_count(), Some(3));
        let cmds = press(&mut app, KeyCode::Enter);
        let started: Vec<(u64, TargetId, String)> = cmds
            .iter()
            .map(|c| match c {
                Cmd::Connect {
                    attempt,
                    target,
                    dest,
                    interactive: false,
                    ..
                } => (*attempt, *target, dest.label()),
                other => panic!("{other:?}"),
            })
            .collect();
        assert_eq!(
            started,
            vec![
                (1, 1, "db-01".into()),
                (2, 2, "db-02".into()),
                (3, 3, "db-03".into())
            ]
        );

        // db-02 connects first and becomes the active tab; db-03 connects without taking it.
        app.update(connected_msg(2, 2, "db-02"));
        assert_eq!(app.target().label, "db-02");
        assert_eq!(form(&app).phase(), Phase::Checking, "still checking the others");
        app.update(connected_msg(3, 3, "db-03"));
        assert_eq!(app.target().label, "db-02");
        fail(&mut app, 1, "root: sudo needs a password here");
        let f = form(&app);
        assert_eq!(f.phase(), Phase::Editing);
        assert_eq!(f.focus, Field::Sudo);
        assert!(
            f.error
                .as_deref()
                .is_some_and(|e| e.starts_with("2 of 3 connected")),
            "{:?}",
            f.error
        );
        assert!(
            matches!(&f.rows[1].state, RowState::Connected(s) if s == "Rocky Linux 9.4 · 6.1.0-18-amd64 · bpftrace v0.21.2 · root via sudo")
        );
        assert_eq!(app.targets.len(), 3);

        // Enter again: only the one that failed is tried (connected hosts are skipped).
        let cmds = press(&mut app, KeyCode::Enter);
        assert!(
            matches!(cmds.as_slice(), [Cmd::Connect { attempt: 4, target: 4, dest, .. }] if dest.label() == "db-01"),
            "{cmds:?}"
        );
        assert_eq!(form(&app).rows.len(), 3, "connected rows stay listed");
        app.update(connected_msg(4, 4, "db-01"));
        assert!(app.overlay.is_none(), "all connected: the dialog closes");
        assert_eq!(app.targets.len(), 4);
        assert!(
            app.notice
                .as_ref()
                .is_some_and(|n| n.text.starts_with("connected to 3 hosts"))
        );
    }

    #[test]
    fn several_hosts_auth_cancel_and_limits() {
        let mut app = ready();
        press(&mut app, KeyCode::Char('c'));
        type_text(&mut app, "h{1..21}");
        assert!(press(&mut app, KeyCode::Enter).is_empty());
        assert!(
            form(&app)
                .error
                .as_deref()
                .is_some_and(|e| e.contains("at most 20"))
        );
        for _ in 0.."h{1..21}".len() {
            press(&mut app, KeyCode::Backspace);
        }
        type_text(&mut app, "a b c");
        let cmds = press(&mut app, KeyCode::Enter);
        assert_eq!(cmds.len(), 3);
        // a and b need interactive auth, c is still checking: Enter does nothing yet.
        for attempt in [1, 2] {
            app.update(Msg::ConnectNeedsAuth {
                attempt,
                message: "Permission denied".into(),
            });
        }
        assert_eq!(form(&app).phase(), Phase::Checking);
        fail(&mut app, 3, "ssh: Connection refused");
        assert!(matches!(form(&app).phase(), Phase::NeedsAuth(_)));
        // Enter: a and b in the terminal, one after the other; c is not retried by this.
        let cmds = press(&mut app, KeyCode::Enter);
        let terminal: Vec<(u64, TargetId, bool)> = cmds
            .iter()
            .map(|c| match c {
                Cmd::Connect {
                    attempt,
                    target,
                    interactive,
                    ..
                } => (*attempt, *target, *interactive),
                other => panic!("{other:?}"),
            })
            .collect();
        assert_eq!(terminal, vec![(4, 1, true), (5, 2, true)]);
        // Esc while they are checking cancels both.
        assert_eq!(
            press(&mut app, KeyCode::Esc),
            vec![
                Cmd::CancelConnect { attempt: 4 },
                Cmd::CancelConnect { attempt: 5 }
            ]
        );

        // Hosts that are connected already are skipped; all of them: an error.
        let mut app = ready();
        connect(&mut app, "db-01");
        press(&mut app, KeyCode::Char('c'));
        type_text(&mut app, "db-01 db-02");
        let cmds = press(&mut app, KeyCode::Enter);
        assert!(matches!(cmds.as_slice(), [Cmd::Connect { dest, .. }] if dest.label() == "db-02"));
    }

    #[test]
    fn hosts_that_disagree_are_counted() {
        let mut app = ready();
        let id = |app: &App, name: &str| app.entries.iter().position(|e| e.id() == name).expect("entry");
        // Only local: never a count.
        let syscount = id(&app, "syscount_demo.bt");
        assert_eq!(app.disagreement(&app.entries[syscount]), None);
        connect_validated(&mut app, "db-02", &["net/tcpconnect_demo.bt"]);
        connect_validated(&mut app, "db-03", &[]);
        let count = |app: &App, name: &str| app.disagreement(&app.entries[id(app, name)]);
        assert_eq!(count(&app, "syscount_demo.bt"), None, "runs everywhere");
        assert_eq!(count(&app, "net/tcpconnect_demo.bt"), Some((2, 3)));
        // Fails locally only (fixture), passes on both hosts.
        assert_eq!(count(&app, "missing_probe_demo.bt"), Some((2, 3)));
        // Pending somewhere: counted over the results that are in.
        assert_eq!(count(&app, "shebang_no_ext"), None);
        // A lost host no longer counts.
        app.update(Msg::ConnectionLost {
            target: 2,
            reason: "gone".into(),
        });
        assert_eq!(count(&app, "net/tcpconnect_demo.bt"), Some((1, 2)));
    }

    fn confirm(app: &App) -> &crate::app::Confirm {
        match &app.overlay {
            Some(Overlay::Confirm(c)) => c,
            other => panic!("{other:?}"),
        }
    }

    fn select(app: &mut App, id: &str) {
        press(app, KeyCode::Char('/'));
        type_text(app, id);
        press(app, KeyCode::Enter);
        assert_eq!(app.selected().map(|e| e.id()), Some(id));
    }

    #[test]
    fn fleet_run_on_checked_targets() {
        let mut app = ready();
        connect_validated(&mut app, "db-02", &[]);
        connect_validated(&mut app, "db-03", &["syscount_demo.bt"]);
        select(&mut app, "syscount_demo.bt");
        press(&mut app, KeyCode::Enter);
        let c = confirm(&app);
        let rows: Vec<(&str, bool)> = c.targets.iter().map(|t| (t.label.as_str(), t.checked)).collect();
        assert_eq!(
            rows,
            vec![("local", false), ("db-02", false), ("db-03", true)],
            "the active tab only"
        );
        // a: every target where it validated (not db-03).
        press(&mut app, KeyCode::Char('a'));
        let checked: Vec<&str> = confirm(&app)
            .targets
            .iter()
            .filter(|t| t.checked)
            .map(|t| t.label.as_str())
            .collect();
        assert_eq!(checked, vec!["local", "db-02"]);
        // Space on db-03 (cursor starts there): checked despite the failed validation.
        press(&mut app, KeyCode::Char(' '));
        let cmds = press(&mut app, KeyCode::Enter);
        let started: Vec<(TargetId, u64, String)> = cmds
            .iter()
            .map(|c| match c {
                Cmd::StartRun {
                    target, run_id, argv, ..
                } => (
                    *target,
                    *run_id,
                    argv.last().expect("argv").to_string_lossy().into_owned(),
                ),
                other => panic!("{other:?}"),
            })
            .collect();
        assert_eq!(
            started,
            vec![
                (LOCAL, 1, "/srv/bpf/syscount_demo.bt".into()),
                (1, 2, "script.bt".into()),
                (2, 3, "script.bt".into()),
            ]
        );
        assert_eq!(
            app.fleet.as_ref().map(|f| f.members.clone()),
            Some(vec![(LOCAL, 1), (1, 2), (2, 3)])
        );
        assert!(
            app.targets
                .iter()
                .all(|t| t.run.as_ref().is_some_and(|r| r.fleet == Some(3)))
        );
        assert_eq!((app.screen, app.target().label.as_str()), (Screen::Run, "db-03"));

        // x stops this host only, X all of them.
        let cmds = press(&mut app, KeyCode::Char('x'));
        assert_eq!(cmds, vec![Cmd::StopRun { target: 2, run_id: 3 }]);
        let cmds = press(&mut app, KeyCode::Char('X'));
        assert_eq!(
            cmds,
            vec![
                Cmd::StopRun {
                    target: LOCAL,
                    run_id: 1
                },
                Cmd::StopRun { target: 1, run_id: 2 }
            ],
            "db-03 is stopping already"
        );
    }

    #[test]
    fn busy_lost_and_single_targets() {
        let mut app = ready();
        connect_validated(&mut app, "db-02", &[]);
        connect_validated(&mut app, "db-03", &[]);
        // db-02 is busy with another script, db-03 lost: only local and... nothing else.
        app.targets[1].run = Some(crate::model::run_state::Run::new(9, "other.bt", "bpftrace"));
        app.update(Msg::ConnectionLost {
            target: 2,
            reason: "gone".into(),
        });
        press(&mut app, KeyCode::Char('<'));
        press(&mut app, KeyCode::Char('<'));
        assert_eq!(app.target().label, "local");
        select(&mut app, "syscount_demo.bt");
        press(&mut app, KeyCode::Enter);
        assert!(
            confirm(&app).targets.is_empty(),
            "one target can run: no checklist"
        );
        press(&mut app, KeyCode::Esc);

        // With two free targets the busy and lost ones are listed but cannot be checked.
        connect_validated(&mut app, "db-04", &[]);
        press(&mut app, KeyCode::Enter);
        let c = confirm(&app);
        let why: Vec<Option<&str>> = c.targets.iter().map(|t| t.unavailable.as_deref()).collect();
        assert_eq!(
            why,
            vec![
                None,
                Some("busy: other.bt is running"),
                Some("connection lost"),
                None
            ]
        );
        press(&mut app, KeyCode::Up);
        press(&mut app, KeyCode::Char(' '));
        assert!(
            confirm(&app)
                .error
                .as_deref()
                .is_some_and(|e| e == "db-03: connection lost")
        );
        // Unchecking the only checked target: nothing to run.
        press(&mut app, KeyCode::Down);
        press(&mut app, KeyCode::Char(' '));
        assert!(press(&mut app, KeyCode::Enter).is_empty());
        assert!(
            confirm(&app)
                .error
                .as_deref()
                .is_some_and(|e| e.starts_with("check at least one target"))
        );
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
