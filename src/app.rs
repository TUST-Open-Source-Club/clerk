use anyhow::Result;
use chrono::Utc;
use crossterm::event::{Event, EventStream, KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use ratatui::{
    Frame, Terminal,
    backend::Backend,
    layout::{Constraint, Direction, Layout, Position},
    style::{Color, Style},
    text::Line,
    widgets::Paragraph,
};
use serde_json::Value;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::{Mutex, mpsc};
use tokio_stream::StreamExt;
use tracing::info;
use uuid::Uuid;

use crate::agent::{
    llm::{LlmClient, StreamChunk},
    runner::{ReActRunner, RunnerEvent},
    session::SessionContext,
};
use crate::config::MultimodalConfig;
use crate::store::{Message, Store};
use crate::tools::media::read_media_file;
use crate::tools::registry::ToolRegistry;
use crate::ui::chat::ChatPanel;
use crate::ui::input::InputArea;

#[derive(Debug, Clone, PartialEq)]
pub enum AppStatus {
    Idle,
    Streaming,
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
    pub multimodal: MultimodalConfig,
    pub attachments: Vec<PathBuf>,
    pub streaming_reply: String,
    store: Store,
    runner: ReActRunner,
    session_ctx: Arc<Mutex<SessionContext>>,
    working_dir: PathBuf,
    stream_rx: Option<mpsc::UnboundedReceiver<StreamEvent>>,
    stream_abort: Option<tokio::task::AbortHandle>,
    streaming_message_added: bool,
    spinner_frame: usize,
}

#[derive(Debug)]
enum StreamEvent {
    Chunk(StreamChunk),
    ToolEvent(RunnerEvent),
    Done(Result<String>),
}

impl App {
    pub async fn new(
        store: Store,
        client: Arc<dyn LlmClient>,
        registry: Arc<Mutex<ToolRegistry>>,
        multimodal: MultimodalConfig,
    ) -> Result<Self> {
        let session_id = Uuid::new_v4().to_string();
        store.create_session(&session_id, Some("新会话")).await?;
        let messages = store.list_messages(&session_id).await.unwrap_or_default();
        let working_dir = registry.lock().await.context().working_dir.clone();

        info!("创建新会话: {}", session_id);
        let session_ctx = Arc::new(Mutex::new(SessionContext::new(build_system_prompt())));
        let runner = ReActRunner::new(client, registry);

        Ok(Self {
            session_id,
            chat: ChatPanel::new(messages),
            input: InputArea::new(),
            status: AppStatus::Idle,
            should_quit: false,
            tool_events: Vec::new(),
            yolo_mode: false,
            multimodal,
            attachments: Vec::new(),
            streaming_reply: String::new(),
            store,
            runner,
            session_ctx,
            working_dir,
            stream_rx: None,
            stream_abort: None,
            streaming_message_added: false,
            spinner_frame: 0,
        })
    }

    pub async fn load_session(
        store: Store,
        session_id: &str,
        client: Arc<dyn LlmClient>,
        registry: Arc<Mutex<ToolRegistry>>,
        multimodal: MultimodalConfig,
    ) -> Result<Self> {
        if store.get_session(session_id).await?.is_none() {
            store.create_session(session_id, Some("恢复会话")).await?;
        }
        let messages = store.list_messages(session_id).await?;
        let working_dir = registry.lock().await.context().working_dir.clone();

        info!("加载会话: {}", session_id);
        let session_ctx = Arc::new(Mutex::new(SessionContext::new(build_system_prompt())));
        let runner = ReActRunner::new(client, registry);

        Ok(Self {
            session_id: session_id.to_string(),
            chat: ChatPanel::new(messages),
            input: InputArea::new(),
            status: AppStatus::Idle,
            should_quit: false,
            tool_events: Vec::new(),
            yolo_mode: false,
            multimodal,
            attachments: Vec::new(),
            streaming_reply: String::new(),
            store,
            runner,
            session_ctx,
            working_dir,
            stream_rx: None,
            stream_abort: None,
            streaming_message_added: false,
            spinner_frame: 0,
        })
    }

    pub async fn run<B: Backend>(mut self, terminal: &mut Terminal<B>) -> Result<()> {
        let mut reader = EventStream::new();
        let mut tick = tokio::time::interval(Duration::from_millis(100));

        while !self.should_quit {
            terminal.draw(|f| self.draw(f))?;

            tokio::select! {
                maybe_event = reader.next() => {
                    if let Some(Ok(Event::Key(key))) = maybe_event {
                        self.handle_key(key).await?;
                    }
                }
                maybe_event = async {
                    if let Some(rx) = self.stream_rx.as_mut() {
                        rx.recv().await
                    } else {
                        std::future::pending::<Option<StreamEvent>>().await
                    }
                } => {
                    if let Some(event) = maybe_event {
                        self.handle_stream_event(event).await?;
                    }
                }
                _ = tick.tick() => {
                    if self.status == AppStatus::Streaming {
                        self.spinner_frame = self.spinner_frame.wrapping_add(1);
                    }
                }
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
                    } else if self.try_handle_pasted_media_path().await? {
                        // 已作为媒体附件发送
                    } else {
                        self.send_message().await?;
                    }
                }
            }
            KeyCode::Tab => {
                self.input.autocomplete();
            }
            KeyCode::Char(c) => {
                if key.modifiers.contains(KeyModifiers::CONTROL)
                    && c == 'c'
                    && self.status == AppStatus::Streaming
                {
                    if let Some(abort) = self.stream_abort.take() {
                        abort.abort();
                    }
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

    async fn try_handle_pasted_media_path(&mut self) -> Result<bool> {
        let text = self.input.text();
        let trimmed = text.trim();
        if trimmed.contains('\n') || trimmed.is_empty() {
            return Ok(false);
        }

        let path = self.resolve_relative_path(trimmed);
        if !path.exists() {
            return Ok(false);
        }

        let kind = media_kind(&path);
        let is_image = kind == Some("image");
        let is_video = kind == Some("video");
        if !is_image && !is_video {
            return Ok(false);
        }

        if is_image && !self.multimodal.supports_images {
            return Ok(false);
        }
        if is_video && !self.multimodal.supports_video {
            return Ok(false);
        }

        self.attachments.push(path);
        self.input.clear();
        self.input.insert_str("请分析这张图片");
        self.send_message().await?;
        Ok(true)
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
                     /sessions 列出最近会话\n\
                     /attach <path>    附加图片/视频到下一次消息\n\
                     /attachments      列出已附加的媒体\n\
                     /clear_attachments 清除已附加的媒体",
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
            "/attach" => {
                let arg = text.strip_prefix("/attach").unwrap_or("").trim();
                if arg.is_empty() {
                    self.chat
                        .push_message(system_message(&self.session_id, "用法: /attach <path>"));
                } else {
                    self.attach_file(arg).await?;
                }
            }
            "/attachments" => {
                let content = if self.attachments.is_empty() {
                    "当前没有附件".to_string()
                } else {
                    let lines: Vec<String> = self
                        .attachments
                        .iter()
                        .map(|p| format!("- {}", p.display()))
                        .collect();
                    format!("当前附件：\n{}", lines.join("\n"))
                };
                self.chat
                    .push_message(system_message(&self.session_id, &content));
            }
            "/clear_attachments" => {
                self.attachments.clear();
                self.chat
                    .push_message(system_message(&self.session_id, "已清除所有附件"));
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

    async fn attach_file(&mut self, arg: &str) -> Result<()> {
        let path = self.resolve_relative_path(arg);
        if !path.exists() {
            self.chat.push_message(system_message(
                &self.session_id,
                &format!("文件不存在: {}", path.display()),
            ));
            return Ok(());
        }

        let kind = media_kind(&path);
        let is_image = kind == Some("image");
        let is_video = kind == Some("video");
        if !is_image && !is_video {
            self.chat
                .push_message(system_message(&self.session_id, "仅支持图片或视频附件"));
            return Ok(());
        }

        let mut warnings = Vec::new();
        if is_image && !self.multimodal.supports_images {
            warnings.push("当前模型未启用图片输入支持".to_string());
        }
        if is_video && !self.multimodal.supports_video {
            warnings.push("当前模型未启用视频输入支持".to_string());
        }

        self.attachments.push(path.clone());
        let mut msg = format!("已添加附件: {}", path.display());
        if !warnings.is_empty() {
            msg.push_str("\n注意: ");
            msg.push_str(&warnings.join("，"));
        }
        self.chat
            .push_message(system_message(&self.session_id, &msg));
        Ok(())
    }

    fn resolve_relative_path(&self, input: &str) -> PathBuf {
        let path = PathBuf::from(input);
        if path.is_absolute() {
            path
        } else {
            self.working_dir.join(path)
        }
    }

    async fn send_message(&mut self) -> Result<()> {
        let mut text = self.input.text().trim().to_string();
        if text.is_empty() {
            return Ok(());
        }

        self.input.clear();

        if !self.attachments.is_empty() {
            let mut descriptions = Vec::new();
            for path in &self.attachments {
                match read_media_file(path).await {
                    Ok(desc) => descriptions.push(format!("附件 {}:\n{}", path.display(), desc)),
                    Err(e) => descriptions.push(format!("附件 {} 读取失败: {}", path.display(), e)),
                }
            }
            text = format!("{}\n\n{}", text, descriptions.join("\n\n"));
            self.attachments.clear();
        }

        let user_msg = self
            .store
            .add_message(&self.session_id, "user", &text)
            .await?;
        self.chat.push_message(user_msg);

        self.status = AppStatus::Streaming;
        self.tool_events.clear();
        self.streaming_reply.clear();
        self.streaming_message_added = false;
        self.spinner_frame = 0;
        self.stream_abort = None;

        let (stream_tx, stream_rx) = mpsc::unbounded_channel::<StreamEvent>();
        self.stream_rx = Some(stream_rx);

        let ctx = self.session_ctx.clone();
        let runner = self.runner.clone();
        let text_clone = text.clone();

        // 先创建实际的 LLM runner task，以便把 AbortHandle 交给主循环
        let (event_tx, mut event_rx) = mpsc::unbounded_channel::<RunnerEvent>();
        let (chunk_tx, mut chunk_rx) = mpsc::unbounded_channel::<StreamChunk>();
        let stream_handle = tokio::spawn(async move {
            runner
                .run_stream(ctx, &text_clone, chunk_tx, Some(event_tx))
                .await
        });
        self.stream_abort = Some(stream_handle.abort_handle());

        tokio::spawn(async move {
            loop {
                tokio::select! {
                    Some(chunk) = chunk_rx.recv() => {
                        let _ = stream_tx.send(StreamEvent::Chunk(chunk));
                    }
                    Some(event) = event_rx.recv() => {
                        let _ = stream_tx.send(StreamEvent::ToolEvent(event));
                    }
                    else => break,
                }
            }

            let result = stream_handle.await.unwrap_or_else(|e| {
                if e.is_cancelled() {
                    Err(anyhow::anyhow!("生成已取消"))
                } else {
                    Err(anyhow::anyhow!(e))
                }
            });
            let _ = stream_tx.send(StreamEvent::Done(result));
        });

        Ok(())
    }

    async fn handle_stream_event(&mut self, event: StreamEvent) -> Result<()> {
        match event {
            StreamEvent::Chunk(chunk) => {
                if let Some(reasoning) = chunk.reasoning_content {
                    self.streaming_reply.push_str("<think>");
                    self.streaming_reply.push_str(&reasoning);
                    self.streaming_reply.push_str("</think>");
                }
                if let Some(content) = chunk.content {
                    self.streaming_reply.push_str(&content);
                }
                if !self.streaming_message_added {
                    self.chat.push_message(Message {
                        id: 0,
                        session_id: self.session_id.clone(),
                        role: "assistant".to_string(),
                        content: self.streaming_reply.clone(),
                        created_at: Utc::now(),
                    });
                    self.streaming_message_added = true;
                } else {
                    self.chat.update_last_message(&self.streaming_reply);
                }
            }
            StreamEvent::ToolEvent(event) => {
                self.tool_events.push(event.clone());
                let content = format_tool_event(&event);
                let tool_msg = self
                    .store
                    .add_message(&self.session_id, "tool", &content)
                    .await?;
                self.chat.push_message(tool_msg);
            }
            StreamEvent::Done(result) => {
                self.stream_abort = None;
                if self.streaming_message_added {
                    self.chat.pop_last();
                }

                let reply = match result {
                    Ok(text) => text,
                    Err(e) => {
                        let msg = e.to_string();
                        if msg.contains("生成已取消") || msg.contains("cancelled") {
                            self.status = AppStatus::Idle;
                            "生成已取消".to_string()
                        } else {
                            let err = format!("处理失败: {:#}", e);
                            self.status = AppStatus::Error(err.clone());
                            err
                        }
                    }
                };

                let assistant_msg = self
                    .store
                    .add_message(&self.session_id, "assistant", &reply)
                    .await?;
                self.chat.push_message(assistant_msg);
                self.streaming_reply.clear();
                self.streaming_message_added = false;
                self.stream_rx = None;
                if self.status == AppStatus::Streaming {
                    self.status = AppStatus::Idle;
                }
            }
        }
        Ok(())
    }

    #[cfg(test)]
    async fn drain_stream(&mut self) {
        if let Some(mut rx) = self.stream_rx.take() {
            let mut done = false;
            while !done {
                match rx.recv().await {
                    Some(event) => {
                        done = matches!(event, StreamEvent::Done(_));
                        if self.handle_stream_event(event).await.is_err() {
                            break;
                        }
                    }
                    None => break,
                }
            }
        }
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
            AppStatus::Streaming => {
                format!("{} 思考中...", spinner_char(self.spinner_frame))
            }
            AppStatus::Error(e) => format!("错误: {}", e),
        };
        let status =
            Paragraph::new(Line::from(status_text)).style(Style::default().fg(Color::Gray));
        frame.render_widget(status, chunks[2]);
    }
}

fn spinner_char(frame: usize) -> char {
    const FRAMES: [char; 10] = ['⠋', '⠙', '⠹', '⠸', '⠼', '⠴', '⠦', '⠧', '⠇', '⠏'];
    FRAMES[frame % FRAMES.len()]
}

fn format_tool_event(event: &RunnerEvent) -> String {
    match event {
        RunnerEvent::ToolCall { name, arguments } => {
            let args = format_tool_arguments(name, arguments);
            format!("调用工具 {}: {}", name, args)
        }
        RunnerEvent::ToolResult { name, result } => {
            let summary = result.chars().take(200).collect::<String>();
            let ellipsis = if result.chars().count() > 200 {
                " ..."
            } else {
                ""
            };
            format!("工具 {} 结果: {}{}", name, summary, ellipsis)
        }
        RunnerEvent::Error(e) => format!("工具错误: {}", e),
    }
}

fn format_tool_arguments(name: &str, arguments: &Value) -> String {
    match arguments.as_object() {
        Some(map) => {
            let parts: Vec<String> = map
                .iter()
                .map(|(k, v)| format!("{}={}", k, format_arg_value(name, k, v)))
                .collect();
            if parts.is_empty() {
                "(无参数)".to_string()
            } else {
                parts.join(", ")
            }
        }
        None => arguments.to_string(),
    }
}

fn format_arg_value(tool_name: &str, key: &str, value: &serde_json::Value) -> String {
    // shell 命令、文件内容等长字段需要截断
    let is_long_field = matches!(
        (tool_name, key),
        ("shell", "command")
            | ("fs_write", "content")
            | ("web_fetch", "url")
            | ("web_post", "url")
            | ("browser", "url")
            | ("poster", "input")
    );

    let s = value.to_string();
    if is_long_field && s.len() > 120 {
        format!("{}...（共 {} 字符）", &s[..120], s.len())
    } else {
        s
    }
}

fn media_kind(path: &Path) -> Option<&'static str> {
    let mime = infer::get_from_path(path)
        .ok()
        .flatten()
        .map(|k| k.mime_type().to_string())
        .or_else(|| {
            let ext = path.extension().and_then(|e| e.to_str())?.to_lowercase();
            match ext.as_str() {
                "png" | "jpg" | "jpeg" | "gif" | "webp" => Some("image/unknown".to_string()),
                "mp4" | "webm" | "mov" | "avi" | "mkv" => Some("video/unknown".to_string()),
                _ => None,
            }
        })?;
    if mime.starts_with("image/") {
        Some("image")
    } else if mime.starts_with("video/") {
        Some("video")
    } else {
        None
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
- read_media_file: 读取图片/视频文件并返回 base64 数据 URL
- render_to_image: 将 HTML/PDF/Office/图片渲染为 PNG 预览图
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

    struct StreamingFakeLlm {
        chunks: Vec<String>,
    }

    #[async_trait]
    impl LlmClient for StreamingFakeLlm {
        async fn chat(
            &self,
            _messages: Vec<Message>,
            _tools: Vec<ToolDefinition>,
        ) -> Result<LlmResponse> {
            Ok(LlmResponse::Text(self.chunks.join("")))
        }

        async fn chat_stream(
            &self,
            _messages: Vec<Message>,
            _tools: Vec<ToolDefinition>,
        ) -> anyhow::Result<
            Box<dyn tokio_stream::Stream<Item = anyhow::Result<StreamChunk>> + Send + Unpin>,
        > {
            let chunks: Vec<anyhow::Result<StreamChunk>> = self
                .chunks
                .iter()
                .cloned()
                .map(|s| {
                    Ok(StreamChunk {
                        content: Some(s),
                        reasoning_content: None,
                    })
                })
                .collect();
            Ok(Box::new(tokio_stream::iter(chunks)))
        }
    }

    struct SlowStreamingFakeLlm;

    #[async_trait]
    impl LlmClient for SlowStreamingFakeLlm {
        async fn chat(
            &self,
            _messages: Vec<Message>,
            _tools: Vec<ToolDefinition>,
        ) -> Result<LlmResponse> {
            Ok(LlmResponse::Text("slow reply".to_string()))
        }

        async fn chat_stream(
            &self,
            _messages: Vec<Message>,
            _tools: Vec<ToolDefinition>,
        ) -> anyhow::Result<
            Box<dyn tokio_stream::Stream<Item = anyhow::Result<StreamChunk>> + Send + Unpin>,
        > {
            let (tx, rx) = tokio::sync::mpsc::unbounded_channel::<anyhow::Result<StreamChunk>>();
            tokio::spawn(async move {
                tokio::time::sleep(Duration::from_secs(60)).await;
                let _ = tx.send(Ok(StreamChunk {
                    content: Some("hello".to_string()),
                    reasoning_content: None,
                }));
            });
            Ok(Box::new(
                tokio_stream::wrappers::UnboundedReceiverStream::new(rx),
            ))
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
        let mut registry = ToolRegistry::new(ToolContext {
            working_dir: dir.path().to_path_buf(),
        });
        registry.register(Arc::new(FakeTool));
        let app = App::new(
            store,
            client,
            Arc::new(Mutex::new(registry)),
            MultimodalConfig::default(),
        )
        .await
        .unwrap();
        (app, dir)
    }

    async fn create_multimodal_app() -> (App, TempDir) {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("app.db");
        let store = Store::open(&path).await.unwrap();
        let client: Arc<dyn LlmClient> = Arc::new(FakeLlm);
        let mut registry = ToolRegistry::new(ToolContext {
            working_dir: dir.path().to_path_buf(),
        });
        registry.register(Arc::new(FakeTool));
        let multimodal = MultimodalConfig {
            supports_images: true,
            supports_video: true,
        };
        let app = App::new(store, client, Arc::new(Mutex::new(registry)), multimodal)
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
        app.drain_stream().await;
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
        let mut registry = ToolRegistry::new(ToolContext {
            working_dir: dir.path().to_path_buf(),
        });
        registry.register(Arc::new(FakeTool));
        let app = App::load_session(
            store,
            session_id,
            client,
            Arc::new(Mutex::new(registry)),
            MultimodalConfig::default(),
        )
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
        let registry = ToolRegistry::new(ToolContext {
            working_dir: dir.path().to_path_buf(),
        });
        let mut app = App::new(
            store,
            client,
            Arc::new(Mutex::new(registry)),
            MultimodalConfig::default(),
        )
        .await
        .unwrap();

        app.input.insert_char('h');
        app.input.insert_char('i');
        app.send_message().await.unwrap();
        app.drain_stream().await;

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
    async fn test_handle_key_ctrl_c_when_idle_does_nothing() {
        let (mut app, _dir) = create_test_app().await;
        assert!(!app.should_quit);
        let key = KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL);
        app.handle_key(key).await.unwrap();
        assert!(!app.should_quit, "空闲时 Ctrl+C 不应退出");
    }

    #[tokio::test]
    async fn test_handle_key_ctrl_c_aborts_streaming() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("app.db");
        let store = Store::open(&path).await.unwrap();
        let client: Arc<dyn LlmClient> = Arc::new(SlowStreamingFakeLlm);
        let registry = ToolRegistry::new(ToolContext {
            working_dir: dir.path().to_path_buf(),
        });
        let mut app = App::new(
            store,
            client,
            Arc::new(Mutex::new(registry)),
            MultimodalConfig::default(),
        )
        .await
        .unwrap();

        app.input.insert_char('h');
        app.input.insert_char('i');
        app.send_message().await.unwrap();
        assert_eq!(app.status, AppStatus::Streaming);
        assert!(app.stream_abort.is_some());

        let key = KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL);
        app.handle_key(key).await.unwrap();

        app.drain_stream().await;
        assert_eq!(app.status, AppStatus::Idle);
        let messages = app.store.list_messages(&app.session_id).await.unwrap();
        assert!(
            messages
                .iter()
                .any(|m| m.role == "assistant" && m.content == "生成已取消")
        );
    }

    #[tokio::test]
    async fn test_format_tool_event_shows_details() {
        let event = RunnerEvent::ToolCall {
            name: "fs_write".to_string(),
            arguments: serde_json::json!({
                "path": "/tmp/foo.html",
                "content": "hello world"
            }),
        };
        let text = format_tool_event(&event);
        assert!(text.contains("fs_write"));
        assert!(text.contains("/tmp/foo.html"));

        let event = RunnerEvent::ToolCall {
            name: "shell".to_string(),
            arguments: serde_json::json!({"command": "ls -la"}),
        };
        let text = format_tool_event(&event);
        assert!(text.contains("shell"));
        assert!(text.contains("ls -la"));
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
        assert!(prompt.contains("read_media_file"));
        assert!(prompt.contains("render_to_image"));
    }

    #[test]
    fn test_spinner_char_cycles() {
        let first = spinner_char(0);
        assert_eq!(first, '⠋');
        assert_eq!(spinner_char(9), '⠏');
        assert_eq!(spinner_char(10), '⠋');
    }

    #[tokio::test]
    async fn test_streaming_reply_accumulates() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("app.db");
        let store = Store::open(&path).await.unwrap();
        let client: Arc<dyn LlmClient> = Arc::new(StreamingFakeLlm {
            chunks: vec!["He".to_string(), "llo".to_string()],
        });
        let registry = ToolRegistry::new(ToolContext {
            working_dir: dir.path().to_path_buf(),
        });
        let mut app = App::new(
            store,
            client,
            Arc::new(Mutex::new(registry)),
            MultimodalConfig::default(),
        )
        .await
        .unwrap();

        app.input.insert_char('h');
        app.input.insert_char('i');
        app.send_message().await.unwrap();
        app.drain_stream().await;

        assert_eq!(app.streaming_reply, "");
        assert_eq!(app.status, AppStatus::Idle);
        let messages = app.store.list_messages(&app.session_id).await.unwrap();
        assert!(
            messages
                .iter()
                .any(|m| m.role == "assistant" && m.content == "Hello")
        );
        let last_chat = app.chat.messages().last().unwrap();
        assert_eq!(last_chat.role, "assistant");
        assert_eq!(last_chat.content, "Hello");
    }

    #[tokio::test]
    async fn test_send_message_streaming_status() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("app.db");
        let store = Store::open(&path).await.unwrap();
        let client: Arc<dyn LlmClient> = Arc::new(StreamingFakeLlm {
            chunks: vec!["ok".to_string()],
        });
        let registry = ToolRegistry::new(ToolContext {
            working_dir: dir.path().to_path_buf(),
        });
        let mut app = App::new(
            store,
            client,
            Arc::new(Mutex::new(registry)),
            MultimodalConfig::default(),
        )
        .await
        .unwrap();

        app.input.insert_str("hello");
        app.send_message().await.unwrap();
        assert_eq!(app.status, AppStatus::Streaming);

        app.drain_stream().await;
        assert_eq!(app.status, AppStatus::Idle);
    }

    #[tokio::test]
    async fn test_attach_command_image() {
        let (mut app, dir) = create_multimodal_app().await;
        let img_path = dir.path().join("photo.png");
        let img = image::RgbImage::new(10, 10);
        img.save(&img_path).unwrap();

        type_text(&mut app, "/attach photo.png").await;
        press_enter(&mut app).await;

        assert_eq!(app.attachments.len(), 1);
        let last = app.chat.messages().last().unwrap();
        assert_eq!(last.role, "system");
        assert!(last.content.contains("已添加附件"));
    }

    #[tokio::test]
    async fn test_attach_command_missing_file() {
        let (mut app, _dir) = create_multimodal_app().await;
        type_text(&mut app, "/attach missing.png").await;
        press_enter(&mut app).await;
        assert!(app.attachments.is_empty());
        let last = app.chat.messages().last().unwrap();
        assert!(last.content.contains("文件不存在"));
    }

    #[tokio::test]
    async fn test_attach_command_unsupported() {
        let (mut app, dir) = create_multimodal_app().await;
        let txt = dir.path().join("doc.txt");
        tokio::fs::write(&txt, "text").await.unwrap();

        type_text(&mut app, "/attach doc.txt").await;
        press_enter(&mut app).await;
        assert!(app.attachments.is_empty());
        let last = app.chat.messages().last().unwrap();
        assert!(last.content.contains("仅支持图片或视频"));
    }

    #[tokio::test]
    async fn test_attachments_and_clear_commands() {
        let (mut app, dir) = create_multimodal_app().await;
        let img_path = dir.path().join("a.png");
        let img = image::RgbImage::new(10, 10);
        img.save(&img_path).unwrap();

        type_text(&mut app, "/attach a.png").await;
        press_enter(&mut app).await;

        type_text(&mut app, "/attachments").await;
        press_enter(&mut app).await;
        let last = app.chat.messages().last().unwrap();
        assert!(last.content.contains("a.png"));

        type_text(&mut app, "/clear_attachments").await;
        press_enter(&mut app).await;
        assert!(app.attachments.is_empty());
        let last = app.chat.messages().last().unwrap();
        assert!(last.content.contains("已清除"));
    }

    #[tokio::test]
    async fn test_pasted_media_path_detection() {
        let (mut app, dir) = create_multimodal_app().await;
        let img_path = dir.path().join("pic.png");
        let img = image::RgbImage::new(10, 10);
        img.save(&img_path).unwrap();

        type_text(&mut app, "pic.png").await;
        press_enter(&mut app).await;

        let messages = app.store.list_messages(&app.session_id).await.unwrap();
        let user_msg = messages.iter().find(|m| m.role == "user").unwrap();
        assert!(user_msg.content.contains("请分析这张图片"));
        assert!(user_msg.content.contains("data:image/png;base64,"));
        assert!(app.attachments.is_empty());
    }
}
