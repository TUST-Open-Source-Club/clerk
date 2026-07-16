use ratatui::{
    buffer::Buffer,
    layout::Rect,
    style::{Color, Style},
    text::{Line, Text},
    widgets::{Block, Borders, Paragraph, Widget},
};
use unicode_segmentation::UnicodeSegmentation;
use unicode_width::UnicodeWidthStr;

/// 输入框已知的斜杠命令及描述，用于 Tab 补全、命令提示与 /help。
pub const KNOWN_COMMANDS: &[(&str, &str)] = &[
    ("/help", "显示帮助"),
    ("/new", "开始新会话"),
    ("/model", "查看当前模型与接口地址"),
    ("/yolo", "切换 YOLO 模式（工具免确认）"),
    ("/clear", "清空聊天"),
    ("/sessions", "列出最近会话"),
    ("/attach", "附加图片/视频到下一次消息"),
    ("/attachments", "列出已附加的媒体"),
    ("/clear_attachments", "清除已附加的媒体"),
    ("/exit", "退出应用"),
];

/// 输入框为空时展示的占位提示。
const PLACEHOLDER: &str = "输入消息，/ 查看命令，Tab 补全";

/// 输入长度超过该阈值时在标题中显示字数统计。
const INPUT_COUNT_THRESHOLD: usize = 200;

/// 多行输入框：以 grapheme 为单位维护光标，支持中文等宽字符与斜杠命令补全。
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

    pub fn insert_str(&mut self, s: &str) {
        for c in s.chars() {
            self.insert_char(c);
        }
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

    /// Returns the text after the leading `/` if the input starts with `/`.
    pub fn current_command(&self) -> Option<&str> {
        let first = self.lines.first()?;
        first.strip_prefix('/')
    }

    /// If the input starts with `/`, replaces the current slash-command token
    /// with the longest common prefix of matching commands. When there is
    /// exactly one match, the command is completed and a trailing space is added.
    pub fn autocomplete(&mut self) {
        let text = self.text();
        if !text.starts_with('/') {
            return;
        }
        let token = text.split_whitespace().next().unwrap_or("");
        let matches: Vec<&str> = KNOWN_COMMANDS
            .iter()
            .map(|(cmd, _)| *cmd)
            .filter(|cmd| cmd.starts_with(token))
            .collect();
        if matches.is_empty() {
            return;
        }

        let lcp = longest_common_prefix(&matches);
        let replacement = if matches.len() == 1 {
            format!("{} ", lcp)
        } else {
            lcp
        };
        let new_text = text.replacen(token, &replacement, 1);
        self.lines = new_text.split('\n').map(String::from).collect();
        self.cursor = (0, grapheme_count(&self.lines[0]));
    }

    /// Returns the suffix of the first matching slash command when the input
    /// starts with `/` but is not already a complete command.
    pub fn command_hint(&self) -> Option<String> {
        let text = self.text();
        if !text.starts_with('/') {
            return None;
        }
        let token = text.split_whitespace().next().unwrap_or("");
        KNOWN_COMMANDS
            .iter()
            .map(|(cmd, _)| *cmd)
            .find(|cmd| cmd.starts_with(token) && *cmd != token)
            .map(|cmd| cmd[token.len()..].to_string())
    }

    /// 当前输入匹配的斜杠命令建议（命令, 描述），用于输入框上方的提示区。
    /// 仅在输入以 `/` 开头且仍在输入命令名（无空白字符）时返回。
    pub fn command_suggestions(&self) -> Vec<(&'static str, &'static str)> {
        let text = self.text();
        if !text.starts_with('/') || text.contains(char::is_whitespace) {
            return Vec::new();
        }
        KNOWN_COMMANDS
            .iter()
            .filter(|(cmd, _)| cmd.starts_with(text.as_str()))
            .copied()
            .collect()
    }

    /// Computes the cursor's `(x, y)` screen coordinates relative to the
    /// rendering `area`, taking line wrapping into account.
    pub fn cursor_screen_pos(&self, area: Rect) -> (u16, u16) {
        let width = area.width.saturating_sub(2) as usize;
        if width == 0 || self.lines.is_empty() {
            return (1, 1);
        }

        let mut y = 1u16;
        for (row, line) in self.lines.iter().enumerate() {
            if row == self.cursor.0 {
                let prefix: String = line.graphemes(true).take(self.cursor.1).collect();
                let prefix_width = prefix.width();
                let wrapped_before = prefix_width / width;
                y += wrapped_before as u16;
                let x = 1 + (prefix_width % width) as u16;
                return (x, y);
            }
            y += wrapped_line_count(line, width) as u16;
        }
        (1, y)
    }
}

