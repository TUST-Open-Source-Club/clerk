use ratatui::{
    buffer::Buffer,
    layout::{Alignment, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span, Text},
    widgets::{Block, Borders, Paragraph, Widget, Wrap},
};

use crate::ui::markdown::markdown_to_text;
use clerk_core::store::Message;

/// 聊天面板：按角色着色渲染消息列表，支持滚动。
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

    /// 更新最后一条消息的内容（流式渲染用）；无消息时返回 false。
    pub fn update_last_message(&mut self, content: impl Into<String>) -> bool {
        if let Some(last) = self.messages.last_mut() {
            last.content = content.into();
            true
        } else {
            false
        }
    }

    pub fn pop_last(&mut self) -> Option<Message> {
        let msg = self.messages.pop();
        if msg.is_some() {
            self.scroll = 0;
        }
        msg
    }

    /// 更新指定下标消息的内容（加载占位动画用）；越界时返回 false。
    pub fn update_message_at(&mut self, index: usize, content: impl Into<String>) -> bool {
        if let Some(msg) = self.messages.get_mut(index) {
            msg.content = content.into();
            true
        } else {
            false
        }
    }

    /// 移除指定下标的消息（撤下加载占位用）；越界时返回 None。
    pub fn remove_message_at(&mut self, index: usize) -> Option<Message> {
        if index < self.messages.len() {
            self.scroll = 0;
            Some(self.messages.remove(index))
        } else {
            None
        }
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
        let block = Block::default().borders(Borders::ALL).title("聊天");

        if self.messages.is_empty() {
            let inner = block.inner(area);
            block.render(area, buf);
            let lines = welcome_lines();
            let top_pad = inner.height.saturating_sub(lines.len() as u16) / 2;
            let mut padded: Vec<Line> = (0..top_pad).map(|_| Line::from("")).collect();
            padded.extend(lines);
            Paragraph::new(padded)
                .alignment(Alignment::Center)
                .render(inner, buf);
            return;
        }

        let mut all_lines: Vec<Line> = Vec::new();

        for m in &self.messages {
            let role_style = match m.role.as_str() {
                "user" => Style::default().fg(Color::Cyan),
                "assistant" => Style::default().fg(Color::Green),
                "system" => Style::default().fg(Color::Yellow),
                "tool" => tool_event_style(&m.content),
                _ => Style::default().fg(Color::Gray),
            };
            let header = Line::from(format!("[{}]", m.role)).style(role_style);
            all_lines.push(header);

            // 工具事件按类别着色（调用黄 / 结果绿 / 错误红），其余消息用白色正文
            let content_style = if m.role == "tool" {
                tool_event_style(&m.content)
            } else {
                Style::default().fg(Color::White)
            };
            let content_text = markdown_to_text(&m.content);
            for mut line in content_text.lines {
                // 将 markdown 样式与基础文本颜色叠加到每个 span
                for span in &mut line.spans {
                    span.style = span.style.patch(content_style);
                }
                all_lines.push(line);
            }
            all_lines.push(Line::from(""));
        }

        let text = Text::from(all_lines);
        Paragraph::new(text)
            .block(block)
            .wrap(Wrap { trim: false })
            .scroll((self.scroll, 0))
            .render(area, buf);
    }
}

/// 工具事件的类别颜色：调用黄、结果绿、错误红、计划蓝。
/// 依据 clerk_core::text::format_tool_event 的输出前缀判断。
fn tool_event_style(content: &str) -> Style {
    if content.starts_with("工具错误") {
        Style::default().fg(Color::Red)
    } else if content.starts_with("调用工具") {
        Style::default().fg(Color::Yellow)
    } else if content.starts_with("执行计划") {
        Style::default().fg(Color::Blue)
    } else {
        Style::default().fg(Color::Green)
    }
}

