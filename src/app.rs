use anyhow::Result;
use crossterm::event::{self, Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use ratatui::{
    Frame, Terminal,
    backend::Backend,
    layout::{Constraint, Direction, Layout},
    style::{Color, Style},
    text::Line,
    widgets::Paragraph,
};
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::{Mutex, mpsc};
use tracing::info;
use uuid::Uuid;

use crate::agent::{
    llm::LlmClient,
    runner::{ReActRunner, RunnerEvent},
    session::SessionContext,
};
use crate::store::Store;
use crate::tools::registry::ToolRegistry;
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
    pub tool_events: Vec<RunnerEvent>,
    store: Store,
    runner: ReActRunner,
    session_ctx: SessionContext,
}

impl App {
    pub async fn new(
        store: Store,
        client: Arc<dyn LlmClient>,
        registry: Arc<Mutex<ToolRegistry>>,
    ) -> Result<Self> {
        let session_id = Uuid::new_v4().to_string();
        store.create_session(&session_id, Some("新会话")).await?;
        let messages = store.list_messages(&session_id).await.unwrap_or_default();

        info!("创建新会话: {}", session_id);
        let session_ctx = SessionContext::new(build_system_prompt());
        let runner = ReActRunner::new(client, registry);

        Ok(Self {
            session_id,
            chat: ChatPanel::new(messages),
            input: InputArea::new(),
            status: AppStatus::Idle,
            should_quit: false,
            tool_events: Vec::new(),
            store,
            runner,
            session_ctx,
        })
    }

    pub async fn load_session(
        store: Store,
        session_id: &str,
        client: Arc<dyn LlmClient>,
        registry: Arc<Mutex<ToolRegistry>>,
    ) -> Result<Self> {
        if store.get_session(session_id).await?.is_none() {
            store.create_session(session_id, Some("恢复会话")).await?;
        }
        let messages = store.list_messages(session_id).await?;

        info!("加载会话: {}", session_id);
        let session_ctx = SessionContext::new(build_system_prompt());
        let runner = ReActRunner::new(client, registry);

        Ok(Self {
            session_id: session_id.to_string(),
            chat: ChatPanel::new(messages),
            input: InputArea::new(),
            status: AppStatus::Idle,
            should_quit: false,
            tool_events: Vec::new(),
            store,
            runner,
            session_ctx,
        })
    }

    pub async fn run<B: Backend>(mut self, terminal: &mut Terminal<B>) -> Result<()> {
        while !self.should_quit {
            terminal.draw(|f| self.draw(f))?;
            if event::poll(Duration::from_millis(100))?
                && let Event::Key(key) = event::read()?
            {
                self.handle_key(key).await?;
            }
        }
        Ok(())
    }

    async fn handle_key(&mut self, key: KeyEvent) -> Result<()> {
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

    async fn send_message(&mut self) -> Result<()> {
        let text = self.input.text().trim().to_string();
        if text.is_empty() {
            return Ok(());
        }

        self.input.clear();
        let user_msg = self
            .store
            .add_message(&self.session_id, "user", &text)
            .await?;
        self.chat.push_message(user_msg);

        self.status = AppStatus::Thinking;
        self.tool_events.clear();

        let (event_tx, mut event_rx) = mpsc::unbounded_channel::<RunnerEvent>();

        // 在后台运行 LLM，同时接收工具调用事件
        let result = {
            let runner = &self.runner;
            let session_ctx = &mut self.session_ctx;
            runner.run(&mut *session_ctx, &text, Some(event_tx)).await
        };

        // 处理已接收的事件
        while let Ok(event) = event_rx.try_recv() {
            self.tool_events.push(event.clone());
            if let RunnerEvent::ToolCall { name, .. } = &event {
                let tool_msg = self
                    .store
                    .add_message(&self.session_id, "tool", &format!("调用工具: {}", name))
                    .await?;
                self.chat.push_message(tool_msg);
            }
        }

        let reply = match result {
            Ok(text) => text,
            Err(e) => {
                let err = format!("处理失败: {}", e);
                self.status = AppStatus::Error(err.clone());
                err
            }
        };

        let assistant_msg = self
            .store
            .add_message(&self.session_id, "assistant", &reply)
            .await?;
        self.chat.push_message(assistant_msg);
        self.status = AppStatus::Idle;

        Ok(())
    }

    fn draw(&self, frame: &mut Frame) {
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
            AppStatus::Idle => {
                if self.tool_events.is_empty() {
                    "就绪 | Esc 退出 | Enter 发送 | Shift+Enter 换行 | Shift+↑/↓ 滚动聊天"
                        .to_string()
                } else {
                    format!("最近工具调用: {} 次", self.tool_events.len())
                }
            }
            AppStatus::Thinking => "思考中...".to_string(),
            AppStatus::Error(e) => format!("错误: {}", e),
        };
        let status =
            Paragraph::new(Line::from(status_text)).style(Style::default().fg(Color::Gray));
        frame.render_widget(status, chunks[2]);
    }
}

fn build_system_prompt() -> String {
    r#"你是一个终端办公 AI Agent，名为 Clerk。
你可以使用以下工具帮助用户：
- fs_read: 读取文件内容
- fs_write: 写入文件内容
- fs_list: 列出目录内容
- shell: 执行 shell 命令
请根据用户需求判断是否需要调用工具，并简洁地回复。"#
        .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent::llm::{LlmResponse, Message, ToolDefinition};
    use crate::store::Store;
    use crate::tools::schema::{Tool, ToolContext, ToolResult, ToolSchema};
    use async_trait::async_trait;
    use serde_json::Value;
    use std::collections::HashMap;
    use tempfile::TempDir;

    struct FakeLlm;

    #[async_trait]
    impl LlmClient for FakeLlm {
        async fn chat(
            &self,
            _messages: Vec<Message>,
            _tools: Vec<ToolDefinition>,
        ) -> Result<LlmResponse> {
            Ok(LlmResponse::Text("fake reply".to_string()))
        }
    }

    struct FakeTool;

    #[async_trait]
    impl Tool for FakeTool {
        fn name(&self) -> &str {
            "fake"
        }
        fn description(&self) -> &str {
            "fake"
        }
        fn schema(&self) -> ToolSchema {
            ToolSchema::new("fake", "fake")
        }
        async fn execute(
            &self,
            _args: HashMap<String, Value>,
            _ctx: &ToolContext,
        ) -> Result<ToolResult> {
            Ok(ToolResult::Text("done".to_string()))
        }
    }

    async fn create_test_app() -> (App, TempDir) {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("app.db");
        let store = Store::open(&path).await.unwrap();
        let client: Arc<dyn LlmClient> = Arc::new(FakeLlm);
        let mut registry = ToolRegistry::new(ToolContext::default());
        registry.register(Box::new(FakeTool));
        let app = App::new(store, client, Arc::new(Mutex::new(registry)))
            .await
            .unwrap();
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
