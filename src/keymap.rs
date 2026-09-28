//! Key → action table per context. Single source of truth for the handlers, the help
//! modal and the status bar hints (spec §5.6, §9).

use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyModifiers};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Context {
    /// Script list + detail pane.
    Browser,
    /// Typing a filter query (printable keys go to the query, see [`lookup`]).
    Filter,
    Help,
}

impl Context {
    pub fn title(self) -> &'static str {
        match self {
            Self::Browser => "Browser",
            Self::Filter => "Filter",
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
    CloseHelp,
    Quit,
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

use Context::{Browser, Filter, Help};

pub const BINDINGS: &[Binding] = &[
    bind(
        Browser,
        &[ch('j'), key(KeyCode::Down)],
        "j/↓",
        Action::Down,
        "next script",
        None,
    ),
    bind(
        Browser,
        &[ch('k'), key(KeyCode::Up)],
        "k/↑",
        Action::Up,
        "previous script",
        None,
    ),
    bind(
        Browser,
        &[ch('g'), key(KeyCode::Home)],
        "g/Home",
        Action::Top,
        "first script",
        None,
    ),
    bind(
        Browser,
        &[ch('G'), key(KeyCode::End)],
        "G/End",
        Action::Bottom,
        "last script",
        None,
    ),
    bind(
        Browser,
        &[key(KeyCode::Tab)],
        "Tab",
        Action::NextTab,
        "next detail tab",
        None,
    ),
    bind(
        Browser,
        &[key(KeyCode::BackTab)],
        "S-Tab",
        Action::PrevTab,
        "previous detail tab",
        None,
    ),
    bind(Browser, &[ch('1')], "1", Action::Tab(0), "Info tab", None),
    bind(Browser, &[ch('2')], "2", Action::Tab(1), "Source tab", None),
    bind(Browser, &[ch('3')], "3", Action::Tab(2), "Validation tab", None),
    bind(
        Browser,
        &[key(KeyCode::PageDown), ctrl('d')],
        "PgDn/C-d",
        Action::ScrollDown,
        "scroll detail down",
        None,
    ),
    bind(
        Browser,
        &[key(KeyCode::PageUp), ctrl('u')],
        "PgUp/C-u",
        Action::ScrollUp,
        "scroll detail up",
        None,
    ),
    bind(
        Browser,
        &[ch('/')],
        "/",
        Action::OpenFilter,
        "filter by name/description",
        Some("filter"),
    ),
    bind(
        Browser,
        &[key(KeyCode::Esc)],
        "Esc",
        Action::ClearFilter,
        "clear filter",
        None,
    ),
    bind(
        Browser,
        &[ch('e')],
        "e",
        Action::Edit,
        "open in $EDITOR",
        Some("edit"),
    ),
    bind(
        Browser,
        &[ch('r')],
        "r",
        Action::Rescan,
        "rescan + revalidate",
        Some("rescan"),
    ),
    bind(Browser, &[ch('?')], "?", Action::Help, "help", Some("help")),
    bind(
        Browser,
        &[ch('q'), ctrl('c')],
        "q",
        Action::Quit,
        "quit",
        Some("quit"),
    ),
    bind(
        Filter,
        &[key(KeyCode::Enter)],
        "Enter",
        Action::AcceptFilter,
        "keep filter",
        Some("keep"),
    ),
    bind(
        Filter,
        &[key(KeyCode::Esc)],
        "Esc",
        Action::ClearFilter,
        "clear filter",
        Some("clear"),
    ),
    bind(
        Filter,
        &[key(KeyCode::Down)],
        "↓",
        Action::Down,
        "next match",
        Some("next"),
    ),
    bind(
        Filter,
        &[key(KeyCode::Up)],
        "↑",
        Action::Up,
        "previous match",
        Some("prev"),
    ),
    bind(Filter, &[ctrl('c')], "C-c", Action::Quit, "quit", None),
    bind(
        Help,
        &[key(KeyCode::Esc), ch('?'), ch('q')],
        "Esc/?/q",
        Action::CloseHelp,
        "close help",
        Some("close"),
    ),
    bind(Help, &[ctrl('c')], "C-c", Action::Quit, "quit", None),
];

/// The action bound to `event` in `context`. Key releases/repeats-as-release are ignored.
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
        assert_eq!(lookup(Browser, &ev(KeyCode::Char('j'), none)), Some(Action::Down));
        assert_eq!(lookup(Browser, &ev(KeyCode::Down, none)), Some(Action::Down));
        assert_eq!(
            lookup(Browser, &ev(KeyCode::Char('G'), KeyModifiers::SHIFT)),
            Some(Action::Bottom)
        );
        assert_eq!(
            lookup(Browser, &ev(KeyCode::Char('G'), none)),
            Some(Action::Bottom)
        );
        assert_eq!(
            lookup(Browser, &ev(KeyCode::Char('c'), KeyModifiers::CONTROL)),
            Some(Action::Quit)
        );
        assert_eq!(lookup(Browser, &ev(KeyCode::Char('c'), none)), None);
        assert_eq!(
            lookup(Browser, &ev(KeyCode::Char('3'), none)),
            Some(Action::Tab(2))
        );
        assert_eq!(
            lookup(Filter, &ev(KeyCode::Char('q'), none)),
            None,
            "q is text while filtering"
        );
        assert_eq!(
            lookup(Help, &ev(KeyCode::Char('q'), none)),
            Some(Action::CloseHelp)
        );
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
    fn every_context_can_quit_or_close() {
        for ctx in [Browser, Filter, Help] {
            assert!(bindings(ctx).any(|b| b.action == Action::Quit), "{ctx:?}");
            assert!(hints(ctx).next().is_some(), "{ctx:?} has no hints");
        }
    }
}