/// 空会话时的欢迎页：标题、简短用法与常用命令。
fn welcome_lines() -> Vec<Line<'static>> {
    let title_style = Style::default()
        .fg(Color::Cyan)
        .add_modifier(Modifier::BOLD);
    let key_style = Style::default().fg(Color::Yellow);
    let hint_style = Style::default().fg(Color::DarkGray);
    vec![
        Line::from(Span::styled(
            concat!("✻ Clerk v", env!("CARGO_PKG_VERSION")),
            title_style,
        )),
        Line::from(""),
        Line::from(Span::styled(
            "终端里的 AI 助手",
            Style::default().fg(Color::Gray),
        )),
        Line::from(""),
        Line::from(Span::styled(
            "输入消息开始对话，输入 / 查看全部命令",
            hint_style,
        )),
        Line::from(""),
        Line::from(vec![
            Span::styled("/help", key_style),
            Span::styled("  显示帮助    ", hint_style),
            Span::styled("/new", key_style),
            Span::styled("  开始新会话", hint_style),
        ]),
        Line::from(vec![
            Span::styled("/model", key_style),
            Span::styled("  查看模型    ", hint_style),
            Span::styled("/yolo", key_style),
            Span::styled("  切换工具免确认", hint_style),
        ]),
        Line::from(""),
        Line::from(Span::styled(
            "Enter 发送 · Shift+Enter 换行 · Shift+↑/↓ 滚动聊天",
            hint_style,
        )),
    ]
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
    fn test_update_message_at() {
        let mut panel = ChatPanel::new(vec![make_msg("user", "a"), make_msg("assistant", "b")]);
        assert!(panel.update_message_at(1, "c"));
        assert_eq!(panel.messages()[1].content, "c");
        assert!(!panel.update_message_at(5, "x"));
    }

    #[test]
    fn test_remove_message_at() {
        let mut panel = ChatPanel::new(vec![
            make_msg("user", "a"),
            make_msg("assistant", "b"),
            make_msg("user", "c"),
        ]);
        let removed = panel.remove_message_at(1).unwrap();
        assert_eq!(removed.content, "b");
        assert_eq!(panel.messages().len(), 2);
        assert_eq!(panel.messages()[1].content, "c");
        assert!(panel.remove_message_at(9).is_none());
    }

    #[test]
    fn test_clear() {
        let mut panel = ChatPanel::new(vec![make_msg("user", "hello")]);
        panel.clear();
        assert!(panel.messages.is_empty());
        assert_eq!(panel.scroll, 0);
    }

    #[test]
    fn test_welcome_rendered_when_empty() {
        let panel = ChatPanel::new(vec![]);
        let mut buf = Buffer::empty(Rect::new(0, 0, 60, 20));
        panel.render(buf.area, &mut buf);
        let text = buf.content.iter().map(|c| c.symbol()).collect::<String>();
        assert!(text.contains("Clerk"));
        assert!(text.contains("/help"));
        assert!(text.contains("/new"));
    }

    #[test]
    fn test_welcome_hidden_with_messages() {
        let panel = ChatPanel::new(vec![make_msg("user", "hello")]);
        let mut buf = Buffer::empty(Rect::new(0, 0, 60, 20));
        panel.render(buf.area, &mut buf);
        let text = buf.content.iter().map(|c| c.symbol()).collect::<String>();
        assert!(text.contains("hello"));
        assert!(!text.contains("Clerk v"));
    }

    #[test]
    fn test_tool_event_colors() {
        let panel = ChatPanel::new(vec![
            make_msg("tool", "调用工具 fs_read: path=/tmp/a"),
            make_msg("tool", "工具 fs_read 结果: ok"),
            make_msg("tool", "工具错误: boom"),
            make_msg("tool", "执行计划：\n1. 读取"),
        ]);
        let mut buf = Buffer::empty(Rect::new(0, 0, 60, 20));
        panel.render(buf.area, &mut buf);

        // 布局：边框 y=0；每条消息为 [tool] 头 + 内容 + 空行
        // 调用（y=1）黄、结果（y=4）绿、错误（y=7）红、计划（y=10）蓝
        assert_eq!(buf.get(1, 1).fg, Color::Yellow);
        assert_eq!(buf.get(1, 2).fg, Color::Yellow);
        assert_eq!(buf.get(1, 4).fg, Color::Green);
        assert_eq!(buf.get(1, 5).fg, Color::Green);
        assert_eq!(buf.get(1, 7).fg, Color::Red);
        assert_eq!(buf.get(1, 8).fg, Color::Red);
        assert_eq!(buf.get(1, 10).fg, Color::Blue);
    }
}