impl Widget for &InputArea {
    fn render(self, area: Rect, buf: &mut Buffer) {
        let mut title = "输入 (Enter 发送 · Shift+Enter 换行)".to_string();
        let count: usize = self.lines.iter().map(|l| grapheme_count(l)).sum();
        if count > INPUT_COUNT_THRESHOLD {
            title.push_str(&format!(" [{} 字]", count));
        }
        let block = Block::default().borders(Borders::ALL).title(title);

        if self.is_empty() {
            Paragraph::new(Line::from(PLACEHOLDER))
                .style(Style::default().fg(Color::DarkGray))
                .block(block)
                .render(area, buf);
            return;
        }

        let width = area.width.saturating_sub(2) as usize;
        let wrapped = wrap_lines(&self.lines, width.max(1));
        let text = Text::from(wrapped.into_iter().map(Line::from).collect::<Vec<_>>());
        Paragraph::new(text)
            .block(block)
            .style(Style::default().fg(Color::White))
            .render(area, buf);
    }
}

/// 字符串的 grapheme（用户感知字符）数量。
fn grapheme_count(s: &str) -> usize {
    s.graphemes(true).count()
}

/// 第 N 个 grapheme 对应的字节偏移；越界时返回字符串长度。
fn byte_index_from_grapheme(s: &str, grapheme_idx: usize) -> usize {
    s.grapheme_indices(true)
        .nth(grapheme_idx)
        .map(|(idx, _)| idx)
        .unwrap_or(s.len())
}

/// 字节偏移之前一个 grapheme 边界。
fn prev_grapheme_boundary(s: &str, byte_idx: usize) -> usize {
    s.grapheme_indices(true)
        .take_while(|(idx, _)| *idx < byte_idx)
        .last()
        .map(|(idx, _g)| idx)
        .unwrap_or(0)
}

/// 字节偏移之后一个 grapheme 边界；越界时返回字符串长度。
fn next_grapheme_boundary(s: &str, byte_idx: usize) -> usize {
    s.grapheme_indices(true)
        .find(|(idx, _)| *idx > byte_idx)
        .map(|(idx, _)| idx)
        .unwrap_or(s.len())
}

/// 按显示宽度将每行软换行为多行（考虑 grapheme 宽度）。
fn wrap_lines(lines: &[String], width: usize) -> Vec<String> {
    let mut result = Vec::new();
    for line in lines {
        if line.is_empty() {
            result.push(String::new());
            continue;
        }
        let mut current = String::new();
        let mut current_width = 0usize;
        for g in line.graphemes(true) {
            let w = g.width();
            if !current.is_empty() && current_width + w > width {
                result.push(current);
                current = String::new();
                current_width = 0;
            }
            current.push_str(g);
            current_width += w;
        }
        if !current.is_empty() {
            result.push(current);
        }
    }
    result
}

/// 计算单行按显示宽度软换行后的行数。
fn wrapped_line_count(s: &str, width: usize) -> usize {
    if s.is_empty() {
        return 1;
    }
    let mut count = 0usize;
    let mut current_width = 0usize;
    for g in s.graphemes(true) {
        let w = g.width();
        if current_width > 0 && current_width + w > width {
            count += 1;
            current_width = 0;
        }
        current_width += w;
    }
    if current_width > 0 {
        count += 1;
    }
    count
}

