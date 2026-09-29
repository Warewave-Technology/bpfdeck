//! Inline editing (D-025): `i` edits the shown version of a script in the Source tab, `Esc`
//! keeps the result as a draft, `u` switches between the original and the draft. The
//! source file is never written; the executor keeps a private copy for bpftrace to read.

use std::cell::Cell;
use std::path::PathBuf;

use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

use super::{App, Level, Screen};
use crate::bpftrace::validate::ValidationRequest;
use crate::catalog::Script;
use crate::keymap::Action;
use crate::model::editor::TextBuffer;
use crate::msg::Cmd;

/// Source tab index.
const SOURCE_TAB: usize = 1;
/// Lines moved by PgUp/PgDn in the editor.
const PAGE: isize = 20;

/// An edited version of a script, kept for the session.
#[derive(Debug, Clone)]
pub struct Draft {
    pub script: Script,
    /// sha256 of the draft content (as in `ValidationRequest`).
    pub hash: String,
    /// The private copy bpftrace reads; `None` until the executor has written it.
    pub path: Option<PathBuf>,
    /// Shown, validated and run instead of the original.
    pub active: bool,
}

/// The editor on the Source tab. `top`/`left` are the scroll offsets, kept by the renderer
/// so the cursor stays visible.
#[derive(Debug)]
pub struct Editor {
    pub id: String,
    pub buffer: TextBuffer,
    pub top: Cell<usize>,
    pub left: Cell<usize>,
}

impl App {
    /// `i`: edit the selected script (its shown version) in the Source tab.
    pub(super) fn start_editing(&mut self) {
        let Some(entry) = self.selected() else {
            return;
        };
        let editor = Editor {
            id: entry.id().to_string(),
            buffer: TextBuffer::new(&entry.shown().content),
            top: Cell::new(0),
            left: Cell::new(0),
        };
        self.editor = Some(editor);
        self.screen = Screen::Browser;
        self.filter_editing = false;
        self.tab = SOURCE_TAB;
    }

