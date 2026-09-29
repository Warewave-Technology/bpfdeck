//! Application state and reducer: `App::update(Msg) -> Vec<Cmd>`. No I/O here, ever.

mod compare;
mod connect;
mod edit;
mod filter;
mod run;
pub mod target;
pub mod tree;

use std::cell::Cell;
use std::collections::{HashMap, HashSet};

use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyModifiers};

use crate::bpftrace::BpftraceInfo;
use crate::bpftrace::validate::{Strategy, Validation, ValidationRequest, Verdict};
use crate::catalog::{Catalog, Script};
use crate::keymap::{self, Action, Context};
use crate::model::form::ParamForm;
use crate::model::run_state::Run;
use crate::msg::{Cmd, Msg};
use crate::source::{Origin, ResolvedSource, SourceSpec};
use crate::sys::SystemInfo;
pub use compare::CompareView;
pub use connect::{
    ConnectForm, Field as ConnectField, Phase as ConnectPhase, RowState as ConnectRowState, SudoMode,
};
pub use edit::{Draft, Editor};
use filter::Fuzzy;
pub use run::{Ask, Confirm, FleetRun, LogView};
use target::{LOCAL, Target, TargetId};
use tree::ListRow;

pub const TABS: [&str; 3] = ["Info", "Source", "Validation"];
/// Above this many scripts the tree starts with its top-level directories collapsed, so
/// the first screen is an overview (bpftrace's repo: `src/ 7`, `tests/ 41`, `tools/ 45`).
const COLLAPSE_ABOVE: usize = 30;
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

/// Not validated yet on a target (no entry in `Entry::validations`).
static PENDING: ValidationState = ValidationState::Pending;

#[derive(Debug, Clone)]
pub struct Entry {
    /// The script as scanned from the source (never changed by bpfdeck).
    pub script: Script,
    /// For the shown version (the draft when it is active).
    pub request: ValidationRequest,
    /// Per target: each host validates the same script on its own kernel.
    pub validations: HashMap<TargetId, ValidationState>,
    /// Inline edits (D-025).
    pub draft: Option<Draft>,
}

impl Entry {
    pub fn id(&self) -> &str {
        &self.script.file.id
    }

    /// What is shown, validated and run: the active draft or the original.
    pub fn shown(&self) -> &Script {
        match &self.draft {
            Some(d) if d.active => &d.script,
            _ => &self.script,
        }
    }

    pub fn edited(&self) -> bool {
        self.draft.as_ref().is_some_and(|d| d.active)
    }

    /// The file bpftrace gets for the shown version; `None` while a draft's copy is
    /// still being written.
    pub fn run_path(&self) -> Option<&std::path::Path> {
        match &self.draft {
            Some(d) if d.active => d.path.as_deref(),
            _ => Some(&self.script.file.path),
        }
    }

    fn refresh_request(&mut self) {
        let s = self.shown();
        let path = self.run_path().unwrap_or(&self.script.file.path).to_path_buf();
        self.request = ValidationRequest::new(&path, &s.content, &s.meta);
    }

    pub fn validation(&self, target: TargetId) -> &ValidationState {
        self.validations.get(&target).unwrap_or(&PENDING)
    }
}

/// Which pane has the keyboard: the browser (top) or the results pane (bottom).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Screen {
    Browser,
    /// The active target's results tab (run header, panels, log).
    Run,
}

