//! Key → action table per context. Single source of truth for the handlers, the help
//! modal and the status bar hints (spec §5.6, §9).

use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyModifiers};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Context {
    /// Script list + detail pane.
    Browser,
    /// Typing a filter query (unbound printable keys go to the query).
    Filter,
    /// Parameters form (unbound printable keys go to the focused field).
    Form,
    /// Run confirmation dialog.
    Confirm,
    /// Yes/no question.
    Ask,
    /// Run view: header + event log.
    Run,
    /// Typing a log filter.
    LogFilter,
    Help,
}

impl Context {
    pub const ALL: [Context; 8] = [
        Self::Browser,
        Self::Filter,
        Self::Form,
        Self::Confirm,
        Self::Ask,
        Self::Run,
        Self::LogFilter,
        Self::Help,
    ];

    pub fn title(self) -> &'static str {
        match self {
            Self::Browser => "Browser",
            Self::Filter => "Filter",
            Self::Form => "Parameters",
            Self::Confirm => "Run confirmation",
            Self::Ask => "Question",
            Self::Run => "Run view",
            Self::LogFilter => "Log filter",
            Self::Help => "Help",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Action {
    Down,
    Up,
    Top,
    Bottom,
    NextTab,
    PrevTab,
    Tab(usize),
    ScrollDown,
    ScrollUp,
    OpenFilter,
    ClearFilter,
    AcceptFilter,
    Edit,
    Rescan,
    Help,
    Close,
    Quit,
    /// Start the selected script (params form → confirmation), or show its active run.
    Run,
    ShowRun,
    NextField,
    PrevField,
    Submit,
    ToggleUnsafe,
    Yes,
    No,
    Stop,
    ToggleFollow,
    ToggleFullWidth,
    PrevKey,
    NextKey,
    ToggleSort,
    Export,
    ToggleTree,
    Collapse,
    Expand,
    PrevTarget,
    NextTarget,
}

#[derive(Debug, Clone, Copy)]
pub struct Key {
    pub code: KeyCode,
    pub mods: KeyModifiers,
}

const fn key(code: KeyCode) -> Key {
    Key {
        code,
        mods: KeyModifiers::NONE,
    }
}

const fn ch(c: char) -> Key {
    key(KeyCode::Char(c))
}

const fn ctrl(c: char) -> Key {
    Key {
        code: KeyCode::Char(c),
        mods: KeyModifiers::CONTROL,
    }
}

pub struct Binding {
    pub context: Context,
    pub keys: &'static [Key],
    /// How the keys are shown in help and hints.
    pub label: &'static str,
    pub action: Action,
    pub help: &'static str,
    /// Short text for the status bar; `None` = not shown there.
    pub hint: Option<&'static str>,
}

const fn bind(
    context: Context,
    keys: &'static [Key],
    label: &'static str,
    action: Action,
    help: &'static str,
    hint: Option<&'static str>,
) -> Binding {
    Binding {
        context,
        keys,
        label,
        action,
        help,
        hint,
    }
}

use Context::{Ask, Browser, Confirm, Filter, Form, Help, LogFilter, Run};
use KeyCode::{BackTab, Down, End, Enter, Esc, Home, Left, PageDown, PageUp, Right, Tab, Up};

#[rustfmt::skip]
pub const BINDINGS: &[Binding] = &[
    bind(Browser, &[ch('j'), key(Down)], "j/↓", Action::Down, "next script", None),
    bind(Browser, &[ch('k'), key(Up)], "k/↑", Action::Up, "previous script", None),
    bind(Browser, &[ch('g'), key(Home)], "g/Home", Action::Top, "first script", None),
    bind(Browser, &[ch('G'), key(End)], "G/End", Action::Bottom, "last script", None),
    bind(Browser, &[key(Enter)], "Enter", Action::Run, "run script / open dir", Some("run")),
    bind(Browser, &[ch('o')], "o", Action::ShowRun, "show last run output", None),
    bind(Browser, &[ch('t')], "t", Action::ToggleTree, "tree / flat list", None),
    bind(Browser, &[ch('<')], "<", Action::PrevTarget, "previous target tab", None),
    bind(Browser, &[ch('>')], ">", Action::NextTarget, "next target tab", None),
    bind(Browser, &[ch('h'), key(Left)], "h/←", Action::Collapse, "collapse dir / go to parent", None),
    bind(Browser, &[ch('l'), key(Right)], "l/→", Action::Expand, "expand dir", None),
    bind(Browser, &[key(Tab)], "Tab", Action::NextTab, "next detail tab", None),
    bind(Browser, &[key(BackTab)], "S-Tab", Action::PrevTab, "previous detail tab", None),
    bind(Browser, &[ch('1')], "1", Action::Tab(0), "Info tab", None),
    bind(Browser, &[ch('2')], "2", Action::Tab(1), "Source tab", None),
    bind(Browser, &[ch('3')], "3", Action::Tab(2), "Validation tab", None),
    bind(Browser, &[key(PageDown), ctrl('d')], "PgDn/C-d", Action::ScrollDown, "scroll detail down", None),
    bind(Browser, &[key(PageUp), ctrl('u')], "PgUp/C-u", Action::ScrollUp, "scroll detail up", None),
    bind(Browser, &[ch('/')], "/", Action::OpenFilter, "filter by name/description", Some("filter")),
    bind(Browser, &[key(Esc)], "Esc", Action::ClearFilter, "clear filter", None),
    bind(Browser, &[ch('e')], "e", Action::Edit, "open in $EDITOR", None),
    bind(Browser, &[ch('r')], "r", Action::Rescan, "rescan + revalidate", None),
    bind(Browser, &[ch('?')], "?", Action::Help, "help", Some("help")),
    bind(Browser, &[ch('q'), ctrl('c')], "q", Action::Quit, "quit (asks if running)", Some("quit")),

    bind(Filter, &[key(Enter)], "Enter", Action::AcceptFilter, "keep filter", Some("keep")),
    bind(Filter, &[key(Esc)], "Esc", Action::ClearFilter, "clear filter", Some("clear")),
    bind(Filter, &[key(Down)], "↓", Action::Down, "next match", Some("next")),
    bind(Filter, &[key(Up)], "↑", Action::Up, "previous match", Some("prev")),

    bind(Form, &[key(Tab), key(Down)], "Tab/↓", Action::NextField, "next field", Some("next")),
    bind(Form, &[key(BackTab), key(Up)], "S-Tab/↑", Action::PrevField, "previous field", None),
    bind(Form, &[key(Enter)], "Enter", Action::Submit, "continue to confirmation", Some("continue")),
    bind(Form, &[key(Esc)], "Esc", Action::Close, "cancel", Some("cancel")),

    bind(Confirm, &[key(Enter)], "Enter", Action::Submit, "run it", Some("run")),
    bind(Confirm, &[ch('u')], "u", Action::ToggleUnsafe, "toggle --unsafe (if needed)", None),
    bind(Confirm, &[key(Esc), ch('q')], "Esc", Action::Close, "cancel", Some("cancel")),

    bind(Ask, &[ch('y')], "y", Action::Yes, "yes", Some("yes")),
    bind(Ask, &[ch('n'), key(Esc)], "n/Esc", Action::No, "no", Some("no")),

    bind(Run, &[ch('x'), ctrl('c')], "x/C-c", Action::Stop, "stop (SIGINT to bpftrace)", Some("stop")),
    bind(Run, &[ch('p')], "p", Action::ToggleFollow, "pause/follow the log", Some("pause")),
    bind(Run, &[ch('/')], "/", Action::OpenFilter, "filter the log", Some("filter")),
    bind(Run, &[ch('j'), key(Down)], "j/↓", Action::Down, "scroll log down", None),
    bind(Run, &[ch('k'), key(Up)], "k/↑", Action::Up, "scroll log up (pauses)", None),
    bind(Run, &[key(PageDown), ctrl('d')], "PgDn/C-d", Action::ScrollDown, "page down", None),
    bind(Run, &[key(PageUp), ctrl('u')], "PgUp/C-u", Action::ScrollUp, "page up (pauses)", None),
    bind(Run, &[ch('g'), key(Home)], "g/Home", Action::Top, "oldest line (pauses)", None),
    bind(Run, &[ch('G'), key(End)], "G/End", Action::Bottom, "newest line, follow", None),
    bind(Run, &[ch('z')], "z", Action::ToggleFullWidth, "maximize results", None),
    bind(Run, &[ch('<')], "<", Action::PrevTarget, "previous target tab", None),
    bind(Run, &[ch('>')], ">", Action::NextTarget, "next target tab", None),
    bind(Run, &[key(Tab)], "Tab", Action::NextTab, "next panel", Some("panel")),
    bind(Run, &[key(BackTab)], "S-Tab", Action::PrevTab, "previous panel", None),
    bind(Run, &[ch('[')], "[", Action::PrevKey, "previous key (keyed hist)", None),
    bind(Run, &[ch(']')], "]", Action::NextKey, "next key (keyed hist)", None),
    bind(Run, &[ch('s')], "s", Action::ToggleSort, "table: sort by key/value", None),
    bind(Run, &[ch('w')], "w", Action::Export, "write run to files (.txt + .ndjson)", None),
    bind(Run, &[key(Esc)], "Esc", Action::Close, "back to list, run continues", Some("back")),
    bind(Run, &[ch('?')], "?", Action::Help, "help", Some("help")),

    bind(LogFilter, &[key(Enter)], "Enter", Action::AcceptFilter, "keep filter", Some("keep")),
    bind(LogFilter, &[key(Esc)], "Esc", Action::ClearFilter, "clear filter", Some("clear")),

    bind(Help, &[key(Esc), ch('?'), ch('q')], "Esc/?/q", Action::Close, "close help", Some("close")),
    bind(Help, &[ch('j'), key(Down)], "j/↓", Action::ScrollDown, "scroll", None),
    bind(Help, &[ch('k'), key(Up)], "k/↑", Action::ScrollUp, "scroll back", None),
];

/// The action bound to `event` in `context`. Key releases are ignored.
pub fn lookup(context: Context, event: &KeyEvent) -> Option<Action> {
    if event.kind == KeyEventKind::Release {
        return None;
    }
    BINDINGS
        .iter()
        .filter(|b| b.context == context)
        .find(|b| b.keys.iter().any(|k| matches(k, event)))
        .map(|b| b.action)
}

pub fn bindings(context: Context) -> impl Iterator<Item = &'static Binding> {
    BINDINGS.iter().filter(move |b| b.context == context)
}

/// `(keys, short text)` for the status bar.
pub fn hints(context: Context) -> impl Iterator<Item = (&'static str, &'static str)> {
    bindings(context).filter_map(|b| Some((b.label, b.hint?)))
}

/// Terminals report `G` as `Char('G')` with or without SHIFT; ignore SHIFT on chars.
fn matches(key: &Key, event: &KeyEvent) -> bool {
    if key.code != event.code {
        return false;
    }
    let mods = match event.code {
        KeyCode::Char(_) => event.modifiers - KeyModifiers::SHIFT,
        _ => event.modifiers,
    };
    mods == key.mods
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ev(code: KeyCode, mods: KeyModifiers) -> KeyEvent {
        KeyEvent::new(code, mods)
    }

    #[test]
    fn lookups() {
        let none = KeyModifiers::NONE;
        let shift = KeyModifiers::SHIFT;
        let ctrl = KeyModifiers::CONTROL;
        assert_eq!(lookup(Browser, &ev(KeyCode::Char('j'), none)), Some(Action::Down));
        assert_eq!(lookup(Browser, &ev(Down, none)), Some(Action::Down));
        assert_eq!(
            lookup(Browser, &ev(KeyCode::Char('G'), shift)),
            Some(Action::Bottom)
        );
        assert_eq!(
            lookup(Browser, &ev(KeyCode::Char('G'), none)),
            Some(Action::Bottom)
        );
        assert_eq!(lookup(Browser, &ev(KeyCode::Char('c'), ctrl)), Some(Action::Quit));
        assert_eq!(lookup(Browser, &ev(KeyCode::Char('c'), none)), None);
        assert_eq!(
            lookup(Browser, &ev(KeyCode::Char('3'), none)),
            Some(Action::Tab(2))
        );
        assert_eq!(lookup(Browser, &ev(Enter, none)), Some(Action::Run));
        assert_eq!(
            lookup(Filter, &ev(KeyCode::Char('q'), none)),
            None,
            "q is text while filtering"
        );
        assert_eq!(
            lookup(Form, &ev(KeyCode::Char(' '), none)),
            None,
            "space is text in a form"
        );
        assert_eq!(lookup(Help, &ev(KeyCode::Char('q'), none)), Some(Action::Close));
        // Ctrl-C stops the run in the run view, it never quits bpfdeck there.
        assert_eq!(lookup(Run, &ev(KeyCode::Char('c'), ctrl)), Some(Action::Stop));
        assert_eq!(lookup(Run, &ev(KeyCode::Char('q'), none)), None);
        let mut release = ev(KeyCode::Char('j'), none);
        release.kind = KeyEventKind::Release;
        assert_eq!(lookup(Browser, &release), None);
    }

    #[test]
    fn no_key_is_bound_twice_in_a_context() {
        for (i, a) in BINDINGS.iter().enumerate() {
            for b in &BINDINGS[i + 1..] {
                if a.context != b.context {
                    continue;
                }
                for ka in a.keys {
                    for kb in b.keys {
                        assert!(
                            !(ka.code == kb.code && ka.mods == kb.mods),
                            "{:?} bound to both {:?} and {:?}",
                            ka.code,
                            a.action,
                            b.action
                        );
                    }
                }
            }
        }
    }

    #[test]
    fn every_context_has_a_way_out_and_hints() {
        for ctx in Context::ALL {
            let exits = [
                Action::Quit,
                Action::Close,
                Action::ClearFilter,
                Action::No,
                Action::AcceptFilter,
            ];
            assert!(bindings(ctx).any(|b| exits.contains(&b.action)), "{ctx:?}");
            assert!(hints(ctx).next().is_some(), "{ctx:?} has no hints");
        }
    }
}
