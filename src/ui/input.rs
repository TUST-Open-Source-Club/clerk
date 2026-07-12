use ratatui::{
    buffer::Buffer,
    layout::Rect,
    style::{Color, Style},
    text::{Line, Text},
    widgets::{Block, Borders, Paragraph, Widget},
};
use unicode_segmentation::UnicodeSegmentation;

#[derive(Debug, Clone)]
pub struct InputArea {
    lines: Vec<String>,
    cursor: (usize, usize), // (row, column in graphemes)
}

impl Default for InputArea {
    fn default() -> Self {
        Self {
            lines: vec![String::new()],
            cursor: (0, 0),
        }
    }
}

impl InputArea {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn text(&self) -> String {
        self.lines.join("\n")
    }

    pub fn clear(&mut self) {
        self.lines = vec![String::new()];
        self.cursor = (0, 0);
    }

    pub fn is_empty(&self) -> bool {
        self.lines.len() == 1 && self.lines[0].is_empty()
    }

    pub fn insert_char(&mut self, c: char) {
        let (row, col) = self.cursor;
        let line = &mut self.lines[row];
        let byte_idx = byte_index_from_grapheme(line, col);
        line.insert(byte_idx, c);
        self.cursor.1 += 1;
    }

    pub fn insert_newline(&mut self) {
        let (row, col) = self.cursor;
        let line = self.lines[row].clone();
        let byte_idx = byte_index_from_grapheme(&line, col);
        let new_line = line[byte_idx..].to_string();
        self.lines[row].truncate(byte_idx);
        self.lines.insert(row + 1, new_line);
        self.cursor = (row + 1, 0);
    }

    pub fn backspace(&mut self) {
        let (row, col) = self.cursor;
        if col > 0 {
            let line = &mut self.lines[row];
            let byte_idx = byte_index_from_grapheme(line, col);
            let prev_byte_idx = prev_grapheme_boundary(line, byte_idx);
            line.replace_range(prev_byte_idx..byte_idx, "");
            self.cursor.1 -= 1;
        } else if row > 0 {
            let removed = self.lines.remove(row);
            let prev_len = grapheme_count(&self.lines[row - 1]);
            self.lines[row - 1].push_str(&removed);
            self.cursor = (row - 1, prev_len);
        }
    }

    pub fn delete_char(&mut self) {
        let (row, col) = self.cursor;
        let line = &mut self.lines[row];
        if col < grapheme_count(line) {
            let byte_idx = byte_index_from_grapheme(line, col);
            let next_byte_idx = next_grapheme_boundary(line, byte_idx);
            line.replace_range(byte_idx..next_byte_idx, "");
        } else if row + 1 < self.lines.len() {
            let next = self.lines.remove(row + 1);
            self.lines[row].push_str(&next);
        }
    }

    pub fn move_left(&mut self) {
        let (row, col) = self.cursor;
        if col > 0 {
            self.cursor.1 -= 1;
        } else if row > 0 {
            self.cursor.0 -= 1;
            self.cursor.1 = grapheme_count(&self.lines[self.cursor.0]);
        }
    }

    pub fn move_right(&mut self) {
        let (row, col) = self.cursor;
        if col < grapheme_count(&self.lines[row]) {
            self.cursor.1 += 1;
        } else if row + 1 < self.lines.len() {
            self.cursor.0 += 1;
            self.cursor.1 = 0;
        }
    }

    pub fn move_up(&mut self) {
        if self.cursor.0 > 0 {
            self.cursor.0 -= 1;
            let target_len = grapheme_count(&self.lines[self.cursor.0]);
            self.cursor.1 = self.cursor.1.min(target_len);
        }
    }

    pub fn move_down(&mut self) {
        if self.cursor.0 + 1 < self.lines.len() {
            self.cursor.0 += 1;
            let target_len = grapheme_count(&self.lines[self.cursor.0]);
            self.cursor.1 = self.cursor.1.min(target_len);
        }
    }

    pub fn move_home(&mut self) {
        self.cursor.1 = 0;
    }

    pub fn move_end(&mut self) {
        self.cursor.1 = grapheme_count(&self.lines[self.cursor.0]);
    }

    pub fn cursor(&self) -> (usize, usize) {
        self.cursor
    }
}

impl Widget for &InputArea {
    fn render(self, area: Rect, buf: &mut Buffer) {
        let text = Text::from(
            self.lines
                .iter()
                .map(|l| Line::from(l.clone()))
                .collect::<Vec<_>>(),
        );
        Paragraph::new(text)
            .block(
                Block::default()
                    .borders(Borders::ALL)
                    .title("输入 (Enter 发送, Shift+Enter 换行, Esc 退出)"),
            )
            .style(Style::default().fg(Color::White))
            .render(area, buf);
    }
}

fn grapheme_count(s: &str) -> usize {
    s.graphemes(true).count()
}

fn byte_index_from_grapheme(s: &str, grapheme_idx: usize) -> usize {
    s.grapheme_indices(true)
        .nth(grapheme_idx)
        .map(|(idx, _)| idx)
        .unwrap_or(s.len())
}

fn prev_grapheme_boundary(s: &str, byte_idx: usize) -> usize {
    s.grapheme_indices(true)
        .take_while(|(idx, _)| *idx < byte_idx)
        .last()
        .map(|(idx, _g)| idx)
        .unwrap_or(0)
}

fn next_grapheme_boundary(s: &str, byte_idx: usize) -> usize {
    s.grapheme_indices(true)
        .find(|(idx, _)| *idx > byte_idx)
        .map(|(idx, _)| idx)
        .unwrap_or(s.len())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_insert_and_text() {
        let mut input = InputArea::new();
        input.insert_char('h');
        input.insert_char('i');
        assert_eq!(input.text(), "hi");
    }

    #[test]
    fn test_newline_and_cursor() {
        let mut input = InputArea::new();
        input.insert_char('a');
        input.insert_char('b');
        input.insert_newline();
        input.insert_char('c');
        assert_eq!(input.text(), "ab\nc");
        assert_eq!(input.cursor(), (1, 1));
    }

    #[test]
    fn test_backspace_merge_lines() {
        let mut input = InputArea::new();
        input.insert_char('a');
        input.insert_newline();
        input.insert_char('b');
        input.move_left();
        input.backspace();
        assert_eq!(input.text(), "ab");
        assert_eq!(input.cursor(), (0, 1));
    }

    #[test]
    fn test_delete_char() {
        let mut input = InputArea::new();
        input.insert_char('a');
        input.insert_char('b');
        input.move_left();
        input.move_left();
        input.delete_char();
        assert_eq!(input.text(), "b");
    }

    #[test]
    fn test_movement() {
        let mut input = InputArea::new();
        input.insert_char('a');
        input.insert_newline();
        input.insert_char('b');
        input.move_home();
        assert_eq!(input.cursor().1, 0);
        input.move_up();
        input.move_end();
        assert_eq!(input.cursor(), (0, 1));
    }

    #[test]
    fn test_grapheme_handling() {
        let mut input = InputArea::new();
        input.insert_char('中');
        input.insert_char('文');
        assert_eq!(input.text(), "中文");
        input.move_left();
        input.backspace();
        assert_eq!(input.text(), "文");
    }
}