/// Modal on top of the current screen.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Overlay {
    Help,
    Params { script_id: String, form: ParamForm },
    Confirm(Confirm),
    Ask(Ask),
    Connect(ConnectForm),
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
    /// `local` first, then connected hosts, in tab order.
    pub targets: Vec<Target>,
    /// Index into `targets` of the selected tab: the active target.
    pub active: usize,
    next_target_id: TargetId,
    next_attempt: u64,
    /// Indices into `entries` matching the filter, in display order (ranked).
    pub visible: Vec<usize>,
    /// What the list shows: scripts, and directory rows in tree view.
    pub rows: Vec<ListRow>,
    /// Position in `rows`.
    pub cursor: usize,
    /// Tree view (`t`); the filter always shows a flat ranked list.
    pub tree: bool,
    /// Collapsed directories (relative paths) in tree view.
    pub collapsed: HashSet<String>,
    /// The user toggled the view: don't pick one automatically on the next load.
    view_chosen: bool,
    pub query: String,
    /// Typing into the list filter.
    pub filter_editing: bool,
    pub screen: Screen,
    pub overlay: Option<Overlay>,
    pub help_scroll: Cell<u16>,
    /// Inline editor on the Source tab (D-025).
    pub editor: Option<Editor>,
    /// The latest run started on several targets at once (F3).
    pub fleet: Option<FleetRun>,
    /// The fleet run's compare tab is the selected results tab (F4).
    pub compare: bool,
    pub compare_view: CompareView,
    pub tab: usize,
    /// Detail scroll offset. The renderer clamps it to the content, hence the `Cell`.
    pub scroll: Cell<u16>,
    pub notice: Option<Notice>,
    /// The results pane takes the whole screen (`z`).
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
            targets: vec![Target::local()],
            active: 0,
            next_target_id: LOCAL + 1,
            next_attempt: 1,
            visible: Vec::new(),
            rows: Vec::new(),
            cursor: 0,
            tree: false,
            collapsed: HashSet::new(),
            view_chosen: false,
            query: String::new(),
            filter_editing: false,
            screen: Screen::Browser,
            overlay: None,
            help_scroll: Cell::new(0),
            editor: None,
            fleet: None,
            compare: false,
            compare_view: CompareView::default(),
            tab: 0,
            scroll: Cell::new(0),
            notice: None,
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
            Cmd::DetectEnv { target: LOCAL },
            Cmd::Load {
                input: self.input.clone(),
            },
        ]
    }

    /// The active target (selected results tab).
    pub fn target(&self) -> &Target {
        &self.targets[self.active.min(self.targets.len() - 1)]
    }

    pub fn target_mut(&mut self) -> &mut Target {
        let i = self.active.min(self.targets.len() - 1);
        &mut self.targets[i]
    }

    pub fn target_by_id(&mut self, id: TargetId) -> Option<&mut Target> {
        self.targets.iter_mut().find(|t| t.id == id)
    }

    /// The target whose current run has this id.
    fn target_by_run(&mut self, run_id: u64) -> Option<&mut Target> {
        self.targets
            .iter_mut()
            .find(|t| t.run.as_ref().is_some_and(|r| r.id == run_id))
    }

    /// Across the targets that can validate: on how many `entry` runs (verdict ● or `!`),
    /// out of how many that have a result. `None` unless they disagree (F2).
    pub fn disagreement(&self, entry: &Entry) -> Option<(usize, usize)> {
        let (mut runs, mut done) = (0, 0);
        for t in self.targets.iter().filter(|t| t.usable()) {
            if let ValidationState::Done(v) = entry.validation(t.id) {
                done += 1;
                if matches!(v.verdict, Verdict::Ok | Verdict::NeedsUnsafe) {
                    runs += 1;
                }
            }
        }
        (0 < runs && runs < done).then_some((runs, done))
    }

    /// Validation state of `entry` on the active target.
    pub fn validation_of<'a>(&self, entry: &'a Entry) -> &'a ValidationState {
        entry.validation(self.target().id)
    }

    /// `<` `>`: through the tabs, the compare tab first while a fleet run exists.
    fn switch_target(&mut self, delta: isize) {
        let extra = usize::from(self.fleet.is_some());
        let n = (self.targets.len() + extra) as isize;
        let at = if self.compare_selected() {
            0
        } else {
            self.active + extra
        };
        let next = (at as isize + delta).rem_euclid(n) as usize;
        self.compare = extra == 1 && next == 0;
        if !self.compare {
            self.active = next - extra;
        }
        self.scroll.set(0);
    }

    /// The script under the cursor (`None` on a directory row).
    pub fn selected(&self) -> Option<&Entry> {
        match self.rows.get(self.cursor)? {
            ListRow::Script { entry, .. } => self.entries.get(*entry),
            ListRow::Dir { .. } => None,
        }
    }

    fn selected_index(&self) -> Option<usize> {
        match self.rows.get(self.cursor)? {
            ListRow::Script { entry, .. } => Some(*entry),
            ListRow::Dir { .. } => None,
        }
    }

    /// Validation states to start from: `Skipped` where a target cannot validate.
    fn fresh_validations(&self) -> HashMap<TargetId, ValidationState> {
        self.targets
            .iter()
            .filter_map(|t| match (&t.conn, &t.bpftrace) {
                (target::Conn::Lost(_), _) => {
                    Some((t.id, ValidationState::Skipped("connection lost".into())))
                }
                (_, BpftraceState::Missing(reason)) => Some((t.id, ValidationState::Skipped(reason.clone()))),
                _ => None,
            })
            .collect()
    }

    pub fn selected_row(&self) -> Option<&ListRow> {
        self.rows.get(self.cursor)
    }

    /// Whether the list currently shows the tree (tree view and no filter).
    pub fn showing_tree(&self) -> bool {
        self.tree && self.query.is_empty()
    }

    fn ids(&self) -> Vec<&str> {
        self.entries.iter().map(Entry::id).collect()
    }

    fn current_key(&self) -> Option<String> {
        let ids = self.ids();
        self.selected_row().map(|r| r.key(&ids))
    }

    pub fn context(&self) -> Context {
        match (&self.overlay, self.screen) {
            (Some(Overlay::Help), _) => Context::Help,
            (Some(Overlay::Params { .. }), _) => Context::Form,
            (Some(Overlay::Confirm(_)), _) => Context::Confirm,
            (Some(Overlay::Ask(_)), _) => Context::Ask,
            (Some(Overlay::Connect(_)), _) => Context::Connect,
            (None, _) if self.editor.is_some() => Context::Editor,
            (None, Screen::Browser) if self.filter_editing => Context::Filter,
            (None, Screen::Browser) => Context::Browser,
            (None, Screen::Run) if self.compare_selected() => Context::Compare,
            (None, Screen::Run) if self.target().log_view.editing => Context::LogFilter,
            (None, Screen::Run) => Context::Run,
        }
    }

    /// The active target's current or last run.
    pub fn active_run(&self) -> Option<&Run> {
        self.target().run.as_ref()
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
            Msg::EnvDetected {
                target,
                host,
                bpftrace,
            } => self.on_env(target, host, bpftrace),
            Msg::Validated {
                target,
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
                    entry
                        .validations
                        .insert(target, ValidationState::Done(validation));
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
            Msg::Exported(result) => {
                match result {
                    Ok(paths) if paths.len() > 2 => {
                        let dir = paths[0]
                            .parent()
                            .map(|d| d.display().to_string())
                            .unwrap_or_default();
                        let report = paths
                            .iter()
                            .find(|p| {
                                p.file_name()
                                    .is_some_and(|n| n.to_string_lossy().starts_with("bpfdeck-fleet-"))
                            })
                            .and_then(|p| p.file_name())
                            .map(|n| format!("; report: {}", n.to_string_lossy()))
                            .unwrap_or_default();
                        self.notify(
                            Level::Info,
                            format!("exported {} files to {dir}{report}", paths.len()),
                        );
                    }
                    Ok(paths) => {
                        let names: Vec<String> = paths.iter().map(|p| p.display().to_string()).collect();
                        self.notify(Level::Info, format!("exported: {}", names.join(", ")));
                    }
                    Err(e) => self.notify(Level::Error, format!("export failed: {e}")),
                }
                Vec::new()
            }
            Msg::ConnectCheck { attempt, check } => {
                self.on_connect_check(attempt, check);
                Vec::new()
            }
            Msg::ConnectNeedsAuth { attempt, message } => {
                self.on_connect_needs_auth(attempt, message);
                Vec::new()
            }
            Msg::ConnectFailed { attempt, reason } => {
                self.on_connect_failed(attempt, reason);
                Vec::new()
            }
            Msg::DraftSaved { id, hash, result } => self.on_draft_saved(&id, &hash, result),
            Msg::ConnectionLost { target, reason } => {
                self.on_connection_lost(target, reason);
                Vec::new()
            }
            Msg::Connected {
                attempt,
                target,
                dest,
                info,
                host,
                bpftrace,
            } => self.on_connected(attempt, target, dest, info, host, bpftrace),
            Msg::Tick(now) => {
                for run in self.targets.iter_mut().filter_map(|t| t.run.as_mut()) {
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
        let keep = self.current_key();
        let first_load = self.source.is_none();
        let skipped = self.fresh_validations();
        // Drafts outlive a rescan (the edits win; the original is what was scanned now).
        let mut drafts: HashMap<String, Draft> = self
            .entries
            .drain(..)
            .filter_map(|e| Some((e.id().to_string(), e.draft?)))
            .collect();
        self.entries = catalog
            .scripts
            .into_iter()
            .map(|script| {
                let draft = drafts.remove(&script.file.id).map(|d| {
                    // Same text and copy; the diff is against the freshly scanned original.
                    Draft {
                        path: d.path,
                        active: d.active,
                        ..Draft::new(&script, d.script.content)
                    }
                });
                let mut entry = Entry {
                    request: ValidationRequest::new(&script.file.path, &script.content, &script.meta),
                    script,
                    validations: skipped.clone(),
                    draft,
                };
                entry.refresh_request();
                entry
            })
            .collect();
        self.source = Some(catalog.source);
        self.load = LoadState::Ready;
        // Scripts spread over directories (like bpftrace/tools + tests) read better as a tree.
        if !self.view_chosen {
            self.tree = self.entries.iter().any(|e| e.id().contains('/'));
        }
        if first_load && self.entries.len() > COLLAPSE_ABOVE {
            self.collapsed = self
                .entries
                .iter()
                .filter_map(|e| e.id().split_once('/').map(|(top, _)| top.to_string()))
                .collect();
        }
        match catalog.warnings.as_slice() {
            [] => {}
            [one] => self.notify(Level::Warn, one.clone()),
            [first, rest @ ..] => self.notify(Level::Warn, format!("{first} (+{} more)", rest.len())),
        }
        self.refilter(keep.as_deref());
        self.validation_cmds(None)
    }

    fn on_env(
        &mut self,
        target: TargetId,
        host: SystemInfo,
        bpftrace: Result<(BpftraceInfo, Strategy), String>,
    ) -> Vec<Cmd> {
        let Some(t) = self.target_by_id(target) else {
            return Vec::new();
        };
        t.host = Some(host);
        match bpftrace {
            Ok((info, strategy)) => {
                t.bpftrace = BpftraceState::Ready { info, strategy };
                self.validation_cmds(Some(target))
            }
            Err(reason) => {
                let label = t.label.clone();
                t.bpftrace = BpftraceState::Missing(reason.clone());
                for entry in &mut self.entries {
                    if entry.validation(target) == &ValidationState::Pending {
                        entry
                            .validations
                            .insert(target, ValidationState::Skipped(reason.clone()));
                    }
                }
                self.notify(Level::Warn, format!("{label}: validation disabled: {reason}"));
                Vec::new()
            }
        }
    }

    /// Validate every pending script on every target that can take it (or only on `only`,
    /// which just became ready: the others already have theirs in flight).
    fn validation_cmds(&self, only: Option<TargetId>) -> Vec<Cmd> {
        let mut cmds = Vec::new();
        let targets = self.targets.iter().filter(|t| only.is_none_or(|id| id == t.id));
        for t in targets.filter(|t| t.usable()) {
            cmds.extend(
                self.entries
                    .iter()
                    .filter(|e| e.validation(t.id) == &ValidationState::Pending && e.run_path().is_some())
                    .map(|e| Cmd::Validate {
                        target: t.id,
                        id: e.id().to_string(),
                        request: e.request.clone(),
                    }),
            );
        }
        cmds
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
                Context::Connect => self.connect_key(key),
                Context::Editor => self.editor_key(key),
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
            Context::Connect => self.connect_action(action),
            Context::Editor => self.editor_action(action),
            Context::Run | Context::LogFilter => self.run_action(action),
            Context::Compare => self.compare_action(action),
        }
    }

    fn browser_action(&mut self, action: Action) -> Vec<Cmd> {
        match action {
            Action::Down => self.move_cursor(1),
            Action::Up => self.move_cursor(-1),
            Action::Top => self.set_cursor(0),
            Action::Bottom => self.set_cursor(self.rows.len().saturating_sub(1)),
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
                    let keep = self.current_key();
                    self.refilter(keep.as_deref());
                }
            }
            Action::Edit => return self.edit(),
            Action::Rescan => return self.rescan(),
            Action::Help => {
                self.help_scroll.set(0);
                self.overlay = Some(Overlay::Help);
            }
            Action::Run => {
                if let Some(ListRow::Dir { path, .. }) = self.selected_row() {
                    let path = path.clone();
                    self.toggle_dir(&path);
                } else {
                    return self.request_run();
                }
            }
            Action::ToggleTree => {
                self.tree = !self.tree;
                self.view_chosen = true;
                let keep = self.current_key();
                self.rebuild_rows(keep.as_deref());
            }
            Action::Collapse => self.collapse(),
            Action::Expand => self.expand(),
            Action::PrevTarget => self.switch_target(-1),
            Action::NextTarget => self.switch_target(1),
            Action::Connect => self.open_connect(),
            Action::Disconnect => return self.request_disconnect(),
            Action::EditInline => self.start_editing(),
            Action::ToggleOriginal => return self.toggle_original(),
            Action::ShowRun => {
                if self.active_run().is_some() || self.compare_selected() {
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
        let last = self.rows.len().saturating_sub(1);
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
        self.rebuild_rows(keep);
    }

    /// Recompute `rows`; keep the row with key `keep` (see `ListRow::key`) selected, or the
    /// script it belongs to, else the first row.
    fn rebuild_rows(&mut self, keep: Option<&str>) {
        // A kept script must stay reachable: open the directories above it.
        if self.showing_tree()
            && let Some(id) = keep.and_then(|k| k.strip_prefix("s:"))
        {
            let dirs: Vec<String> = id.match_indices('/').map(|(i, _)| id[..i].to_string()).collect();
            for dir in dirs {
                self.collapsed.remove(&dir);
            }
        }
        self.rows = if self.showing_tree() {
            let ids = self.ids();
            let scripts: Vec<(usize, &str)> = self.visible.iter().map(|&i| (i, ids[i])).collect();
            tree::tree_rows(&scripts, &self.collapsed)
        } else {
            tree::flat_rows(&self.visible)
        };
        let ids = self.ids();
        let pos = keep.and_then(|k| {
            self.rows.iter().position(|r| r.key(&ids) == k).or_else(|| {
                // A directory that is not shown (flat list): its first script instead.
                let dir = format!("{}/", k.strip_prefix("d:")?);
                self.rows
                    .iter()
                    .position(|r| r.key(&ids).starts_with(&format!("s:{dir}")))
            })
        });
        self.set_cursor(pos.unwrap_or(0));
    }

    fn toggle_dir(&mut self, path: &str) {
        if !self.collapsed.remove(path) {
            self.collapsed.insert(path.to_string());
        }
        self.rebuild_rows(Some(&format!("d:{path}")));
    }

    /// `←`: collapse the directory under the cursor, else jump to the parent directory.
    fn collapse(&mut self) {
        if !self.showing_tree() {
            return;
        }
        match self.selected_row() {
            Some(ListRow::Dir {
                path, expanded: true, ..
            }) => {
                let path = path.clone();
                self.toggle_dir(&path);
            }
            Some(row) => {
                let ids = self.ids();
                if let Some(parent) = tree::parent(row, &ids) {
                    let key = format!("d:{parent}");
                    if let Some(pos) = self.rows.iter().position(|r| r.key(&ids) == key) {
                        self.set_cursor(pos);
                    }
                }
            }
            None => {}
        }
    }

    /// `→`: expand the directory under the cursor, or step into an expanded one.
    fn expand(&mut self) {
        match self.selected_row() {
            Some(ListRow::Dir {
                path,
                expanded: false,
                ..
            }) => {
                let path = path.clone();
                self.toggle_dir(&path);
            }
            Some(ListRow::Dir { expanded: true, .. }) => self.move_cursor(1),
            _ => {}
        }
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
    use crate::remote::Dest;
    use crate::remote::connect::{Check, CheckStatus};
    use crate::remote::facts::RemoteInfo;
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

    /// Connect `host_name` and validate every script there: `failed` fail, the rest pass.
    pub fn connect_validated(app: &mut App, host_name: &str, failed: &[&str]) {
        let cmds = connect(app, host_name);
        let target = app.target().id;
        for cmd in cmds {
            let Cmd::Validate { id, request, .. } = cmd else {
                continue;
            };
            let verdict = if failed.contains(&id.as_str()) {
                Verdict::Failed {
                    reason: "kprobe:tcp_connect: No such file or directory".into(),
                }
            } else {
                Verdict::Ok
            };
            app.update(Msg::Validated {
                target,
                id,
                content_hash: request.content_hash,
                validation: validation(verdict, "", &[], &[]),
            });
        }
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

    pub fn type_text(app: &mut App, text: &str) {
        for c in text.chars() {
            app.update(Msg::Key(KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE)));
        }
    }

    pub fn press(app: &mut App, code: KeyCode) -> Vec<Cmd> {
        app.update(Msg::Key(KeyEvent::new(code, KeyModifiers::NONE)))
    }

    /// `c`, type `host`, Enter: the dialog is checking. Returns the attempt id.
    pub fn start_connect(app: &mut App, host: &str) -> u64 {
        press(app, KeyCode::Char('c'));
        type_text(app, host);
        match press(app, KeyCode::Enter).as_slice() {
            [Cmd::Connect { attempt, .. }] => *attempt,
            other => panic!("{other:?}"),
        }
    }

    pub fn checks() -> Vec<Check> {
        let ok = |t: &str| Check {
            status: CheckStatus::Ok,
            text: t.into(),
        };
        vec![
            ok("ssh: connected as ops · 140 ms"),
            ok("host: Rocky Linux 9.4 (Blue Onyx) · x86_64 · 5.14.0-427.13.1.el9_4.x86_64"),
            ok("shell: sh, mktemp, head, setsid"),
            ok("root: sudo -n works"),
        ]
    }

    /// Connect to `host` with every check passing; returns the commands of `Connected`.
    pub fn connect(app: &mut App, host: &str) -> Vec<Cmd> {
        let attempt = start_connect(app, host);
        let target = app.next_target_id - 1;
        for check in checks() {
            app.update(Msg::ConnectCheck { attempt, check });
        }
        app.update(Msg::Connected {
            attempt,
            target,
            dest: Dest::parse(host, "").expect("host"),
            info: RemoteInfo {
                user: "ops".into(),
                os: "Rocky Linux 9.4 (Blue Onyx)".into(),
                arch: "x86_64".into(),
                privilege: "root via sudo".into(),
                btf: true,
            },
            host: SystemInfo {
                privilege: Privilege::Root,
                lockdown: Lockdown::None,
                kernel_release: "5.14.0-427.13.1.el9_4.x86_64".into(),
            },
            bpftrace: bpftrace(),
        })
    }

    /// Loaded, bpftrace detected, every script validated except `shebang_no_ext` (pending).
    pub fn ready_app(host: SystemInfo) -> App {
        let mut app = App::new("/srv/bpf".into());
        app.init();
        app.update(Msg::EnvDetected {
            target: LOCAL,
            host,
            bpftrace: Ok(bpftrace()),
        });
        let cmds = app.update(Msg::Loaded(Ok(catalog())));
        for cmd in cmds {
            let Cmd::Validate { id, request, .. } = cmd else {
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
                target: LOCAL,
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
                Cmd::DetectEnv { target: LOCAL },
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
            target: LOCAL,
            host: host(Privilege::Root, Lockdown::None),
            bpftrace: Ok(bpftrace()),
        });
        assert_eq!(validate_ids(&cmds).len(), 8);

        // bpftrace first, then scripts.
        let mut app = App::new("x".into());
        app.init();
        let cmds = app.update(Msg::EnvDetected {
            target: LOCAL,
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
            target: LOCAL,
            host: host(Privilege::None, Lockdown::Unknown),
            bpftrace: Err("cannot run bpftrace: not found".into()),
        });
        assert!(cmds.is_empty());
        assert!(
            app.entries
                .iter()
                .all(|e| matches!(e.validation(LOCAL), ValidationState::Skipped(_)))
        );
        assert_eq!(app.notice.as_ref().map(|n| n.level), Some(Level::Warn));
        // A rescan keeps them skipped and asks for nothing.
        assert!(validate_ids(&app.update(Msg::Loaded(Ok(catalog())))).is_empty());
    }

    #[test]
    fn stale_validation_results_are_ignored() {
        let mut app = ready_app(host(Privilege::Root, Lockdown::None));
        app.update(Msg::Validated {
            target: LOCAL,
            id: "shebang_no_ext".into(),
            content_hash: "not-the-current-content".into(),
            validation: ok_validation(),
        });
        let after = app
            .entries
            .iter()
            .find(|e| e.id() == "shebang_no_ext")
            .expect("entry");
        assert_eq!(after.validation(LOCAL), &ValidationState::Pending);
    }

    #[test]
    fn navigation_and_tabs() {
        let mut app = ready_app(host(Privilege::Root, Lockdown::None));
        assert_eq!(app.selected().map(Entry::id), Some("missing_probe_demo.bt"));
        app.update(ch('j'));
        app.update(key(KeyCode::Down));
        // Tree view: the `net/` directory row sits between them.
        assert_eq!(app.selected().map(Entry::id), Some("net/tcpconnect_demo.bt"));
        app.update(ch('j'));
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

    fn row_keys(app: &App) -> Vec<String> {
        let ids: Vec<&str> = app.entries.iter().map(Entry::id).collect();
        app.rows.iter().map(|r| r.key(&ids)).collect()
    }

    #[test]
    fn tree_view_is_the_default_with_subdirectories() {
        let app = ready_app(host(Privilege::Root, Lockdown::None));
        assert!(app.tree && app.showing_tree());
        assert_eq!(
            row_keys(&app)[..3],
            ["s:missing_probe_demo.bt", "d:net", "s:net/tcpconnect_demo.bt"]
        );

        // A flat catalog stays flat.
        let mut flat = catalog();
        flat.scripts.retain(|s| !s.file.id.contains('/'));
        let mut app = App::new("x".into());
        app.init();
        app.update(Msg::Loaded(Ok(flat)));
        assert!(!app.tree);
    }

    #[test]
    fn tree_navigation_collapse_and_expand() {
        let mut app = ready_app(host(Privilege::Root, Lockdown::None));
        app.update(ch('j')); // on `net/`
        assert!(app.selected().is_none(), "a directory row has no script");
        assert!(
            app.update(key(KeyCode::Enter)).is_empty(),
            "Enter on a directory toggles it"
        );
        assert_eq!(app.rows.len(), 8, "net/ collapsed: 7 top-level scripts + the dir");
        assert_eq!(app.update(ch('e')), vec![], "no editor for a directory");
        app.update(key(KeyCode::Right));
        assert_eq!(app.rows.len(), 9);
        app.update(key(KeyCode::Right)); // step into the expanded dir
        assert_eq!(app.selected().map(Entry::id), Some("net/tcpconnect_demo.bt"));
        app.update(ch('h')); // on a script: go to its directory
        assert_eq!(row_keys(&app)[app.cursor], "d:net");
        app.update(key(KeyCode::Left)); // on an expanded dir: collapse it
        assert!(app.collapsed.contains("net"));
        assert_eq!(row_keys(&app)[app.cursor], "d:net", "selection stays on the dir");

        // Rescans keep the collapsed state and the view.
        app.update(Msg::Loaded(Ok(catalog())));
        assert!(app.collapsed.contains("net") && app.tree);

        // `t` switches to the flat list and back, keeping the selection.
        app.update(ch('j'));
        let before = app.selected().map(|e| e.id().to_string());
        app.update(ch('t'));
        assert!(!app.tree);
        app.update(ch('t'));
        app.update(ch('t'));
        assert_eq!(app.rows.len(), 8);
        assert_eq!(app.selected().map(|e| e.id().to_string()), before);
        app.update(Msg::Loaded(Ok(catalog())));
        assert!(!app.tree, "an explicit choice survives rescans");
    }

    #[test]
    fn large_trees_start_collapsed() {
        let mut big = catalog();
        let template = big.scripts[0].clone();
        for i in 0..40 {
            let mut s = template.clone();
            s.file.id = format!("tools/t{i:02}.bt");
            big.scripts.push(s);
        }
        big.scripts.sort_by(|a, b| a.file.id.cmp(&b.file.id));
        let mut app = App::new("x".into());
        app.init();
        app.update(Msg::Loaded(Ok(big.clone())));
        let keys = row_keys(&app);
        assert!(keys.contains(&"d:tools".to_string()) && keys.contains(&"d:net".to_string()));
        assert_eq!(keys.len(), 9, "7 root scripts + net/ + tools/, both collapsed");
        // Only the first load decides; a rescan keeps what the user opened.
        app.collapsed.clear();
        app.update(Msg::Loaded(Ok(big)));
        assert_eq!(row_keys(&app).len(), 50);
    }

    #[test]
    fn flat_list_from_a_directory_row_selects_its_first_script() {
        let mut app = ready_app(host(Privilege::Root, Lockdown::None));
        app.update(ch('j'));
        app.update(key(KeyCode::Left)); // net/ collapsed, cursor on it
        app.update(ch('t'));
        assert_eq!(app.selected().map(Entry::id), Some("net/tcpconnect_demo.bt"));
    }

    #[test]
    fn filter_is_flat_and_clearing_it_reveals_the_match() {
        let mut app = ready_app(host(Privilege::Root, Lockdown::None));
        app.collapsed.insert("net".into());
        app.update(ch('t'));
        app.update(ch('t')); // rebuild with net/ collapsed
        assert!(!row_keys(&app).contains(&"s:net/tcpconnect_demo.bt".to_string()));
        app.update(ch('/'));
        for c in "tcpconn".chars() {
            app.update(ch(c));
        }
        assert!(!app.showing_tree());
        assert_eq!(app.selected().map(Entry::id), Some("net/tcpconnect_demo.bt"));
        app.update(key(KeyCode::Esc));
        assert!(app.showing_tree());
        assert_eq!(
            app.selected().map(Entry::id),
            Some("net/tcpconnect_demo.bt"),
            "its dir was opened"
        );
        assert!(!app.collapsed.contains("net"));
    }
}
