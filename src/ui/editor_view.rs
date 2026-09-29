//! The inline editor on the Source tab (D-025): highlighted text with line numbers, the
//! terminal cursor at the edit position, scrolled so the cursor stays visible.

use ratatui::Frame;
use ratatui::layout::{Position, Rect};
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;

use super::source_view::{self, classify};
use super::theme::Theme;
use crate::app::Editor;

const TAB: usize = 4;

/// Display columns of `s` (tabs expanded, as `source_view` draws them).
fn width(s: &str) -> usize {
    s.chars().map(|c| if c == '\t' { TAB } else { 1 }).sum()
}

/// Keep `pos` inside `[start, start + size)` by moving `start`.
fn follow(start: usize, pos: usize, size: usize) -> usize {
    if pos < start {
        pos
    } else if size > 0 && pos >= start + size {
        pos + 1 - size
    } else {
        start
    }
}

pub fn draw(frame: &mut Frame, area: Rect, ed: &Editor) {
    let text = ed.buffer.lines().join("\n");
    let classified = classify(&text);
    let (row, col) = ed.buffer.cursor();
    let gutter = ed.buffer.lines().len().to_string().len() + 1;
    let height = usize::from(area.height);
    let text_width = usize::from(area.width).saturating_sub(gutter);

    let line = &ed.buffer.lines()[row];
    let cursor_x = width(&line.chars().take(col).collect::<String>());
    let top = follow(ed.top.get(), row, height);
    let left = follow(ed.left.get(), cursor_x, text_width);
    ed.top.set(top);
    ed.left.set(left);

    let lines: Vec<Line> = classified
        .iter()
        .enumerate()
        .skip(top)
        .take(height)
        .map(|(n, segments)| {
            let mut spans = vec![Span::styled(
                format!("{:>w$} ", n + 1, w = gutter - 1),
                if n == row {
                    Theme::title()
                } else {
                    Theme::line_number()
                },
            )];
            // Skip `left` columns, keep `text_width`.
            let (mut skip, mut room) = (left, text_width);
            for (kind, seg) in segments {
                let seg = seg.replace('\t', &" ".repeat(TAB));
                let visible: String = seg.chars().skip(skip).take(room).collect();
                skip = skip.saturating_sub(seg.chars().count());
                room -= visible.chars().count();
                if !visible.is_empty() {
                    spans.push(Span::styled(visible, source_view::style(*kind)));
                }
            }
            Line::from(spans)
        })
        .collect();
    frame.render_widget(Paragraph::new(lines), area);

    let x = area.x + u16::try_from(gutter + cursor_x - left).unwrap_or(0);
    let y = area.y + u16::try_from(row - top).unwrap_or(0);
    frame.set_cursor_position(Position::new(x.min(area.right().saturating_sub(1)), y));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn follow_keeps_the_position_visible() {
        assert_eq!(follow(0, 5, 10), 0);
        assert_eq!(follow(0, 12, 10), 3);
        assert_eq!(follow(8, 3, 10), 3);
        assert_eq!(width("\tab"), 6);
    }
}