    /// Keys typed into the editor (everything not bound in the Editor context).
    pub(super) fn editor_key(&mut self, key: KeyEvent) {
        let Some(ed) = &mut self.editor else {
            return;
        };
        let b = &mut ed.buffer;
        match key.code {
            KeyCode::Char(c)
                if !key
                    .modifiers
                    .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT) =>
            {
                b.insert(c)
            }
            KeyCode::Enter => b.newline(),
            KeyCode::Tab => b.indent(),
            KeyCode::Backspace => b.backspace(),
            KeyCode::Delete => b.delete(),
            KeyCode::Left => b.left(),
            KeyCode::Right => b.right(),
            KeyCode::Up => b.vertical(-1),
            KeyCode::Down => b.vertical(1),
            KeyCode::PageUp => b.vertical(-PAGE),
            KeyCode::PageDown => b.vertical(PAGE),
            KeyCode::Home => b.home(),
            KeyCode::End => b.end(),
            _ => {}
        }
    }

    pub(super) fn editor_action(&mut self, action: Action) -> Vec<Cmd> {
        match action {
            Action::Close => return self.finish_editing(),
            Action::Undo => {
                if let Some(ed) = &mut self.editor
                    && !ed.buffer.undo()
                {
                    self.notify(Level::Info, "nothing to undo".into());
                }
            }
            _ => {}
        }
        Vec::new()
    }

    /// `Esc` in the editor: keep the text as the script's draft (or drop the draft when
    /// the text is the original again).
    fn finish_editing(&mut self) -> Vec<Cmd> {
        let Some(ed) = self.editor.take() else {
            return Vec::new();
        };
        let text = ed.buffer.text();
        let Some(i) = self.entries.iter().position(|e| e.id() == ed.id) else {
            return Vec::new();
        };
        let entry = &mut self.entries[i];
        if text == entry.shown().content {
            self.notify(Level::Info, "no changes".into());
            return Vec::new();
        }
        if text == entry.script.content {
            entry.draft = None;
            self.notify(Level::Info, format!("{}: same as the original again", ed.id));
            return self.revalidate(i);
        }
        let script = Script::new(entry.script.file.clone(), text.clone());
        let hash = ValidationRequest::new(&script.file.path, &script.content, &script.meta).content_hash;
        entry.draft = Some(Draft {
            script,
            hash: hash.clone(),
            path: None,
            active: true,
        });
        let cmds = self.revalidate(i);
        self.notify(
            Level::Info,
            format!(
                "{}: edited; validation and runs use your version (u: original)",
                ed.id
            ),
        );
        let mut all = vec![Cmd::SaveDraft {
            id: ed.id,
            hash,
            content: text,
        }];
        all.extend(cmds);
        all
    }

    /// `u`: switch the selected script between the original and its draft.
    pub(super) fn toggle_original(&mut self) -> Vec<Cmd> {
        let Some(i) = self.selected_index() else {
            return Vec::new();
        };
        let entry = &mut self.entries[i];
        let id = entry.id().to_string();
        let Some(draft) = &mut entry.draft else {
            self.notify(Level::Info, format!("{id} has no edits (i edits it here)"));
            return Vec::new();
        };
        draft.active = !draft.active;
        let text = if draft.active {
            "your edited version (u: original)"
        } else {
            "the original (u: your edits)"
        };
        self.notify(Level::Info, format!("{id}: {text}"));
        self.scroll.set(0);
        self.revalidate(i)
    }

    pub(super) fn on_draft_saved(
        &mut self,
        id: &str,
        hash: &str,
        result: Result<PathBuf, String>,
    ) -> Vec<Cmd> {
        let Some(i) = self
            .entries
            .iter()
            .position(|e| e.id() == id && e.draft.as_ref().is_some_and(|d| d.hash == hash))
        else {
            return Vec::new();
        };
        match result {
            Ok(path) => {
                if let Some(d) = &mut self.entries[i].draft {
                    d.path = Some(path);
                }
                self.revalidate(i)
            }
            Err(e) => {
                self.notify(
                    Level::Error,
                    format!("{id}: cannot keep a copy of the edits: {e}"),
                );
                Vec::new()
            }
        }
    }

    /// The shown version changed: new request, fresh validation on every usable target.
    fn revalidate(&mut self, i: usize) -> Vec<Cmd> {
        let fresh = self.fresh_validations();
        let entry = &mut self.entries[i];
        entry.refresh_request();
        entry.validations = fresh;
        // A draft is validated once its copy exists (`on_draft_saved`).
        if entry.run_path().is_none() {
            return Vec::new();
        }
        let (id, request) = (entry.id().to_string(), entry.request.clone());
        self.targets
            .iter()
            .filter(|t| t.usable())
            .map(|t| Cmd::Validate {
                target: t.id,
                id: id.clone(),
                request: request.clone(),
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use ratatui::crossterm::event::KeyCode;

    use super::*;
    use crate::app::ValidationState;
    use crate::app::fixtures::*;
    use crate::app::target::LOCAL;
    use crate::keymap::Context;
    use crate::msg::Msg;
    use crate::sys::{Lockdown, Privilege};
    use pretty_assertions::assert_eq;

    fn ready() -> App {
        ready_app(host(Privilege::Root, Lockdown::None))
    }

    fn select(app: &mut App, id: &str) {
        press(app, KeyCode::Char('/'));
        type_text(app, id);
        press(app, KeyCode::Enter);
        assert_eq!(app.selected().map(|e| e.id()), Some(id));
    }

    fn ctrl(app: &mut App, c: char) -> Vec<Cmd> {
        app.update(Msg::Key(KeyEvent::new(KeyCode::Char(c), KeyModifiers::CONTROL)))
    }

    #[test]
    fn edit_keep_revert_and_restore() {
        let mut app = ready();
        select(&mut app, "syscount_demo.bt");
        let original = app.selected().expect("entry").script.content.clone();
        press(&mut app, KeyCode::Char('i'));
        assert_eq!((app.context(), app.tab), (Context::Editor, SOURCE_TAB));
        // `q`, `u`, `d` are text here.
        type_text(&mut app, "// qud");
        press(&mut app, KeyCode::Enter);
        let cmds = press(&mut app, KeyCode::Esc);
        let entry = app.selected().expect("entry");
        let edited = format!("// qud\n{original}");
        assert_eq!(entry.shown().content, edited);
        assert_eq!(entry.script.content, original, "the original stays");
        let hash = match cmds.as_slice() {
            [Cmd::SaveDraft { id, hash, content }] if id == "syscount_demo.bt" && *content == edited => {
                hash.clone()
            }
            other => panic!("{other:?}"),
        };
        assert_eq!(entry.validation(LOCAL), &ValidationState::Pending);
        assert!(entry.run_path().is_none(), "not runnable before the copy exists");

        // The copy is written: validation of the edited version starts.
        let cmds = app.update(Msg::DraftSaved {
            id: "syscount_demo.bt".into(),
            hash: hash.clone(),
            result: Ok("/tmp/drafts/1-syscount_demo.bt".into()),
        });
        assert!(
            matches!(cmds.as_slice(), [Cmd::Validate { target: LOCAL, request, .. }]
                if request.path == std::path::Path::new("/tmp/drafts/1-syscount_demo.bt") && request.content_hash == hash),
            "{cmds:?}"
        );

        // u: original (validated from its own file), u again: the edits are back.
        let cmds = press(&mut app, KeyCode::Char('u'));
        let entry = app.selected().expect("entry");
        assert_eq!(entry.shown().content, original);
        assert!(
            matches!(cmds.as_slice(), [Cmd::Validate { request, .. }] if request.path == entry.script.file.path)
        );
        press(&mut app, KeyCode::Char('u'));
        assert_eq!(app.selected().expect("entry").shown().content, edited);

        // Editing back to the original drops the draft.
        press(&mut app, KeyCode::Char('i'));
        for _ in 0..7 {
            press(&mut app, KeyCode::Delete);
        }
        let cmds = press(&mut app, KeyCode::Esc);
        assert!(app.selected().expect("entry").draft.is_none());
        assert_eq!(cmds.len(), 1, "revalidated: {cmds:?}");
    }

    #[test]
    fn undo_no_change_and_stale_saves() {
        let mut app = ready();
        select(&mut app, "syscount_demo.bt");
        press(&mut app, KeyCode::Char('u'));
        assert!(
            app.notice
                .as_ref()
                .is_some_and(|n| n.text.contains("has no edits"))
        );
        press(&mut app, KeyCode::Char('i'));
        type_text(&mut app, "x");
        ctrl(&mut app, 'z');
        assert!(press(&mut app, KeyCode::Esc).is_empty());
        assert!(app.notice.as_ref().is_some_and(|n| n.text == "no changes"));
        assert!(app.selected().expect("entry").draft.is_none());
        // A save for content that is no longer the draft is ignored.
        assert!(
            app.update(Msg::DraftSaved {
                id: "syscount_demo.bt".into(),
                hash: "old".into(),
                result: Ok("/tmp/x".into()),
            })
            .is_empty()
        );
    }

    #[test]
    fn drafts_survive_a_rescan() {
        let mut app = ready();
        select(&mut app, "syscount_demo.bt");
        press(&mut app, KeyCode::Char('i'));
        type_text(&mut app, "//x\n");
        press(&mut app, KeyCode::Esc);
        app.update(Msg::Loaded(Ok(catalog())));
        let entry = app
            .entries
            .iter()
            .find(|e| e.id() == "syscount_demo.bt")
            .expect("entry");
        assert!(entry.draft.as_ref().is_some_and(|d| d.active));
        assert!(entry.shown().content.starts_with("//x\n"));
        assert_eq!(
            entry.request.content_hash,
            entry.draft.as_ref().expect("draft").hash
        );
    }
}
