//! Text buffer of the inline script editor (D-025): lines, a cursor, undo. Pure.

/// Undo steps kept; older ones are dropped.
const UNDO_LIMIT: usize = 500;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum EditKind {
    Insert,
    Delete,
    /// Never merged with the previous step.
    Other,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct Snapshot {
    lines: Vec<String>,
    row: usize,
    col: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TextBuffer {
    lines: Vec<String>,
    /// Cursor line.
    row: usize,
    /// Cursor position in characters within the line.
    col: usize,
    /// Column that up/down try to return to.
    want_col: usize,
    trailing_newline: bool,
    undo: Vec<Snapshot>,
    /// Kind of the last edit, so typing a word is one undo step. Moving resets it.
    last: Option<EditKind>,
}

fn byte_at(s: &str, col: usize) -> usize {
    s.char_indices().nth(col).map_or(s.len(), |(i, _)| i)
}

fn char_len(s: &str) -> usize {
    s.chars().count()
}

impl TextBuffer {
    pub fn new(text: &str) -> Self {
        let trailing_newline = text.ends_with('\n');
        let body = text.strip_suffix('\n').unwrap_or(text);
        let lines = body
            .split('\n')
            .map(|l| l.strip_suffix('\r').unwrap_or(l).to_string())
            .collect();
        Self {
            lines,
            row: 0,
            col: 0,
            want_col: 0,
            trailing_newline,
            undo: Vec::new(),
            last: None,
        }
    }

    pub fn text(&self) -> String {
        let mut s = self.lines.join("\n");
        if self.trailing_newline {
            s.push('\n');
        }
        s
    }

    pub fn lines(&self) -> &[String] {
        &self.lines
    }

    /// `(line, character)`.
    pub fn cursor(&self) -> (usize, usize) {
        (self.row, self.col)
    }

    fn line(&self) -> &str {
        &self.lines[self.row]
    }

    fn checkpoint(&mut self, kind: EditKind) {
        if self.last != Some(kind) || kind == EditKind::Other {
            if self.undo.len() == UNDO_LIMIT {
                self.undo.remove(0);
            }
            self.undo.push(Snapshot {
                lines: self.lines.clone(),
                row: self.row,
                col: self.col,
            });
        }
        self.last = Some(kind);
    }

    fn moved(&mut self) {
        self.last = None;
    }

    pub fn insert(&mut self, c: char) {
        let kind = if c.is_whitespace() {
            EditKind::Other
        } else {
            EditKind::Insert
        };
        self.checkpoint(kind);
        let at = byte_at(self.line(), self.col);
        self.lines[self.row].insert(at, c);
        self.col += 1;
        self.want_col = self.col;
    }

    /// One indentation step: a tab if the script indents with tabs, else two spaces.
    pub fn indent(&mut self) {
        self.checkpoint(EditKind::Other);
        let unit = if self.lines.iter().any(|l| l.starts_with('\t')) {
            "\t"
        } else {
            "  "
        };
        let at = byte_at(self.line(), self.col);
        self.lines[self.row].insert_str(at, unit);
        self.col += char_len(unit);
        self.want_col = self.col;
    }

    /// Split the line at the cursor; the new line keeps the current indentation.
    pub fn newline(&mut self) {
        self.checkpoint(EditKind::Other);
        let line = self.line();
        let at = byte_at(line, self.col);
        let indent: String = line[..at]
            .chars()
            .take_while(|c| *c == ' ' || *c == '\t')
            .collect();
        let rest = self.lines[self.row].split_off(at);
        self.row += 1;
        self.lines.insert(self.row, format!("{indent}{rest}"));
        self.col = char_len(&indent);
        self.want_col = self.col;
    }

    pub fn backspace(&mut self) {
        if self.col == 0 && self.row == 0 {
            return;
        }
        self.checkpoint(EditKind::Delete);
        if self.col > 0 {
            let line = &mut self.lines[self.row];
            let at = byte_at(line, self.col - 1);
            line.remove(at);
            self.col -= 1;
        } else {
            let line = self.lines.remove(self.row);
            self.row -= 1;
            self.col = char_len(&self.lines[self.row]);
            self.lines[self.row].push_str(&line);
        }
        self.want_col = self.col;
    }

    pub fn delete(&mut self) {
        let len = char_len(self.line());
        if self.col == len && self.row + 1 == self.lines.len() {
            return;
        }
        self.checkpoint(EditKind::Delete);
        if self.col < len {
            let at = byte_at(self.line(), self.col);
            self.lines[self.row].remove(at);
        } else {
            let next = self.lines.remove(self.row + 1);
            self.lines[self.row].push_str(&next);
        }
    }

    pub fn left(&mut self) {
        self.moved();
        if self.col > 0 {
            self.col -= 1;
        } else if self.row > 0 {
            self.row -= 1;
            self.col = char_len(self.line());
        }
        self.want_col = self.col;
    }

    pub fn right(&mut self) {
        self.moved();
        if self.col < char_len(self.line()) {
            self.col += 1;
        } else if self.row + 1 < self.lines.len() {
            self.row += 1;
            self.col = 0;
        }
        self.want_col = self.col;
    }

    /// Up (negative) or down by `delta` lines, keeping the wanted column where possible.
    pub fn vertical(&mut self, delta: isize) {
        self.moved();
        let last = self.lines.len() - 1;
        self.row = self.row.saturating_add_signed(delta).min(last);
        self.col = self.want_col.min(char_len(self.line()));
    }

    pub fn home(&mut self) {
        self.moved();
        self.col = 0;
        self.want_col = 0;
    }

    pub fn end(&mut self) {
        self.moved();
        self.col = char_len(self.line());
        self.want_col = self.col;
    }

    /// Back to the state before the last edit step. `false` when there is nothing to undo.
    pub fn undo(&mut self) -> bool {
        let Some(s) = self.undo.pop() else {
            return false;
        };
        self.lines = s.lines;
        self.row = s.row;
        self.col = s.col;
        self.want_col = s.col;
        self.last = None;
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use pretty_assertions::assert_eq;

    fn typed(b: &mut TextBuffer, s: &str) {
        for c in s.chars() {
            match c {
                '\n' => b.newline(),
                c => b.insert(c),
            }
        }
    }

    #[test]
    fn round_trips_text() {
        for text in ["", "a", "a\n", "a\nb\n", "a\n\nb", "ü\tx\n"] {
            assert_eq!(TextBuffer::new(text).text(), text);
        }
        assert_eq!(TextBuffer::new("a\r\nb\r\n").text(), "a\nb\n");
    }

    #[test]
    fn typing_newlines_and_indentation() {
        let mut b = TextBuffer::new("BEGIN {\n}\n");
        b.end();
        typed(&mut b, "\n");
        b.indent();
        typed(&mut b, "printf(\"hi\");\nexit();");
        assert_eq!(b.text(), "BEGIN {\n  printf(\"hi\");\n  exit();\n}\n");
        assert_eq!(b.cursor(), (2, 9));
        let mut tabs = TextBuffer::new("x {\n\ty;\n}");
        tabs.vertical(1);
        tabs.end();
        tabs.newline();
        tabs.insert('z');
        assert_eq!(tabs.text(), "x {\n\ty;\n\tz\n}");
    }

    #[test]
    fn deleting_across_lines_and_multibyte() {
        let mut b = TextBuffer::new("aü\nbc");
        b.vertical(1);
        b.backspace();
        assert_eq!((b.text(), b.cursor()), ("aübc".to_string(), (0, 2)));
        b.backspace();
        assert_eq!(b.text(), "abc");
        b.home();
        b.delete();
        assert_eq!(b.text(), "bc");
        b.end();
        b.delete(); // at the very end: nothing
        b.home();
        b.backspace(); // at the very start: nothing
        assert_eq!(b.text(), "bc");
        let mut j = TextBuffer::new("a\nb");
        j.end();
        j.delete();
        assert_eq!(j.text(), "ab");
    }

    #[test]
    fn cursor_moves_keep_the_wanted_column() {
        let mut b = TextBuffer::new("long line\nab\nanother long");
        b.end();
        b.vertical(1);
        assert_eq!(b.cursor(), (1, 2));
        b.vertical(1);
        assert_eq!(b.cursor(), (2, 9));
        b.vertical(-10);
        assert_eq!(b.cursor(), (0, 9));
        b.vertical(10);
        assert_eq!(b.cursor().0, 2);
        b.home();
        b.left();
        assert_eq!(b.cursor(), (1, 2));
        b.right();
        assert_eq!(b.cursor(), (2, 0));
    }

    #[test]
    fn undo_groups_words() {
        let mut b = TextBuffer::new("x\n");
        b.end();
        typed(&mut b, " foo bar");
        b.backspace();
        b.backspace();
        assert_eq!(b.text(), "x foo b\n");
        assert!(b.undo()); // the two backspaces
        assert_eq!(b.text(), "x foo bar\n");
        assert!(b.undo()); // "bar"
        assert!(b.undo()); // " "
        assert_eq!(b.text(), "x foo\n");
        while b.undo() {}
        assert_eq!(b.text(), "x\n");
        assert_eq!(b.cursor(), (0, 1));
        assert!(!b.undo());
    }
}
