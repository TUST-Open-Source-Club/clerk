use anyhow::Result;
use chrono::Utc;
use crossterm::event::{self, Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use ratatui::{
    Frame, Terminal,
    backend::Backend,
    layout::{Constraint, Direction, Layout, Position},
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
use crate::store::{Message, Store};
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
    pub yolo_mode: bool,
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
            yolo_mode: false,
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
            yolo_mode: false,
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
            KeyCode::Enter => {
                if key.modifiers.contains(KeyModifiers::SHIFT) {
                    self.input.insert_newline();
                } else if !self.input.is_empty() {
                    let text = self.input.text();
                    if text.starts_with('/') {
                        self.handle_command().await?;
                    } else {
                        self.send_message().await?;
                    }
                }
            }
            KeyCode::Tab => {
                self.input.autocomplete();
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

    async fn handle_command(&mut self) -> Result<()> {
        let text = self.input.text();
        self.input.clear();
        let parts: Vec<&str> = text.split_whitespace().collect();
        let cmd = parts.first().copied().unwrap_or("");

        match cmd {
            "/exit" => {
                self.should_quit = true;
            }
            "/help" => {
                self.chat.push_message(system_message(
                    &self.session_id,
                    "可用命令：\n\
                     /help    显示帮助\n\
                     /exit    退出应用\n\
                     /clear   清空聊天\n\
                     /yolo    切换 YOLO 模式（无需工具确认）\n\
                     /sessions 列出最近会话",
                ));
            }
            "/clear" => {
                self.chat.clear();
                self.tool_events.clear();
            }
            "/yolo" => {
                self.yolo_mode = !self.yolo_mode;
                let status = if self.yolo_mode { "开启" } else { "关闭" };
                self.chat.push_message(system_message(
                    &self.session_id,
                    &format!("YOLO 模式已{}", status),
                ));
            }
            "/sessions" => {
                let content = match self.store.list_sessions().await {
                    Ok(sessions) if sessions.is_empty() => "暂无会话".to_string(),
                    Ok(sessions) => {
                        let lines: Vec<String> = sessions
                            .iter()
                            .map(|s| {
                                format!("- {} ({})", s.id, s.title.as_deref().unwrap_or("无标题"))
                            })
                            .collect();
                        format!("最近会话：\n{}", lines.join("\n"))
                    }
                    Err(e) => format!("获取会话列表失败: {:#}", e),
                };
                self.chat
                    .push_message(system_message(&self.session_id, &content));
            }
            _ => {
                self.chat.push_message(system_message(
                    &self.session_id,
                    &format!("未知命令: {}", cmd),
                ));
            }
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
                let err = format!("处理失败: {:#}", e);
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

        let (cursor_x, cursor_y) = self.input.cursor_screen_pos(chunks[1]);
        frame.set_cursor_position(Position::new(
            chunks[1].x + cursor_x,
            chunks[1].y + cursor_y,
        ));

        let status_text = match &self.status {
            AppStatus::Idle => {
                if self.tool_events.is_empty() {
                    "就绪 | /exit 退出 | Enter 发送 | Shift+Enter 换行 | Shift+↑/↓ 滚动聊天"
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
- web_fetch: 获取网页内容
- web_post: 发送 POST 请求
- browser: 使用无头 Chromium 浏览器操作网页、生成 PDF/截图
- office_read_excel / office_write_excel: Excel 读写
- office_read_word / office_write_word: Word 读写
- office_render: 使用 Pandoc 渲染复杂 Word/PDF/PPT（支持模板、公式、图片）
- pdf_merge / pdf_split: PDF 合并与拆分
- poster: HTML 转海报 PDF/PNG
- subagent_create / subagent_run / subagent_list / subagent_delete: 创建并运行子 Agent
- collaborate_parallel / collaborate_sequential: 多子 Agent 并行/顺序协作
- write_skill: 将领域知识保存为 SKILL.md，供后续复用
请根据用户需求判断是否需要调用工具，并简洁地回复。"#
        .to_string()
}

fn system_message(session_id: &str, content: &str) -> Message {
    Message {
        id: 0,
        session_id: session_id.to_string(),
        role: "system".to_string(),
        content: content.to_string(),
        created_at: Utc::now(),
    }
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

    struct ReplyingFakeLlm {
        reply: String,
    }

    #[async_trait]
    impl LlmClient for ReplyingFakeLlm {
        async fn chat(
            &self,
            _messages: Vec<Message>,
            _tools: Vec<ToolDefinition>,
        ) -> Result<LlmResponse> {
            Ok(LlmResponse::Text(self.reply.clone()))
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
        registry.register(Arc::new(FakeTool));
        let app = App::new(store, client, Arc::new(Mutex::new(registry)))
            .await
            .unwrap();
        (app, dir)
    }

    async fn type_text(app: &mut App, text: &str) {
        for c in text.chars() {
            app.handle_key(KeyEvent::from(KeyCode::Char(c)))
                .await
                .unwrap();
        }
    }

    async fn press_enter(app: &mut App) {
        app.handle_key(KeyEvent::from(KeyCode::Enter))
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn test_load_session() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("app.db");
        let store = Store::open(&path).await.unwrap();
        let session_id = "sess-123";
        store
            .create_session(session_id, Some("test"))
            .await
            .unwrap();

        let client: Arc<dyn LlmClient> = Arc::new(FakeLlm);
        let mut registry = ToolRegistry::new(ToolContext::default());
        registry.register(Arc::new(FakeTool));
        let app = App::load_session(store, session_id, client, Arc::new(Mutex::new(registry)))
            .await
            .unwrap();
        assert_eq!(app.session_id, session_id);
    }

    #[tokio::test]
    async fn test_app_new() {
        let (app, _dir) = create_test_app().await;
        assert!(!app.session_id.is_empty());
        let messages = app.store.list_messages(&app.session_id).await.unwrap();
        assert!(messages.is_empty());
    }

    #[tokio::test]
    async fn test_send_message() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("app.db");
        let store = Store::open(&path).await.unwrap();
        let client: Arc<dyn LlmClient> = Arc::new(ReplyingFakeLlm {
            reply: "reply".to_string(),
        });
        let registry = ToolRegistry::new(ToolContext::default());
        let mut app = App::new(store, client, Arc::new(Mutex::new(registry)))
            .await
            .unwrap();

        app.input.insert_char('h');
        app.input.insert_char('i');
        app.send_message().await.unwrap();

        let messages = app.store.list_messages(&app.session_id).await.unwrap();
        assert!(
            messages
                .iter()
                .any(|m| m.role == "user" && m.content == "hi")
        );
        assert!(
            messages
                .iter()
                .any(|m| m.role == "assistant" && m.content == "reply")
        );
    }

    #[tokio::test]
    async fn test_handle_key_chars() {
        let (mut app, _dir) = create_test_app().await;
        app.handle_key(KeyEvent::from(KeyCode::Char('a')))
            .await
            .unwrap();
        app.handle_key(KeyEvent::from(KeyCode::Char('b')))
            .await
            .unwrap();
        assert_eq!(app.input.text(), "ab");
    }

    #[tokio::test]
    async fn test_handle_key_backspace() {
        let (mut app, _dir) = create_test_app().await;
        app.handle_key(KeyEvent::from(KeyCode::Char('a')))
            .await
            .unwrap();
        app.handle_key(KeyEvent::from(KeyCode::Char('b')))
            .await
            .unwrap();
        app.handle_key(KeyEvent::from(KeyCode::Backspace))
            .await
            .unwrap();
        assert_eq!(app.input.text(), "a");
    }

    #[tokio::test]
    async fn test_handle_key_quit_with_ctrl_c() {
        let (mut app, _dir) = create_test_app().await;
        assert!(!app.should_quit);
        let key = KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL);
        app.handle_key(key).await.unwrap();
        assert!(app.should_quit);
    }

    #[tokio::test]
    async fn test_handle_key_exit_command() {
        let (mut app, _dir) = create_test_app().await;
        type_text(&mut app, "/exit").await;
        press_enter(&mut app).await;
        assert!(app.should_quit);
    }

    #[tokio::test]
    async fn test_handle_key_help_command() {
        let (mut app, _dir) = create_test_app().await;
        type_text(&mut app, "/help").await;
        press_enter(&mut app).await;
        let last = app.chat.messages().last().unwrap();
        assert_eq!(last.role, "system");
        assert!(last.content.contains("/help"));
        assert!(app.input.is_empty());
    }

    #[tokio::test]
    async fn test_handle_key_clear_command() {
        let (mut app, _dir) = create_test_app().await;
        type_text(&mut app, "/help").await;
        press_enter(&mut app).await;
        assert!(!app.chat.messages().is_empty());

        type_text(&mut app, "/clear").await;
        press_enter(&mut app).await;
        assert!(app.chat.messages().is_empty());
        assert!(app.tool_events.is_empty());
        assert!(app.input.is_empty());
    }

    #[tokio::test]
    async fn test_handle_key_yolo_command() {
        let (mut app, _dir) = create_test_app().await;
        type_text(&mut app, "/yolo").await;
        press_enter(&mut app).await;
        assert!(app.yolo_mode);
        let last = app.chat.messages().last().unwrap();
        assert!(last.content.contains("YOLO"));

        type_text(&mut app, "/yolo").await;
        press_enter(&mut app).await;
        assert!(!app.yolo_mode);
    }

    #[tokio::test]
    async fn test_handle_key_sessions_command() {
        let (mut app, _dir) = create_test_app().await;
        type_text(&mut app, "/sessions").await;
        press_enter(&mut app).await;
        let last = app.chat.messages().last().unwrap();
        assert_eq!(last.role, "system");
        assert!(last.content.contains(&app.session_id));
    }

    #[tokio::test]
    async fn test_handle_key_unknown_command() {
        let (mut app, _dir) = create_test_app().await;
        type_text(&mut app, "/unknown").await;
        press_enter(&mut app).await;
        let last = app.chat.messages().last().unwrap();
        assert_eq!(last.role, "system");
        assert!(last.content.contains("/unknown"));
    }

    #[tokio::test]
    async fn test_draw_does_not_panic() {
        let (app, _dir) = create_test_app().await;
        let backend = ratatui::backend::TestBackend::new(80, 24);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal.draw(|f| app.draw(f)).unwrap();
        let buffer = terminal.backend().buffer();
        let has_content = buffer.content.iter().any(|c| !c.symbol().is_empty());
        assert!(has_content);
    }

    #[test]
    fn test_build_system_prompt_contains_tools() {
        let prompt = build_system_prompt();
        assert!(prompt.contains("subagent_create"));
        assert!(prompt.contains("write_skill"));
    }
}
