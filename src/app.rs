use anyhow::Result;
use crossterm::event::{self, Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use ratatui::{
    backend::Backend,
    layout::{Constraint, Direction, Layout},
    style::{Color, Style},
    text::Line,
    widgets::{Block, Borders, Paragraph},
    Frame, Terminal,
};
use std::time::Duration;
use tracing::{error, info};
use uuid::Uuid;

use crate::store::{Message, Store};
use crate::ui::chat::ChatPanel;
use crate::ui::input::InputArea;

#[derive(Debug, Clone, PartialEq)]
pub enum AppStatus {
    Idle,
    Thinking,
    Error(String),
}

pub struct App {
    pub session_id: String,
    pub chat: ChatPanel,
    pub input: InputArea,
    pub status: AppStatus,
    pub should_quit: bool,
    store: Store,
}

impl App {
    pub async fn new(store: Store) -> Result<Self> {
        let session_id = Uuid::new_v4().to_string();
        store.create_session(&session_id, Some("新会话")).await?;
        let messages = store.list_messages(&session_id).await.unwrap_or_default();

        info!("创建新会话: {}", session_id);
        Ok(Self {
            session_id,
            chat: ChatPanel::new(messages),
            input: InputArea::new(),
            status: AppStatus::Idle,
            should_quit: false,
            store,
        })
    }

    pub async fn load_session(store: Store, session_id: &str) -> Result<Self> {
        if store.get_session(session_id).await?.is_none() {
            store.create_session(session_id, Some("恢复会话")).await?;
        }
        let messages = store.list_messages(session_id).await?;

        info!("加载会话: {}", session_id);
        Ok(Self {
            session_id: session_id.to_string(),
            chat: ChatPanel::new(messages),
            input: InputArea::new(),
            status: AppStatus::Idle,
            should_quit: false,
            store,
        })
    }

    pub async fn run<B: Backend>(
        mut self,
        terminal: &mut Terminal<B>,
    ) -> Result<()> {
        while !self.should_quit {
            terminal.draw(|f| self.draw(f))?;
            if event::poll(Duration::from_millis(100))? {
                if let Event::Key(key) = event::read()? {
                    self.handle_key(key).await?;
                }
            }
        }
        Ok(())
    }

    async fn handle_key(
        &mut self,
        key: KeyEvent,
    ) -> Result<()> {
        if key.kind != KeyEventKind::Press {
            return Ok(());
        }

        match key.code {
            KeyCode::Esc => {
                self.should_quit = true;
            }
            KeyCode::Enter => {
                if key.modifiers.contains(KeyModifiers::SHIFT) {
                    self.input.insert_newline();
                } else if !self.input.is_empty() {
                    self.send_message().await?;
                }
            }
            KeyCode::Char(c) => {
                if key.modifiers.contains(KeyModifiers::CONTROL) && c == 'c' {
                    self.should_quit = true;
                } else {
                    self.input.insert_char(c);
                }
            }
            KeyCode::Backspace => {
                self.input.backspace();
            }
            KeyCode::Delete => {
                self.input.delete_char();
            }
            KeyCode::Left => {
                self.input.move_left();
            }
            KeyCode::Right => {
                self.input.move_right();
            }
            KeyCode::Up => {
                if key.modifiers.contains(KeyModifiers::SHIFT) {
                    self.chat.scroll_up(3);
                } else {
                    self.input.move_up();
                }
            }
            KeyCode::Down => {
                if key.modifiers.contains(KeyModifiers::SHIFT) {
                    self.chat.scroll_down(3);
                } else {
                    self.input.move_down();
                }
            }
            KeyCode::Home => {
                self.input.move_home();
            }
            KeyCode::End => {
                self.input.move_end();
            }
            _ => {}
        }
        Ok(())
    }

    async fn send_message(&mut self,
    ) -> Result<()> {
        let text = self.input.text().trim().to_string();
        if text.is_empty() {
            return Ok(());
        }

        self.input.clear();
        let user_msg = self.store.add_message(&self.session_id, "user", &text).await?;
        self.chat.push_message(user_msg);

        self.status = AppStatus::Thinking;
        // 阶段 0 先简单 echo，阶段 1 接入 LLM
        let reply = format!("收到：{}\n\n（阶段 0 尚未接入 LLM，仅做 echo 演示）", text);
        let assistant_msg = self
            .store
            .add_message(&self.session_id, "assistant", &reply)
            .await?;
        self.chat.push_message(assistant_msg);
        self.status = AppStatus::Idle;

        Ok(())
    }

    fn draw(&self,
        frame: &mut Frame,
    ) {
        let chunks = Layout::default()
            .direction(Direction::Vertical)
            .margin(1)
            .constraints([
                Constraint::Min(10),
                Constraint::Length(6),
                Constraint::Length(1),
            ])
            .split(frame.area());

        frame.render_widget(&self.chat, chunks[0]);
        frame.render_widget(&self.input, chunks[1]);

        let status_text = match &self.status {
            AppStatus::Idle => "就绪 | Esc 退出 | Enter 发送 | Shift+Enter 换行 | Shift+↑/↓ 滚动聊天".to_string(),
            AppStatus::Thinking => "思考中...".to_string(),
            AppStatus::Error(e) => format!("错误: {}", e),
        };
        let status = Paragraph::new(Line::from(status_text))
            .style(Style::default().fg(Color::Gray));
        frame.render_widget(status, chunks[2]);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::Store;
    use tempfile::TempDir;

    async fn create_test_app() -> (App, TempDir) {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("app.db");
        let store = Store::open(&path).await.unwrap();
        let app = App::new(store).await.unwrap();
        (app, dir)
    }

    #[tokio::test]
    async fn test_new_app_has_session() {
        let (app, _dir) = create_test_app().await;
        assert!(!app.session_id.is_empty());
        assert!(app.input.is_empty());
        assert!(matches!(app.status, AppStatus::Idle));
    }
}
