//! Application state and reducer: `App::update(Msg) -> Vec<Cmd>`. No I/O here, ever.

mod filter;
mod run;

use std::cell::Cell;

use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyModifiers};

use crate::bpftrace::BpftraceInfo;
use crate::bpftrace::validate::{Strategy, Validation, ValidationRequest};
use crate::catalog::{Catalog, Script};
use crate::keymap::{self, Action, Context};
use crate::model::form::ParamForm;
use crate::model::run_state::Run;
use crate::msg::{Cmd, Msg};
use crate::source::{Origin, ResolvedSource, SourceSpec};
use crate::sys::SystemInfo;
use filter::Fuzzy;
pub use run::{Ask, Confirm, LogView};

pub const TABS: [&str; 3] = ["Info", "Source", "Validation"];
/// Lines moved by one detail scroll step.
const SCROLL_STEP: u16 = 10;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LoadState {
    Loading(String),
    Ready,
    Failed(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BpftraceState {
    Detecting,
    Ready {
        info: BpftraceInfo,
        strategy: Strategy,
    },
    /// Not installed / not runnable: browsing works, validation and runs don't.
    Missing(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ValidationState {
    Pending,
    Done(Validation),
    /// Validation cannot run here (no bpftrace).
    Skipped(String),
}

#[derive(Debug, Clone)]
pub struct Entry {
    pub script: Script,
    pub request: ValidationRequest,
    pub validation: ValidationState,
}

impl Entry {
    pub fn id(&self) -> &str {
        &self.script.file.id
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Screen {
    Browser,
    /// The right pane shows the run (header + log).
    Run,
}

/// Modal on top of the current screen.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Overlay {
    Help,
    Params { script_id: String, form: ParamForm },
    Confirm(Confirm),
    Ask(Ask),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Level {
    Info,
    Warn,
    Error,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Notice {
    pub level: Level,
    pub text: String,
}

pub struct App {
    /// The source as given on the command line.
    pub input: String,
    pub should_quit: bool,
    pub load: LoadState,
    pub source: Option<ResolvedSource>,
    pub entries: Vec<Entry>,
    pub host: Option<SystemInfo>,
    pub bpftrace: BpftraceState,
    /// Indices into `entries`, in display order (filtered and ranked).
    pub visible: Vec<usize>,
    /// Position in `visible`.
    pub cursor: usize,
    pub query: String,
    /// Typing into the list filter.
    pub filter_editing: bool,
    pub screen: Screen,
    pub overlay: Option<Overlay>,
    pub help_scroll: Cell<u16>,
    pub tab: usize,
    /// Detail scroll offset. The renderer clamps it to the content, hence the `Cell`.
    pub scroll: Cell<u16>,
    pub notice: Option<Notice>,
    /// The current or last run (D-010: one at a time). Its output stays viewable.
    pub run: Option<Run>,
    pub log_view: LogView,
    /// Run view takes the whole width (`z`).
    pub full_width: bool,
    next_run_id: u64,
    quit_after_run: bool,
    fuzzy: Fuzzy,
}

impl App {
    pub fn new(input: String) -> Self {
        Self {
            input,
            should_quit: false,
            load: LoadState::Loading(String::new()),
            source: None,
            entries: Vec::new(),
            host: None,
            bpftrace: BpftraceState::Detecting,
            visible: Vec::new(),
            cursor: 0,
            query: String::new(),
            filter_editing: false,
            screen: Screen::Browser,
            overlay: None,
            help_scroll: Cell::new(0),
            tab: 0,
            scroll: Cell::new(0),
            notice: None,
            run: None,
            log_view: LogView::following(),
            full_width: false,
            next_run_id: 0,
            quit_after_run: false,
            fuzzy: Fuzzy::new(),
        }
    }

    /// Commands to run once at startup.
    pub fn init(&mut self) -> Vec<Cmd> {
        self.load = LoadState::Loading(loading_message(&self.input));
        vec![
            Cmd::DetectEnv,
            Cmd::Load {
                input: self.input.clone(),
            },
        ]
    }

    pub fn selected(&self) -> Option<&Entry> {
        self.visible.get(self.cursor).and_then(|&i| self.entries.get(i))
    }

    pub fn context(&self) -> Context {
        match (&self.overlay, self.screen) {
            (Some(Overlay::Help), _) => Context::Help,
            (Some(Overlay::Params { .. }), _) => Context::Form,
            (Some(Overlay::Confirm(_)), _) => Context::Confirm,
            (Some(Overlay::Ask(_)), _) => Context::Ask,
            (None, Screen::Browser) if self.filter_editing => Context::Filter,
            (None, Screen::Browser) => Context::Browser,
            (None, Screen::Run) if self.log_view.editing => Context::LogFilter,
            (None, Screen::Run) => Context::Run,
        }
    }

    /// The run is showing in the right pane.
    pub fn showing_run(&self) -> Option<&Run> {
        self.run.as_ref().filter(|_| self.screen == Screen::Run)
    }

    pub fn update(&mut self, msg: Msg) -> Vec<Cmd> {
        match msg {
            Msg::Key(key) => self.on_key(key),
            Msg::Resize => Vec::new(),
            Msg::Terminate => {
                self.should_quit = true;
                Vec::new()
            }
            Msg::Loaded(result) => self.on_loaded(result),
            Msg::EnvDetected { host, bpftrace } => self.on_env(host, bpftrace),
            Msg::Validated {
                id,
                content_hash,
                validation,
            } => {
                // A rescan may have replaced the script meanwhile: only accept a result
                // for the content that is on screen.
                if let Some(entry) = self
                    .entries
                    .iter_mut()
                    .find(|e| e.script.file.id == id && e.request.content_hash == content_hash)
                {
                    entry.validation = ValidationState::Done(validation);
                }
                Vec::new()
            }
            Msg::EditorClosed { id, copy, result } => match result {
                Err(e) => {
                    self.notify(Level::Error, format!("editor: {e}"));
                    Vec::new()
                }
                Ok(()) if copy => {
                    self.notify(
                        Level::Warn,
                        format!("{id}: edited a temporary copy; changes are not saved to the git checkout"),
                    );
                    Vec::new()
                }
                Ok(()) => self.rescan(),
            },
            Msg::RunStarted { run_id, at } => {
                self.on_run_started(run_id, at);
                Vec::new()
            }
            Msg::RunFailed { run_id, reason } => {
                self.on_run_failed(run_id, &reason);
                Vec::new()
            }
            Msg::Run { run_id, at, batch } => {
                self.on_run_batch(run_id, at, batch);
                Vec::new()
            }
            Msg::Tick(now) => {
                if let Some(run) = &mut self.run {
                    run.tick(now);
                }
                Vec::new()
            }
        }
    }

    fn on_loaded(&mut self, result: Result<Catalog, String>) -> Vec<Cmd> {
        // "rescanning…" and similar progress notes are done now.
        if self.notice.as_ref().is_some_and(|n| n.level == Level::Info) {
            self.notice = None;
        }
        let catalog = match result {
            Ok(catalog) => catalog,
            Err(e) => {
                if self.entries.is_empty() {
                    self.load = LoadState::Failed(e);
                } else {
                    self.load = LoadState::Ready;
                    self.notify(Level::Error, format!("rescan failed: {e}"));
                }
                return Vec::new();
            }
        };
        let keep = self.selected().map(|e| e.id().to_string());
        let pending = match &self.bpftrace {
            BpftraceState::Missing(reason) => ValidationState::Skipped(reason.clone()),
            _ => ValidationState::Pending,
        };
        self.entries = catalog
            .scripts
            .into_iter()
            .map(|script| Entry {
                request: ValidationRequest::new(&script.file.path, &script.content, &script.meta),
                script,
                validation: pending.clone(),
            })
            .collect();
        self.source = Some(catalog.source);
        self.load = LoadState::Ready;
        match catalog.warnings.as_slice() {
            [] => {}
            [one] => self.notify(Level::Warn, one.clone()),
            [first, rest @ ..] => self.notify(Level::Warn, format!("{first} (+{} more)", rest.len())),
        }
        self.refilter(keep.as_deref());
        self.validation_cmds()
    }

    fn on_env(&mut self, host: SystemInfo, bpftrace: Result<(BpftraceInfo, Strategy), String>) -> Vec<Cmd> {
        self.host = Some(host);
        match bpftrace {
            Ok((info, strategy)) => {
                self.bpftrace = BpftraceState::Ready { info, strategy };
                self.validation_cmds()
            }
            Err(reason) => {
                for entry in &mut self.entries {
                    if entry.validation == ValidationState::Pending {
                        entry.validation = ValidationState::Skipped(reason.clone());
                    }
                }
                self.notify(Level::Warn, format!("validation disabled: {reason}"));
                self.bpftrace = BpftraceState::Missing(reason);
                Vec::new()
            }
        }
    }

    /// Validate every pending script, once both the scripts and bpftrace are known.
    fn validation_cmds(&self) -> Vec<Cmd> {
        if !matches!(self.bpftrace, BpftraceState::Ready { .. }) {
            return Vec::new();
        }
        self.entries
            .iter()
            .filter(|e| e.validation == ValidationState::Pending)
            .map(|e| Cmd::Validate {
                id: e.id().to_string(),
                request: e.request.clone(),
            })
            .collect()
    }

    fn rescan(&mut self) -> Vec<Cmd> {
        match self.source.clone() {
            Some(source) => {
                self.notify(Level::Info, "rescanning…".to_string());
                vec![Cmd::Rescan { source }]
            }
            // The first load failed: try it again from scratch.
            None => self
                .init()
                .into_iter()
                .filter(|c| matches!(c, Cmd::Load { .. }))
                .collect(),
        }
    }

    fn on_key(&mut self, key: KeyEvent) -> Vec<Cmd> {
        if key.kind == KeyEventKind::Release {
            return Vec::new();
        }
        self.notice = None;
        let context = self.context();
        let action = keymap::lookup(context, &key);
        // Text input contexts: unbound keys are typed.
        if action.is_none() {
            match context {
                Context::Filter => self.edit_query(key),
                Context::LogFilter => self.log_filter_key(key),
                Context::Form => self.form_key(key),
                _ => {}
            }
            return Vec::new();
        }
        let Some(action) = action else {
            return Vec::new();
        };
        match context {
            Context::Browser | Context::Filter => self.browser_action(action),
            Context::Help => {
                match action {
                    Action::Close => self.overlay = None,
                    Action::ScrollDown => self.help_scroll.set(self.help_scroll.get().saturating_add(1)),
                    Action::ScrollUp => self.help_scroll.set(self.help_scroll.get().saturating_sub(1)),
                    _ => {}
                }
                Vec::new()
            }
            Context::Form => self.form_action(action),
            Context::Confirm => self.confirm_action(action),
            Context::Ask => self.ask_action(action),
            Context::Run | Context::LogFilter => self.run_action(action),
        }
    }

    fn browser_action(&mut self, action: Action) -> Vec<Cmd> {
        match action {
            Action::Down => self.move_cursor(1),
            Action::Up => self.move_cursor(-1),
            Action::Top => self.set_cursor(0),
            Action::Bottom => self.set_cursor(self.visible.len().saturating_sub(1)),
            Action::NextTab => self.set_tab((self.tab + 1) % TABS.len()),
            Action::PrevTab => self.set_tab((self.tab + TABS.len() - 1) % TABS.len()),
            Action::Tab(n) => self.set_tab(n.min(TABS.len() - 1)),
            Action::ScrollDown => self.scroll.set(self.scroll.get().saturating_add(SCROLL_STEP)),
            Action::ScrollUp => self.scroll.set(self.scroll.get().saturating_sub(SCROLL_STEP)),
            Action::OpenFilter => self.filter_editing = true,
            Action::AcceptFilter => self.filter_editing = false,
            Action::ClearFilter => {
                self.filter_editing = false;
                if !self.query.is_empty() {
                    self.query.clear();
                    let keep = self.selected().map(|e| e.id().to_string());
                    self.refilter(keep.as_deref());
                }
            }
            Action::Edit => return self.edit(),
            Action::Rescan => return self.rescan(),
            Action::Help => {
                self.help_scroll.set(0);
                self.overlay = Some(Overlay::Help);
            }
            Action::Run => return self.request_run(),
            Action::ShowRun => {
                if self.run.is_some() {
                    self.screen = Screen::Run;
                } else {
                    self.notify(Level::Info, "no run yet: select a script and press Enter".into());
                }
            }
            Action::Quit => self.quit_or_ask(),
            _ => {}
        }
        Vec::new()
    }

    fn edit_query(&mut self, key: KeyEvent) {
        match key.code {
            KeyCode::Char(c)
                if !key
                    .modifiers
                    .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT) =>
            {
                self.query.push(c);
            }
            KeyCode::Backspace => {
                self.query.pop();
            }
            _ => return,
        }
        // Typing jumps to the best match.
        self.refilter(None);
    }

    fn edit(&mut self) -> Vec<Cmd> {
        let Some(entry) = self.selected() else {
            return Vec::new();
        };
        let copy = matches!(self.source.as_ref().map(|s| &s.origin), Some(Origin::Git { .. }));
        vec![Cmd::OpenEditor {
            id: entry.id().to_string(),
            path: entry.script.file.path.clone(),
            copy,
        }]
    }

    fn move_cursor(&mut self, delta: isize) {
        let last = self.visible.len().saturating_sub(1);
        self.set_cursor(self.cursor.saturating_add_signed(delta).min(last));
    }

    fn set_cursor(&mut self, cursor: usize) {
        if cursor != self.cursor {
            self.scroll.set(0);
        }
        self.cursor = cursor;
    }

    fn set_tab(&mut self, tab: usize) {
        self.tab = tab;
        self.scroll.set(0);
    }

    /// Recompute `visible` from the query; keep `keep` selected if it is still visible.
    fn refilter(&mut self, keep: Option<&str>) {
        let haystacks: Vec<String> = self
            .entries
            .iter()
            .map(|e| {
                format!(
                    "{} {}",
                    e.id(),
                    e.script.meta.description.as_deref().unwrap_or_default()
                )
            })
            .collect();
        self.visible = self.fuzzy.rank(&self.query, haystacks.iter().map(String::as_str));
        let pos = keep.and_then(|id| self.visible.iter().position(|&i| self.entries[i].id() == id));
        self.set_cursor(pos.unwrap_or(0));
    }

    fn notify(&mut self, level: Level, text: String) {
        self.notice = Some(Notice { level, text });
    }
}

fn loading_message(input: &str) -> String {
    match SourceSpec::parse(input) {
        SourceSpec::Git(spec) => format!("Updating {}…", spec.url),
        SourceSpec::Local(_) => "Scanning…".to_string(),
    }
}

/// Deterministic app states for reducer and snapshot tests.
#[cfg(test)]
pub(crate) mod fixtures {
    use std::path::{Path, PathBuf};

    use super::*;
    use crate::bpftrace::Version;
    use crate::bpftrace::validate::{ProbeCheck, Verdict};
    use crate::discovery::{self, ScriptFile};
    use crate::source::Origin;
    use crate::sys::{Lockdown, Privilege};

    /// The fixture scripts, with paths under a fixed fake root so snapshots are stable.
    pub fn catalog() -> Catalog {
        let dir = Path::new(concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/scripts"));
        let root = PathBuf::from("/srv/bpf");
        let scripts = discovery::walk(dir)
            .expect("walk")
            .scripts
            .into_iter()
            .map(|f| {
                let content = std::fs::read_to_string(&f.path).expect("read");
                let file = ScriptFile {
                    path: root.join(&f.id),
                    ..f
                };
                Script::new(file, content)
            })
            .collect();
        Catalog {
            source: ResolvedSource {
                origin: Origin::Local,
                root,
                file: None,
                warnings: Vec::new(),
            },
            scripts,
            warnings: Vec::new(),
        }
    }

    pub fn host(privilege: Privilege, lockdown: Lockdown) -> SystemInfo {
        SystemInfo {
            privilege,
            lockdown,
            kernel_release: "6.1.0-18-amd64".into(),
        }
    }

    pub fn bpftrace() -> (BpftraceInfo, Strategy) {
        let info = BpftraceInfo {
            path: "/usr/bin/bpftrace".into(),
            version: Some(Version {
                major: 0,
                minor: 21,
                patch: 2,
            }),
            version_raw: "bpftrace v0.21.2".into(),
            supports_dry_run: true,
        };
        (info, Strategy::DryRun)
    }

    fn validation(verdict: Verdict, output: &str, notes: &[&str], probes: &[(&str, bool)]) -> Validation {
        Validation {
            verdict,
            strategy: Strategy::DryRun,
            output: output.into(),
            notes: notes.iter().map(|n| n.to_string()).collect(),
            probes: probes
                .iter()
                .map(|(p, found)| ProbeCheck {
                    probe: p.to_string(),
                    found: *found,
                })
                .collect(),
        }
    }

    /// Loaded, bpftrace detected, every script validated except `shebang_no_ext` (pending).
    pub fn ready_app(host: SystemInfo) -> App {
        let mut app = App::new("/srv/bpf".into());
        app.init();
        app.update(Msg::EnvDetected {
            host,
            bpftrace: Ok(bpftrace()),
        });
        let cmds = app.update(Msg::Loaded(Ok(catalog())));
        for cmd in cmds {
            let Cmd::Validate { id, request } = cmd else {
                continue;
            };
            let v = match id.as_str() {
                "shebang_no_ext" => continue,
                "missing_probe_demo.bt" => validation(
                    Verdict::Failed {
                        reason:
                            "stdin:1:1-44: ERROR: kprobe:this_function_does_not_exist_bpfdeck: No such file"
                                .into(),
                    },
                    "stdin:1:1-44: ERROR: kprobe:this_function_does_not_exist_bpfdeck: No such file",
                    &[],
                    &[],
                ),
                "unsafe_demo.bt" => validation(
                    Verdict::NeedsUnsafe,
                    "stdin:7:3-17: ERROR: system() is unsafe. To use you need the --unsafe flag",
                    &[],
                    &[],
                ),
                "vfs_latency_demo.bt" => Validation {
                    strategy: Strategy::ProbeList,
                    ..validation(
                        Verdict::Partial { found: 1, total: 2 },
                        "",
                        &["heuristic: probes looked up with `bpftrace -l`, script not loaded"],
                        &[("kprobe:vfs_read", true), ("kretprobe:vfs_read", false)],
                    )
                },
                "params_demo.bt" => validation(
                    Verdict::Ok,
                    "",
                    &["positional parameters $1..$1 were set to 0 for the dry run"],
                    &[],
                ),
                _ => validation(Verdict::Ok, "", &[], &[]),
            };
            app.update(Msg::Validated {
                id,
                content_hash: request.content_hash,
                validation: v,
            });
        }
        app
    }
}

#[cfg(test)]
mod tests {
    use super::fixtures::*;
    use super::*;
    use crate::bpftrace::validate::Verdict;
    use crate::sys::{Lockdown, Privilege};
    use pretty_assertions::assert_eq;

    fn key(code: KeyCode) -> Msg {
        Msg::Key(KeyEvent::new(code, KeyModifiers::NONE))
    }

    fn ch(c: char) -> Msg {
        key(KeyCode::Char(c))
    }

    fn ids(app: &App) -> Vec<&str> {
        app.visible.iter().map(|&i| app.entries[i].id()).collect()
    }

    fn validate_ids(cmds: &[Cmd]) -> Vec<&str> {
        cmds.iter()
            .filter_map(|c| match c {
                Cmd::Validate { id, .. } => Some(id.as_str()),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn startup_loads_and_detects() {
        let mut app = App::new("https://github.com/bpftrace/bpftrace#v0.21.0".into());
        let cmds = app.init();
        assert_eq!(
            cmds,
            vec![
                Cmd::DetectEnv,
                Cmd::Load {
                    input: "https://github.com/bpftrace/bpftrace#v0.21.0".into()
                }
            ]
        );
        assert_eq!(
            app.load,
            LoadState::Loading("Updating https://github.com/bpftrace/bpftrace…".into())
        );
    }

    #[test]
    fn validation_starts_when_both_scripts_and_bpftrace_are_known() {
        // Scripts first, then bpftrace.
        let mut app = App::new("x".into());
        app.init();
        assert!(app.update(Msg::Loaded(Ok(catalog()))).is_empty());
        let cmds = app.update(Msg::EnvDetected {
            host: host(Privilege::Root, Lockdown::None),
            bpftrace: Ok(bpftrace()),
        });
        assert_eq!(validate_ids(&cmds).len(), 8);

        // bpftrace first, then scripts.
        let mut app = App::new("x".into());
        app.init();
        let cmds = app.update(Msg::EnvDetected {
            host: host(Privilege::Root, Lockdown::None),
            bpftrace: Ok(bpftrace()),
        });
        assert!(cmds.is_empty());
        assert_eq!(validate_ids(&app.update(Msg::Loaded(Ok(catalog())))).len(), 8);
    }

    #[test]
    fn missing_bpftrace_skips_validation() {
        let mut app = App::new("x".into());
        app.init();
        app.update(Msg::Loaded(Ok(catalog())));
        let cmds = app.update(Msg::EnvDetected {
            host: host(Privilege::None, Lockdown::Unknown),
            bpftrace: Err("cannot run bpftrace: not found".into()),
        });
        assert!(cmds.is_empty());
        assert!(
            app.entries
                .iter()
                .all(|e| matches!(e.validation, ValidationState::Skipped(_)))
        );
        assert_eq!(app.notice.as_ref().map(|n| n.level), Some(Level::Warn));
        // A rescan keeps them skipped and asks for nothing.
        assert!(validate_ids(&app.update(Msg::Loaded(Ok(catalog())))).is_empty());
    }

    #[test]
    fn stale_validation_results_are_ignored() {
        let mut app = ready_app(host(Privilege::Root, Lockdown::None));
        app.update(Msg::Validated {
            id: "shebang_no_ext".into(),
            content_hash: "not-the-current-content".into(),
            validation: ok_validation(),
        });
        let after = app
            .entries
            .iter()
            .find(|e| e.id() == "shebang_no_ext")
            .expect("entry");
        assert_eq!(after.validation, ValidationState::Pending);
    }

    #[test]
    fn navigation_and_tabs() {
        let mut app = ready_app(host(Privilege::Root, Lockdown::None));
        assert_eq!(app.selected().map(Entry::id), Some("missing_probe_demo.bt"));
        app.update(ch('j'));
        app.update(key(KeyCode::Down));
        assert_eq!(app.selected().map(Entry::id), Some("no_header.bt"));
        app.update(ch('G'));
        assert_eq!(app.selected().map(Entry::id), Some("vfs_latency_demo.bt"));
        app.update(ch('j'));
        assert_eq!(
            app.selected().map(Entry::id),
            Some("vfs_latency_demo.bt"),
            "stays at the end"
        );
        app.update(ch('g'));
        app.update(ch('k'));
        assert_eq!(app.cursor, 0);

        app.update(key(KeyCode::Tab));
        assert_eq!(app.tab, 1);
        app.update(key(KeyCode::BackTab));
        app.update(key(KeyCode::BackTab));
        assert_eq!(app.tab, 2);
        app.update(ch('1'));
        assert_eq!(app.tab, 0);

        app.update(key(KeyCode::PageDown));
        assert_eq!(app.scroll.get(), SCROLL_STEP);
        app.update(ch('j'));
        assert_eq!(app.scroll.get(), 0, "new selection starts at the top");
    }

    #[test]
    fn filter_typing_accept_and_clear() {
        let mut app = ready_app(host(Privilege::Root, Lockdown::None));
        app.update(ch('/'));
        assert!(app.filter_editing);
        for c in "tcpconn".chars() {
            app.update(ch(c));
        }
        // Fuzzy: best match first, and the cursor jumps to it.
        assert_eq!(ids(&app).first(), Some(&"net/tcpconnect_demo.bt"));
        assert_eq!(app.selected().map(Entry::id), Some("net/tcpconnect_demo.bt"));
        let matched = ids(&app).len();
        assert!(matched < 8, "{:?}", ids(&app));
        // q and j are text while filtering.
        app.update(ch('q'));
        app.update(ch('j'));
        assert!(!app.should_quit);
        assert_eq!(app.query, "tcpconnqj");
        app.update(key(KeyCode::Backspace));
        app.update(key(KeyCode::Backspace));
        assert_eq!(app.query, "tcpconn");
        app.update(key(KeyCode::Enter));
        assert!(!app.filter_editing);
        assert_eq!(ids(&app).len(), matched, "accepted filter stays");

        app.update(key(KeyCode::Esc));
        assert_eq!(app.query, "");
        assert_eq!(ids(&app).len(), 8);
        assert_eq!(
            app.selected().map(Entry::id),
            Some("net/tcpconnect_demo.bt"),
            "selection kept"
        );

        // Description matches too.
        app.update(ch('/'));
        for c in "histogram".chars() {
            app.update(ch(c));
        }
        assert_eq!(ids(&app).first(), Some(&"vfs_latency_demo.bt"));
        app.update(key(KeyCode::Esc));
        assert!(!app.filter_editing);
    }

    #[test]
    fn edit_rescan_and_editor_results() {
        let mut app = ready_app(host(Privilege::Root, Lockdown::None));
        let cmds = app.update(ch('e'));
        assert_eq!(
            cmds,
            vec![Cmd::OpenEditor {
                id: "missing_probe_demo.bt".into(),
                path: "/srv/bpf/missing_probe_demo.bt".into(),
                copy: false,
            }]
        );
        let rescan = vec![Cmd::Rescan {
            source: catalog().source,
        }];
        assert_eq!(
            app.update(Msg::EditorClosed {
                id: "missing_probe_demo.bt".into(),
                copy: false,
                result: Ok(())
            }),
            rescan
        );
        assert_eq!(app.update(ch('r')), rescan);
        assert!(
            app.update(Msg::EditorClosed {
                id: "x".into(),
                copy: true,
                result: Ok(())
            })
            .is_empty()
        );
        assert_eq!(app.notice.as_ref().map(|n| n.level), Some(Level::Warn));
        app.update(Msg::EditorClosed {
            id: "x".into(),
            copy: false,
            result: Err("vi: not found".into()),
        });
        assert_eq!(app.notice.as_ref().map(|n| n.level), Some(Level::Error));
    }

    #[test]
    fn rescan_keeps_selection_and_revalidates_changed_scripts() {
        let mut app = ready_app(host(Privilege::Root, Lockdown::None));
        app.update(ch('G'));
        let mut catalog = catalog();
        catalog.scripts.retain(|s| s.file.id != "no_header.bt");
        assert_eq!(app.update(ch('r')).len(), 1);
        assert_eq!(app.notice.as_ref().map(|n| n.level), Some(Level::Info));
        let cmds = app.update(Msg::Loaded(Ok(catalog)));
        assert_eq!(app.notice, None, "progress notice cleared when the rescan lands");
        assert_eq!(app.selected().map(Entry::id), Some("vfs_latency_demo.bt"));
        // Everything is re-requested; the executor's validator cache answers unchanged ones.
        assert_eq!(validate_ids(&cmds).len(), 7);
    }

    #[test]
    fn failed_load_then_retry() {
        let mut app = App::new("/nope".into());
        app.init();
        app.update(Msg::Loaded(Err("/nope does not exist".into())));
        assert_eq!(app.load, LoadState::Failed("/nope does not exist".into()));
        assert_eq!(
            app.update(ch('r')),
            vec![Cmd::Load {
                input: "/nope".into()
            }]
        );
    }

    #[test]
    fn help_and_quit() {
        let mut app = ready_app(host(Privilege::Root, Lockdown::None));
        app.update(ch('?'));
        assert_eq!(app.overlay, Some(Overlay::Help));
        app.update(ch('j'));
        assert_eq!(app.cursor, 0, "help swallows list keys");
        app.update(ch('q'));
        assert_eq!(app.overlay, None);
        assert!(!app.should_quit, "q closes help first");
        app.update(ch('q'));
        assert!(app.should_quit);

        let mut app = App::new("x".into());
        app.update(Msg::Terminate);
        assert!(app.should_quit);
    }

    fn ok_validation() -> Validation {
        Validation {
            verdict: Verdict::Ok,
            strategy: Strategy::DryRun,
            output: String::new(),
            notes: Vec::new(),
            probes: Vec::new(),
        }
    }
}