/// 计算一组字符串的最长公共前缀。
pub(crate) fn longest_common_prefix(strs: &[&str]) -> String {
    if strs.is_empty() {
        return String::new();
    }
    let mut prefix = strs[0].to_string();
    for s in &strs[1..] {
        while !s.starts_with(&prefix) {
            prefix.pop();
            if prefix.is_empty() {
                break;
            }
        }
    }
    prefix
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
    fn test_render_and_clear() {
        let mut input = InputArea::new();
        input.insert_char('a');
        let mut buf = Buffer::empty(Rect::new(0, 0, 20, 5));
        input.render(buf.area, &mut buf);
        let text = buf.content.iter().map(|c| c.symbol()).collect::<String>();
        assert!(text.contains('a'));

        input.clear();
        assert!(input.is_empty());
        assert_eq!(input.cursor(), (0, 0));
    }

    #[test]
    fn test_movement_across_lines() {
        let mut input = InputArea::new();
        input.insert_char('a');
        input.insert_newline();
        input.insert_char('b');
        input.move_home();
        input.move_left();
        assert_eq!(input.cursor(), (0, 1));
        input.move_right();
        assert_eq!(input.cursor(), (1, 0));
        input.move_down();
        assert_eq!(input.cursor(), (1, 0));
    }

    #[test]
    fn test_delete_merge_lines() {
        let mut input = InputArea::new();
        input.insert_char('a');
        input.insert_newline();
        input.insert_char('b');
        input.move_home();
        input.move_left();
        input.delete_char();
        assert_eq!(input.text(), "ab");
    }

    #[test]
    fn test_backspace_at_line_start() {
        let mut input = InputArea::new();
        input.insert_char('a');
        input.insert_newline();
        input.backspace();
        assert_eq!(input.text(), "a");
        assert_eq!(input.cursor(), (0, 1));
    }

    #[test]
    fn test_grapheme_boundaries() {
        assert_eq!(grapheme_count("中文"), 2);
        assert_eq!(byte_index_from_grapheme("中文", 1), 3);
        assert_eq!(prev_grapheme_boundary("中文", 3), 0);
        assert_eq!(next_grapheme_boundary("中文", 0), 3);
    }

    #[test]
    fn test_current_command() {
        let mut input = InputArea::new();
        assert_eq!(input.current_command(), None);
        input.insert_char('/');
        input.insert_char('e');
        input.insert_char('x');
        assert_eq!(input.current_command(), Some("ex"));
        input.clear();
        input.insert_char('h');
        assert_eq!(input.current_command(), None);
    }

    #[test]
    fn test_autocomplete_single_match() {
        let mut input = InputArea::new();
        for c in "/ex".chars() {
            input.insert_char(c);
        }
        input.autocomplete();
        assert_eq!(input.text(), "/exit ");
        assert_eq!(input.cursor(), (0, 6));
    }

    #[test]
    fn test_autocomplete_longest_common_prefix() {
        let mut input = InputArea::new();
        input.insert_char('/');
        // All known commands share the leading '/', so the longest common prefix is '/'.
        input.autocomplete();
        assert_eq!(input.text(), "/");
        assert_eq!(input.cursor(), (0, 1));
    }

    #[test]
    fn test_autocomplete_no_match() {
        let mut input = InputArea::new();
        for c in "/zzz".chars() {
            input.insert_char(c);
        }
        input.autocomplete();
        assert_eq!(input.text(), "/zzz");
    }

    #[test]
    fn test_autocomplete_ignored_without_slash() {
        let mut input = InputArea::new();
        input.insert_char('e');
        input.insert_char('x');
        input.autocomplete();
        assert_eq!(input.text(), "ex");
    }

    #[test]
    fn test_command_hint() {
        let mut input = InputArea::new();
        for c in "/ex".chars() {
            input.insert_char(c);
        }
        assert_eq!(input.command_hint(), Some("it".to_string()));

        input.clear();
        for c in "/exit".chars() {
            input.insert_char(c);
        }
        assert_eq!(input.command_hint(), None);

        input.clear();
        input.insert_char('h');
        assert_eq!(input.command_hint(), None);
    }

    #[test]
    fn test_command_suggestions_all_on_slash() {
        let mut input = InputArea::new();
        input.insert_char('/');
        let suggestions = input.command_suggestions();
        assert_eq!(suggestions.len(), KNOWN_COMMANDS.len());
        assert!(suggestions.contains(&("/help", "显示帮助")));
        assert!(suggestions.contains(&("/new", "开始新会话")));
        assert!(suggestions.contains(&("/model", "查看当前模型与接口地址")));
    }

    #[test]
    fn test_command_suggestions_filtered() {
        let mut input = InputArea::new();
        for c in "/cl".chars() {
            input.insert_char(c);
        }
        let suggestions = input.command_suggestions();
        let names: Vec<&str> = suggestions.iter().map(|(cmd, _)| *cmd).collect();
        assert!(names.contains(&"/clear"));
        assert!(names.contains(&"/clear_attachments"));
        assert!(!names.contains(&"/help"));
    }

    #[test]
    fn test_command_suggestions_hidden_when_not_typing_command() {
        let mut input = InputArea::new();
        input.insert_char('h');
        assert!(input.command_suggestions().is_empty());

        // 命令名之后输入参数时不再提示
        input.clear();
        for c in "/attach foo".chars() {
            input.insert_char(c);
        }
        assert!(input.command_suggestions().is_empty());
    }

    #[test]
    fn test_autocomplete_new_command() {
        let mut input = InputArea::new();
        for c in "/ne".chars() {
            input.insert_char(c);
        }
        input.autocomplete();
        assert_eq!(input.text(), "/new ");
    }

    /// 收集缓冲区文本并去除空白（宽字符会将其后的单元格重置为空格）。
    fn compact_buffer_text(buf: &Buffer) -> String {
        buf.content
            .iter()
            .map(|c| c.symbol())
            .collect::<String>()
            .chars()
            .filter(|c| !c.is_whitespace())
            .collect()
    }

    #[test]
    fn test_placeholder_rendered_when_empty() {
        let input = InputArea::new();
        let mut buf = Buffer::empty(Rect::new(0, 0, 40, 5));
        input.render(buf.area, &mut buf);
        let text = compact_buffer_text(&buf);
        assert!(text.contains("输入消息"));
    }

    #[test]
    fn test_placeholder_hidden_with_text() {
        let mut input = InputArea::new();
        input.insert_char('a');
        let mut buf = Buffer::empty(Rect::new(0, 0, 40, 5));
        input.render(buf.area, &mut buf);
        let text = compact_buffer_text(&buf);
        assert!(!text.contains("输入消息，/"));
    }

    #[test]
    fn test_length_counter_shown_over_threshold() {
        let mut input = InputArea::new();
        for _ in 0..201 {
            input.insert_char('a');
        }
        let mut buf = Buffer::empty(Rect::new(0, 0, 80, 12));
        input.render(buf.area, &mut buf);
        let text = compact_buffer_text(&buf);
        assert!(text.contains("[201字]"));
    }

    #[test]
    fn test_cursor_screen_pos_basic() {
        let mut input = InputArea::new();
        input.insert_char('a');
        input.insert_char('b');
        input.insert_char('c');
        // area width 10 -> inner width 8; cursor at col 3 -> x = 1 + 3 = 4
        let area = Rect::new(5, 5, 10, 5);
        assert_eq!(input.cursor_screen_pos(area), (4, 1));
    }

    #[test]
    fn test_cursor_screen_pos_with_line_wrap() {
        let mut input = InputArea::new();
        for c in "abcdefghij".chars() {
            input.insert_char(c);
        }
        // inner width 4; cursor at end (col 10, display width 10)
        // wrapped_before = 10 / 4 = 2, x = 1 + (10 % 4) = 3
        let area = Rect::new(0, 0, 6, 5);
        assert_eq!(input.cursor_screen_pos(area), (3, 3));
    }

    #[test]
    fn test_cursor_screen_pos_multiline() {
        let mut input = InputArea::new();
        input.insert_char('a');
        input.insert_newline();
        input.insert_char('b');
        let area = Rect::new(0, 0, 10, 5);
        assert_eq!(input.cursor_screen_pos(area), (2, 2));
    }

    #[test]
    fn test_wrap_lines_in_render() {
        let mut input = InputArea::new();
        for c in "abcdef".chars() {
            input.insert_char(c);
        }
        let mut buf = Buffer::empty(Rect::new(0, 0, 6, 5));
        input.render(buf.area, &mut buf);
        let text = buf.content.iter().map(|c| c.symbol()).collect::<String>();
        assert!(text.contains("abcd"));
        assert!(text.contains("ef"));
    }
}
