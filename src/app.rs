use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyModifiers};

/// Top-level application state. Grows per docs/spec.md §5.
pub struct App {
    pub source: String,
    pub should_quit: bool,
}

impl App {
    pub fn new(source: String) -> Self {
        Self {
            source,
            should_quit: false,
        }
    }

    pub fn on_key(&mut self, key: KeyEvent) {
        if key.kind != KeyEventKind::Press {
            return;
        }
        match key.code {
            KeyCode::Char('q') => self.should_quit = true,
            KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => self.should_quit = true,
            _ => {}
        }
    }
}
