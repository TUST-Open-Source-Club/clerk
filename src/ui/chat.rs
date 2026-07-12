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
    fn test_set_messages_resets_scroll() {
        let mut panel = ChatPanel::new(vec![]);
        panel.scroll_up(10);
        panel.set_messages(vec![make_msg("assistant", "hi")]);
        assert_eq!(panel.scroll, 0);
        assert_eq!(panel.messages.len(), 1);
    }
}
