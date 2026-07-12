use ratatui::{
    buffer::Buffer,
    layout::Rect,
    style::{Color, Style},
    text::{Line, Text},
    widgets::{Block, Borders, Paragraph, Widget, Wrap},
};

use crate::store::Message;

pub struct ChatPanel {
    messages: Vec<Message>,
    scroll: u16,
}

impl ChatPanel {
    pub fn new(messages: Vec<Message>) -> Self {
        Self {
            messages,
            scroll: 0,
        }
    }

    pub fn scroll_up(&mut self, amount: u16) {
        self.scroll = self.scroll.saturating_add(amount);
    }

    pub fn scroll_down(&mut self, amount: u16) {
        self.scroll = self.scroll.saturating_sub(amount);
    }

    pub fn push_message(&mut self, message: Message) {
        self.messages.push(message);
        self.scroll = 0;
    }

    pub fn set_messages(&mut self, messages: Vec<Message>) {
        self.messages = messages;
        self.scroll = 0;
    }

    pub fn clear(&mut self) {
        self.messages.clear();
        self.scroll = 0;
    }

    pub fn messages(&self) -> &[Message] {
        &self.messages
    }
}

impl Widget for &ChatPanel {
    fn render(self, area: Rect, buf: &mut Buffer) {
        let lines: Vec<Line> = self
            .messages
            .iter()
            .flat_map(|m| {
                let role_style = match m.role.as_str() {
                    "user" => Style::default().fg(Color::Cyan),
                    "assistant" => Style::default().fg(Color::Green),
                    "system" => Style::default().fg(Color::Yellow),
                    "tool" => Style::default().fg(Color::Magenta),
                    _ => Style::default().fg(Color::Gray),
                };
                let header = Line::from(format!("[{}]", m.role)).style(role_style);
                let content = Line::from(m.content.clone());
                vec![header, content, Line::from("")]
            })
            .collect();

        let text = Text::from(lines);
        Paragraph::new(text)
            .block(Block::default().borders(Borders::ALL).title("聊天"))
            .wrap(Wrap { trim: true })
            .scroll((self.scroll, 0))
            .render(area, buf);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Utc;

    fn make_msg(role: &str, content: &str) -> Message {
        Message {
            id: 0,
            session_id: "s1".to_string(),
            role: role.to_string(),
            content: content.to_string(),
            created_at: Utc::now(),
        }
    }

    #[test]
    fn test_push_and_scroll() {
        let mut panel = ChatPanel::new(vec![]);
        panel.push_message(make_msg("user", "hello"));
        assert_eq!(panel.messages.len(), 1);
        panel.scroll_up(5);
        assert_eq!(panel.scroll, 5);
        panel.scroll_down(2);
        assert_eq!(panel.scroll, 3);
    }

    #[test]
    fn test_render_widget() {
        let panel = ChatPanel::new(vec![
            make_msg("user", "hello"),
            make_msg("assistant", "hi"),
            make_msg("tool", "done"),
            make_msg("unknown", "x"),
        ]);
        let mut buf = Buffer::empty(Rect::new(0, 0, 20, 10));
        panel.render(buf.area, &mut buf);
        let text = buf.content.iter().map(|c| c.symbol()).collect::<String>();
        assert!(text.contains("hello"));
        assert!(text.contains("assistant"));
    }

    #[test]
    fn test_clear() {
        let mut panel = ChatPanel::new(vec![make_msg("user", "hello")]);
        panel.clear();
        assert!(panel.messages.is_empty());
        assert_eq!(panel.scroll, 0);
    }
}
