use anyhow::Result;
use chrono::Utc;
use crossterm::event::{Event, EventStream, KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use ratatui::{
    Frame, Terminal,
    backend::Backend,
    layout::{Constraint, Direction, Layout, Position},
    style::{Color, Style},
    text::{Line, Span},
    widgets::Paragraph,
};
use serde_json::Value;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::{Mutex, mpsc};
use tokio_stream::StreamExt;
use tracing::info;
use uuid::Uuid;

use clerk_core::agent::{
    llm::{LlmClient, StreamChunk},
    runner::{ApprovalRequest, PlanExecuteRunner, RunnerEvent},
    session::SessionContext,
};
use clerk_core::config::MultimodalConfig;
use clerk_core::media::{media_kind, with_attachments};
use clerk_core::prompt::build_system_prompt;
use clerk_core::store::{Message, Store};
use clerk_core::text::{format_tool_arguments, format_tool_event};
use clerk_core::tools::registry::ToolRegistry;

use crate::ui::chat::ChatPanel;
use crate::ui::input::{InputArea, KNOWN_COMMANDS, longest_common_prefix};

/// 应用状态：空闲 / 正在流式生成 / 出错。
#[derive(Debug, Clone, PartialEq)]
pub enum AppStatus {
    Idle,
    Streaming,
    Error(String),
}

/// 状态栏与 /model 命令展示的模型信息。
#[derive(Debug, Clone)]
pub struct ModelInfo {
    pub model: String,
    pub base_url: String,
}

/// TUI 应用：持有会话状态、UI 组件、Agent runner 与流式/审批通道。
pub struct App {
    pub session_id: String,
    pub chat: ChatPanel,
    pub input: InputArea,
    pub status: AppStatus,
    pub should_quit: bool,
    pub tool_events: Vec<RunnerEvent>,
    pub yolo_mode: bool,
    /// 等待用户审批的工具调用；Some 表示 UI 处于审批模式
    pub approval_mode: Option<PendingApproval>,
    pub multimodal: MultimodalConfig,
    pub model_info: ModelInfo,
    pub attachments: Vec<PathBuf>,
    pub streaming_reply: String,
    store: Store,
    runner: PlanExecuteRunner,
    registry: Arc<Mutex<ToolRegistry>>,
    session_ctx: Arc<Mutex<SessionContext>>,
    working_dir: PathBuf,
    stream_rx: Option<mpsc::UnboundedReceiver<StreamEvent>>,
    stream_abort: Option<tokio::task::AbortHandle>,
    streaming_message_added: bool,
    spinner_frame: usize,
    /// 本轮流式生成开始时间，用于状态栏展示已耗时
    stream_started_at: Option<Instant>,
    /// 首个输出块到达前，聊天区加载占位消息的下标
    placeholder_index: Option<usize>,
    /// 会话选择器：Some 表示 UI 处于会话选择模式
    pub session_picker: Option<SessionPickerState>,
}

/// 待审批的工具调用：保存审批请求与一次性响应端
#[derive(Debug)]
pub struct PendingApproval {
    pub name: String,
    pub arguments: Value,
    respond: tokio::sync::oneshot::Sender<bool>,
}

/// 会话选择器状态：保存候选会话与当前选中下标。
#[derive(Debug)]
pub struct SessionPickerState {
    pub sessions: Vec<clerk_core::store::sqlite::Session>,
    pub selected: usize,
}

/// 流式任务发往 UI 的事件：输出块、工具事件、审批请求与完成信号。
#[derive(Debug)]
enum StreamEvent {
    Chunk(StreamChunk),
    ToolEvent(RunnerEvent),
    ApprovalRequired {
        name: String,
        arguments: Value,
        respond: tokio::sync::oneshot::Sender<bool>,
    },
    Done(Result<String>),
}

impl App {
    /// 创建新会话的 App：在 Store 中建会话，初始化 UI 组件与 PlanExecuteRunner。
    pub async fn new(
        store: Store,
        client: Arc<dyn LlmClient>,
        registry: Arc<Mutex<ToolRegistry>>,
        multimodal: MultimodalConfig,
        model_info: ModelInfo,
    ) -> Result<Self> {
        let session_id = Uuid::new_v4().to_string();
        store.create_session(&session_id, Some("新会话")).await?;
        let messages = store.list_messages(&session_id).await.unwrap_or_default();
        let (working_dir, yolo_mode) = {
            let registry = registry.lock().await;
            let ctx = registry.context();
            let yolo = ctx.permissions.as_ref().is_some_and(|p| p.yolo);
            (ctx.working_dir.clone(), yolo)
        };

        info!("创建新会话: {}", session_id);
        let session_ctx = Arc::new(Mutex::new(SessionContext::new(build_system_prompt())));
        let runner = PlanExecuteRunner::new(client, registry.clone());

        Ok(Self {
            session_id,
            chat: ChatPanel::new(messages),
            input: InputArea::new(),
            status: AppStatus::Idle,
            should_quit: false,
            tool_events: Vec::new(),
            yolo_mode,
            approval_mode: None,
            multimodal,
            model_info,
            attachments: Vec::new(),
            streaming_reply: String::new(),
            store,
            runner,
            registry,
            session_ctx,
            working_dir,
            stream_rx: None,
            stream_abort: None,
            streaming_message_added: false,
            spinner_frame: 0,
            stream_started_at: None,
            placeholder_index: None,
            session_picker: None,
        })
    }

    /// 加载已有会话的 App：会话不存在时自动创建，恢复历史消息。
    pub async fn load_session(
        store: Store,
        session_id: &str,
        client: Arc<dyn LlmClient>,
        registry: Arc<Mutex<ToolRegistry>>,
        multimodal: MultimodalConfig,
        model_info: ModelInfo,
    ) -> Result<Self> {
        if store.get_session(session_id).await?.is_none() {
            store.create_session(session_id, Some("恢复会话")).await?;
        }
        let messages = store.list_messages(session_id).await?;
        let (working_dir, yolo_mode) = {
            let registry = registry.lock().await;
            let ctx = registry.context();
            let yolo = ctx.permissions.as_ref().is_some_and(|p| p.yolo);
            (ctx.working_dir.clone(), yolo)
        };

        info!("加载会话: {}", session_id);
        let session_ctx = Arc::new(Mutex::new(SessionContext::new(build_system_prompt())));
        let runner = PlanExecuteRunner::new(client, registry.clone());

        Ok(Self {
            session_id: session_id.to_string(),
            chat: ChatPanel::new(messages),
            input: InputArea::new(),
            status: AppStatus::Idle,
            should_quit: false,
            tool_events: Vec::new(),
            yolo_mode,
            approval_mode: None,
            multimodal,
            model_info,
            attachments: Vec::new(),
            streaming_reply: String::new(),
            store,
            runner,
            registry,
            session_ctx,
            working_dir,
            stream_rx: None,
            stream_abort: None,
            streaming_message_added: false,
            spinner_frame: 0,
            stream_started_at: None,
            placeholder_index: None,
            session_picker: None,
        })
    }

    /// 设置上下文压缩配置，替换内部 runner。
    pub fn set_context_config(&mut self, config: clerk_core::config::ContextConfig) {
        self.runner = self.runner.clone().with_context_config(config);
    }

    /// 切换到指定会话：加载历史消息并清空当前附件与工具事件。
    pub async fn select_session(&mut self, session_id: &str) -> Result<()> {
        if self.store.get_session(session_id).await?.is_none() {
            self.store
                .create_session(session_id, Some("恢复会话"))
                .await?;
        }
        let messages = self.store.list_messages(session_id).await?;
        self.session_id = session_id.to_string();
        self.chat.set_messages(messages);
        self.attachments.clear();
        self.tool_events.clear();
        self.streaming_reply.clear();
        self.streaming_message_added = false;
        self.placeholder_index = None;
        self.session_ctx = Arc::new(Mutex::new(SessionContext::new(build_system_prompt())));
        self.chat.push_message(system_message(
            &self.session_id,
            &format!("已加载会话 {}", session_id),
        ));
        Ok(())
    }

    /// 主事件循环：绘制界面，分发键盘事件与流式事件，驱动加载动画。
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
                        // 首个输出块到达前，让占位消息中的省略号动起来
                        if let Some(idx) = self.placeholder_index
                            && !self.chat.update_message_at(idx, dots_frame(self.spinner_frame))
                        {
                            self.placeholder_index = None;
                        }
                    }
                }
            }
        }
        Ok(())
    }

    /// 处理键盘事件：审批模式下只响应 y/n；否则处理发送、编辑、滚动与命令。
    async fn handle_key(&mut self, key: KeyEvent) -> Result<()> {
        if key.kind != KeyEventKind::Press {
            return Ok(());
        }

        // 审批模式下只响应 y/n（以及 Ctrl+C 中断），其余按键忽略
        if self.approval_mode.is_some() {
            match key.code {
                KeyCode::Char('y') | KeyCode::Char('Y') => self.answer_approval(true),
                KeyCode::Char('n') | KeyCode::Char('N') => self.answer_approval(false),
                KeyCode::Char('c')
                    if key.modifiers.contains(KeyModifiers::CONTROL)
                        && self.status == AppStatus::Streaming =>
                {
                    if let Some(abort) = self.stream_abort.take() {
                        abort.abort();
                    }
                }
                _ => {}
            }
            return Ok(());
        }

        // 会话选择器激活时：↑↓ 选择，Enter 确认，ESC 取消，其余按键忽略
        if self.session_picker.is_some() {
            match key.code {
                KeyCode::Up => {
                    if let Some(picker) = &mut self.session_picker {
                        picker.selected = picker.selected.saturating_sub(1);
                    }
                }
                KeyCode::Down => {
                    if let Some(picker) = &mut self.session_picker
                        && picker.selected + 1 < picker.sessions.len()
                    {
                        picker.selected += 1;
                    }
                }
                KeyCode::Enter => {
                    if let Some(picker) = self.session_picker.take() {
                        let session_id = picker.sessions[picker.selected].id.clone();
                        self.select_session(&session_id).await?;
                    }
                }
                KeyCode::Esc => {
                    self.session_picker = None;
                    self.chat
                        .push_message(system_message(&self.session_id, "已取消会话选择"));
                }
                _ => {}
            }
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
                if self.input.text().starts_with("/attach ") {
                    self.complete_attach_path();
                } else {
                    self.input.autocomplete();
                }
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

    /// 用户对待审批工具做出决定：发送响应并退出审批模式。
    fn answer_approval(&mut self, approved: bool) {
        if let Some(pending) = self.approval_mode.take() {
            let _ = pending.respond.send(approved);
            let msg = if approved {
                format!("已批准执行工具 {}", pending.name)
            } else {
                format!("已拒绝执行工具 {}", pending.name)
            };
            self.chat
                .push_message(system_message(&self.session_id, &msg));
        }
    }

    /// 检测输入是否为单个本地媒体文件路径（粘贴图片/视频场景）：
    /// 是且模型支持时转为附件并直接发送，返回 true。
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

    /// 处理斜杠命令（/help、/exit、/new、/model、/yolo、/sessions、/attach 等）。
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
                let lines: Vec<String> = KNOWN_COMMANDS
                    .iter()
                    .map(|(cmd, desc)| format!("{:<20} {}", cmd, desc))
                    .collect();
                self.chat.push_message(system_message(
                    &self.session_id,
                    &format!("可用命令：\n{}", lines.join("\n")),
                ));
            }
            "/new" => {
                self.start_new_session().await?;
            }
            "/model" => {
                let content = format!(
                    "当前模型: {}\n接口地址: {}",
                    self.model_info.model, self.model_info.base_url
                );
                self.chat
                    .push_message(system_message(&self.session_id, &content));
            }
            "/clear" => {
                self.chat.clear();
                self.tool_events.clear();
            }
            "/yolo" => {
                self.yolo_mode = !self.yolo_mode;
                let configured = {
                    let mut registry = self.registry.lock().await;
                    let mut ctx = registry.context().clone();
                    if let Some(permissions) = ctx.permissions.as_mut() {
                        permissions.yolo = self.yolo_mode;
                        registry.set_context(ctx);
                        true
                    } else {
                        false
                    }
                };
                let status = if self.yolo_mode { "开启" } else { "关闭" };
                let mut msg = format!("YOLO 模式已{}", status);
                if !configured {
                    msg.push_str("\n（未配置 [permissions]，当前所有工具均无需审批）");
                }
                self.chat
                    .push_message(system_message(&self.session_id, &msg));
            }
            "/sessions" => match self.store.list_sessions().await {
                Ok(sessions) if sessions.is_empty() => {
                    self.chat
                        .push_message(system_message(&self.session_id, "暂无会话"));
                }
                Ok(sessions) => {
                    self.session_picker = Some(SessionPickerState {
                        sessions,
                        selected: 0,
                    });
                    self.chat.push_message(system_message(
                        &self.session_id,
                        "已打开会话选择器：↑↓ 选择，Enter 确认，ESC 取消",
                    ));
                }
                Err(e) => {
                    self.chat.push_message(system_message(
                        &self.session_id,
                        &format!("获取会话列表失败: {:#}", e),
                    ));
                }
            },
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

    /// 添加附件：校验文件存在、类型为图片/视频，并对未启用对应多模态能力的模型给出提示。
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

    /// 将用户输入的路径解析为绝对路径（相对路径基于工作目录）。
    fn resolve_relative_path(&self, input: &str) -> PathBuf {
        let path = PathBuf::from(input);
        if path.is_absolute() {
            path
        } else {
            self.working_dir.join(path)
        }
    }

    /// 开始新会话：中断进行中的流式任务，重置会话上下文与 UI 状态。
    async fn start_new_session(&mut self) -> Result<()> {
        if let Some(abort) = self.stream_abort.take() {
            abort.abort();
        }
        self.stream_rx = None;

        let session_id = Uuid::new_v4().to_string();
        self.store
            .create_session(&session_id, Some("新会话"))
            .await?;
        info!("开始新会话: {}", session_id);
        self.session_id = session_id;
        // 整体替换 Arc，避免影响仍在收尾的旧流式任务持有的上下文
        self.session_ctx = Arc::new(Mutex::new(SessionContext::new(build_system_prompt())));
        self.chat.clear();
        self.tool_events.clear();
        self.attachments.clear();
        self.streaming_reply.clear();
        self.streaming_message_added = false;
        self.stream_started_at = None;
        self.placeholder_index = None;
        self.approval_mode = None;
        self.status = AppStatus::Idle;
        self.chat.push_message(system_message(
            &self.session_id,
            &format!("已开始新会话 ({})", short_id(&self.session_id)),
        ));
        Ok(())
    }

    /// /attach 的文件路径补全：用工作目录下匹配项的最长公共前缀替换参数。
    fn complete_attach_path(&mut self) {
        let text = self.input.text();
        let Some(partial) = text.strip_prefix("/attach ") else {
            return;
        };
        if let Some(completed) = complete_file_path(partial, &self.working_dir)
            && completed != partial
        {
            self.input.clear();
            self.input.insert_str(&format!("/attach {}", completed));
        }
    }

    /// 发送用户消息：拼接附件描述、持久化，然后启动流式 runner 任务，
    /// 并把输出块/工具事件/审批请求统一桥接到 UI 的 StreamEvent 通道。
    async fn send_message(&mut self) -> Result<()> {
        let mut text = self.input.text().trim().to_string();
        if text.is_empty() {
            return Ok(());
        }

        self.input.clear();

        if !self.attachments.is_empty() {
            text = with_attachments(text, &self.attachments).await;
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
        self.stream_started_at = Some(Instant::now());

        // 首个输出块到达前，先放一条省略号动画的占位消息，避免界面看似卡死
        self.chat.push_message(Message {
            id: 0,
            session_id: self.session_id.clone(),
            role: "assistant".to_string(),
            content: dots_frame(0).to_string(),
            created_at: Utc::now(),
        });
        self.placeholder_index = Some(self.chat.messages().len() - 1);

        let (stream_tx, stream_rx) = mpsc::unbounded_channel::<StreamEvent>();
        self.stream_rx = Some(stream_rx);

        let ctx = self.session_ctx.clone();
        let text_clone = text.clone();

        // 先创建实际的 LLM runner task，以便把 AbortHandle 交给主循环
        let (event_tx, mut event_rx) = mpsc::unbounded_channel::<RunnerEvent>();
        let (chunk_tx, mut chunk_rx) = mpsc::unbounded_channel::<StreamChunk>();
        let (approval_tx, mut approval_rx) = mpsc::unbounded_channel::<ApprovalRequest>();
        let runner = self.runner.clone().with_approval_tx(approval_tx);
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
                    Some(req) = approval_rx.recv() => {
                        let _ = stream_tx.send(StreamEvent::ApprovalRequired {
                            name: req.name,
                            arguments: req.arguments,
                            respond: req.respond,
                        });
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

    /// 处理流式事件：累积输出块（推理内容包入 <think>）、记录工具事件、
    /// 进入审批模式，或在完成时落库最终回复并复位状态。
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
                    // 首个输出块到达：撤下加载占位消息，换成真正的流式消息
                    if let Some(idx) = self.placeholder_index.take() {
                        self.chat.remove_message_at(idx);
                    }
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
            StreamEvent::ApprovalRequired {
                name,
                arguments,
                respond,
            } => {
                let args = format_tool_arguments(&name, &arguments);
                let prompt = format!("[tool] approve {}? (y/n)\n参数: {}", name, args);
                self.chat
                    .push_message(system_message(&self.session_id, &prompt));
                self.approval_mode = Some(PendingApproval {
                    name,
                    arguments,
                    respond,
                });
            }
            StreamEvent::Done(result) => {
                self.stream_abort = None;
                self.approval_mode = None;
                self.stream_started_at = None;
                // 没有任何输出块时，撤下仍在的加载占位消息
                if let Some(idx) = self.placeholder_index.take() {
                    self.chat.remove_message_at(idx);
                }
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

    /// 绘制界面：聊天区、命令提示区（输入 / 时出现）、输入区（并定位光标）与两行状态栏。
    fn draw(&self, frame: &mut Frame) {
        let suggestions = self.input.command_suggestions();
        let hint_height = suggestions.len().min(5) as u16;

        let mut constraints = vec![Constraint::Min(10)];
        if hint_height > 0 {
            constraints.push(Constraint::Length(hint_height));
        }
        constraints.push(Constraint::Length(6));
        constraints.push(Constraint::Length(2));

        let chunks = Layout::default()
            .direction(Direction::Vertical)
            .margin(1)
            .constraints(constraints)
            .split(frame.area());

        frame.render_widget(&self.chat, chunks[0]);

        let input_idx = chunks.len() - 2;
        let status_idx = chunks.len() - 1;

        if hint_height > 0 {
            let hint_lines: Vec<Line> = suggestions
                .iter()
                .take(5)
                .map(|(cmd, desc)| {
                    Line::from(vec![
                        Span::styled(format!("  {:<20}", cmd), Style::default().fg(Color::Cyan)),
                        Span::styled(*desc, Style::default().fg(Color::DarkGray)),
                    ])
                })
                .collect();
            frame.render_widget(Paragraph::new(hint_lines), chunks[1]);
        }

        frame.render_widget(&self.input, chunks[input_idx]);

        let (cursor_x, cursor_y) = self.input.cursor_screen_pos(chunks[input_idx]);
        frame.set_cursor_position(Position::new(
            chunks[input_idx].x + cursor_x,
            chunks[input_idx].y + cursor_y,
        ));

        // 状态栏第一行：运行状态（审批 / 流式 / 错误 / 就绪）
        let (state_text, state_style) = if let Some(pending) = &self.approval_mode {
            (
                format!("[tool] approve {}? (y/n)", pending.name),
                Style::default().fg(Color::Yellow),
            )
        } else {
            match &self.status {
                AppStatus::Idle => {
                    if self.tool_events.is_empty() {
                        (
                            "就绪 | Enter 发送 | Shift+Enter 换行 | / 命令 | Shift+↑/↓ 滚动"
                                .to_string(),
                            Style::default().fg(Color::Gray),
                        )
                    } else {
                        (
                            format!("最近工具调用: {} 次", self.tool_events.len()),
                            Style::default().fg(Color::Gray),
                        )
                    }
                }
                AppStatus::Streaming => {
                    let elapsed = self
                        .stream_started_at
                        .map(|t| t.elapsed().as_secs())
                        .unwrap_or(0);
                    (
                        streaming_status_text(self.spinner_frame, elapsed),
                        Style::default().fg(Color::Cyan),
                    )
                }
                AppStatus::Error(e) => (format!("错误: {}", e), Style::default().fg(Color::Red)),
            }
        };

        // 状态栏第二行：模型 | 工作目录 | 会话 | 审批模式
        let approval_span = if self.yolo_mode {
            Span::styled("YOLO 开", Style::default().fg(Color::Green))
        } else {
            Span::styled("手动审批", Style::default().fg(Color::Yellow))
        };
        let sep = Span::styled(" | ", Style::default().fg(Color::DarkGray));
        let info_line = Line::from(vec![
            Span::styled(
                self.model_info.model.clone(),
                Style::default().fg(Color::Cyan),
            ),
            sep.clone(),
            Span::styled(
                self.working_dir.display().to_string(),
                Style::default().fg(Color::Blue),
            ),
            sep.clone(),
            Span::styled(
                format!("session {}", short_id(&self.session_id)),
                Style::default().fg(Color::Gray),
            ),
            sep,
            approval_span,
        ]);

        let status = Paragraph::new(vec![Line::from(state_text).style(state_style), info_line]);
        frame.render_widget(status, chunks[status_idx]);

        if let Some(picker) = &self.session_picker {
            render_session_picker(frame, picker);
        }
    }
}

/// 渲染会话选择器浮层：标题、会话列表与操作提示。
fn render_session_picker(frame: &mut Frame, picker: &SessionPickerState) {
    let area = frame.area();
    let width = area.width.saturating_sub(8).min(80);
    let height = (picker.sessions.len() as u16 + 4)
        .min(area.height.saturating_sub(4))
        .max(6);
    let x = area.x + (area.width.saturating_sub(width)) / 2;
    let y = area.y + (area.height.saturating_sub(height)) / 2;
    let rect = ratatui::layout::Rect::new(x, y, width, height);

    let mut lines: Vec<Line> = Vec::new();
    lines.push(Line::from(Span::styled(
        "选择会话",
        Style::default().fg(Color::Cyan),
    )));
    lines.push(Line::from(""));
    for (i, session) in picker.sessions.iter().enumerate() {
        let title = session.title.as_deref().unwrap_or("无标题");
        let text = format!(
            "{} {} ({})",
            session.updated_at.format("%m-%d %H:%M"),
            title,
            short_id(&session.id)
        );
        let style = if i == picker.selected {
            Style::default().fg(Color::Black).bg(Color::Cyan)
        } else {
            Style::default().fg(Color::White)
        };
        lines.push(Line::from(Span::styled(text, style)));
    }
    lines.push(Line::from(""));
    lines.push(Line::from(Span::styled(
        "↑↓ 选择，Enter 确认，ESC 取消",
        Style::default().fg(Color::Gray),
    )));

    let block = ratatui::widgets::Block::default()
        .borders(ratatui::widgets::Borders::ALL)
        .title("会话");
    frame.render_widget(Paragraph::new(lines).block(block), rect);
}

fn spinner_char(frame: usize) -> char {
    const FRAMES: [char; 10] = ['⠋', '⠙', '⠹', '⠸', '⠼', '⠴', '⠦', '⠧', '⠇', '⠏'];
    FRAMES[frame % FRAMES.len()]
}

/// 动画省略号帧：给渲染盲文效果差的终端一个纯 ASCII 的活动信号。
/// 每两 tick（200ms）换一帧，四帧一循环。
fn dots_frame(frame: usize) -> &'static str {
    const FRAMES: [&str; 4] = ["... ", ".. .", ". ..", " ..."];
    FRAMES[(frame / 2) % FRAMES.len()]
}

/// 流式状态栏文本：盲文 spinner + 动画省略号 + 已耗时秒数 + Ctrl+C 中断提示。
fn streaming_status_text(frame: usize, elapsed_secs: u64) -> String {
    format!(
        "{} 思考中{} ({}s) (Ctrl+C 中断)",
        spinner_char(frame),
        dots_frame(frame),
        elapsed_secs
    )
}

/// 会话 ID 的短形式（前 8 个字符），用于状态栏展示。
fn short_id(id: &str) -> &str {
    let end = id.char_indices().nth(8).map(|(i, _)| i).unwrap_or(id.len());
    &id[..end]
}

/// /attach 的文件路径补全：以 `working_dir` 为基准解析 `partial` 的目录部分，
/// 返回目录内匹配项的最长公共前缀路径；唯一的目录匹配会追加 `/` 以便继续输入。
/// 无匹配或无法继续延长时返回 None。
fn complete_file_path(partial: &str, working_dir: &Path) -> Option<String> {
    let (dir_part, prefix) = match partial.rfind('/') {
        Some(idx) => (&partial[..=idx], &partial[idx + 1..]),
        None => ("", partial),
    };
    let dir = if dir_part.is_empty() {
        working_dir.to_path_buf()
    } else {
        let p = PathBuf::from(dir_part.trim_end_matches('/'));
        if p.is_absolute() {
            p
        } else {
            working_dir.join(p)
        }
    };

    let mut matches: Vec<String> = std::fs::read_dir(&dir)
        .ok()?
        .filter_map(|e| e.ok())
        .filter_map(|e| {
            let name = e.file_name().into_string().ok()?;
            if !name.starts_with(prefix) {
                return None;
            }
            if prefix.is_empty() && name.starts_with('.') {
                return None; // 空前缀时不提示隐藏文件
            }
            if e.path().is_dir() {
                Some(format!("{}/", name))
            } else {
                Some(name)
            }
        })
        .collect();
    matches.sort();

    let refs: Vec<&str> = matches.iter().map(String::as_str).collect();
    let lcp = longest_common_prefix(&refs);
    if lcp.len() <= prefix.len() {
        return None;
    }
    Some(format!("{}{}", dir_part, lcp))
}

/// 构造一条仅用于 UI 展示的系统消息（不落库，id 为 0）。
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
    use async_trait::async_trait;
    use clerk_core::agent::llm::{LlmResponse, Message, ToolDefinition};
    use clerk_core::store::Store;
    use clerk_core::tools::schema::{Tool, ToolContext, ToolResult, ToolSchema};
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

    fn test_model_info() -> ModelInfo {
        ModelInfo {
            model: "test-model".to_string(),
            base_url: "https://test.example/v1".to_string(),
        }
    }

    async fn create_test_app() -> (App, TempDir) {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("app.db");
        let store = Store::open(&path).await.unwrap();
        let client: Arc<dyn LlmClient> = Arc::new(FakeLlm);
        let mut registry = ToolRegistry::new(ToolContext {
            working_dir: dir.path().to_path_buf(),
            ..Default::default()
        });
        registry.register(Arc::new(FakeTool));
        let app = App::new(
            store,
            client,
            Arc::new(Mutex::new(registry)),
            MultimodalConfig::default(),
            test_model_info(),
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
            ..Default::default()
        });
        registry.register(Arc::new(FakeTool));
        let multimodal = MultimodalConfig {
            supports_images: true,
            supports_video: true,
        };
        let app = App::new(
            store,
            client,
            Arc::new(Mutex::new(registry)),
            multimodal,
            test_model_info(),
        )
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

    /// 收集缓冲区文本并去除空白：宽字符（CJK）会将其后的单元格重置为空格，
    /// 直接拼接无法在宽字符附近匹配子串，因此断言前先去掉所有空白。
    fn compact_buffer_text(buffer: &ratatui::buffer::Buffer) -> String {
        buffer
            .content
            .iter()
            .map(|c| c.symbol())
            .collect::<String>()
            .chars()
            .filter(|c| !c.is_whitespace())
            .collect()
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
            ..Default::default()
        });
        registry.register(Arc::new(FakeTool));
        let app = App::load_session(
            store,
            session_id,
            client,
            Arc::new(Mutex::new(registry)),
            MultimodalConfig::default(),
            test_model_info(),
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
            ..Default::default()
        });
        let mut app = App::new(
            store,
            client,
            Arc::new(Mutex::new(registry)),
            MultimodalConfig::default(),
            test_model_info(),
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
            ..Default::default()
        });
        let mut app = App::new(
            store,
            client,
            Arc::new(Mutex::new(registry)),
            MultimodalConfig::default(),
            test_model_info(),
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

    // ---- 工具审批 ----

    use clerk_core::agent::llm::{FunctionCall, ToolCall};
    use clerk_core::config::PermissionConfig;
    use std::sync::atomic::{AtomicUsize, Ordering};

    struct QueueFakeLlm {
        responses: Mutex<Vec<LlmResponse>>,
    }

    #[async_trait]
    impl LlmClient for QueueFakeLlm {
        async fn chat(
            &self,
            _messages: Vec<Message>,
            _tools: Vec<ToolDefinition>,
        ) -> Result<LlmResponse> {
            let mut responses = self.responses.lock().await;
            Ok(responses.remove(0))
        }
    }

    struct CountingTool {
        count: Arc<AtomicUsize>,
    }

    #[async_trait]
    impl Tool for CountingTool {
        fn name(&self) -> &str {
            "counted"
        }
        fn description(&self) -> &str {
            "counted"
        }
        fn schema(&self) -> ToolSchema {
            ToolSchema::new("counted", "counted")
        }
        async fn execute(
            &self,
            _args: HashMap<String, Value>,
            _ctx: &ToolContext,
        ) -> Result<ToolResult> {
            self.count.fetch_add(1, Ordering::SeqCst);
            Ok(ToolResult::Text("counted done".to_string()))
        }
    }

    /// 构造一个会触发 counted 工具调用的 App：计划 -> 工具调用 -> 步骤完成 -> 总结。
    async fn create_approval_app(
        permissions: Option<PermissionConfig>,
    ) -> (App, TempDir, Arc<AtomicUsize>) {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("app.db");
        let store = Store::open(&path).await.unwrap();
        let client: Arc<dyn LlmClient> = Arc::new(QueueFakeLlm {
            responses: Mutex::new(vec![
                LlmResponse::Text(r#"["调用工具"]"#.to_string()),
                LlmResponse::ToolCalls(vec![ToolCall {
                    id: "1".to_string(),
                    call_type: "function".to_string(),
                    function: FunctionCall {
                        name: "counted".to_string(),
                        arguments: "{}".to_string(),
                    },
                }]),
                LlmResponse::Text("step done".to_string()),
                LlmResponse::Text("最终回复".to_string()),
            ]),
        });
        let count = Arc::new(AtomicUsize::new(0));
        let mut registry = ToolRegistry::new(ToolContext {
            working_dir: dir.path().to_path_buf(),
            permissions,
        });
        registry.register(Arc::new(CountingTool {
            count: count.clone(),
        }));
        let app = App::new(
            store,
            client,
            Arc::new(Mutex::new(registry)),
            MultimodalConfig::default(),
            test_model_info(),
        )
        .await
        .unwrap();
        (app, dir, count)
    }

    /// 处理流事件直到结束；遇到审批请求时按给定决定应答。
    async fn press_enter_with_approval(app: &mut App, approved: bool) {
        app.handle_key(KeyEvent::from(KeyCode::Enter))
            .await
            .unwrap();
        loop {
            let event = {
                let Some(rx) = app.stream_rx.as_mut() else {
                    break;
                };
                match rx.recv().await {
                    Some(e) => e,
                    None => break,
                }
            };
            let is_done = matches!(event, StreamEvent::Done(_));
            app.handle_stream_event(event).await.unwrap();
            if app.approval_mode.is_some() {
                app.answer_approval(approved);
            }
            if is_done {
                break;
            }
        }
    }

    #[tokio::test]
    async fn test_tool_approval_accepted() {
        let (mut app, _dir, count) = create_approval_app(Some(PermissionConfig::default())).await;
        type_text(&mut app, "hi").await;
        press_enter_with_approval(&mut app, true).await;

        assert_eq!(count.load(Ordering::SeqCst), 1);
        assert!(app.approval_mode.is_none());
        assert_eq!(app.status, AppStatus::Idle);
        assert!(
            app.chat
                .messages()
                .iter()
                .any(|m| m.content.contains("[tool] approve counted? (y/n)"))
        );
        assert!(
            app.chat
                .messages()
                .iter()
                .any(|m| m.content.contains("已批准执行工具 counted"))
        );
        let messages = app.store.list_messages(&app.session_id).await.unwrap();
        assert!(
            messages
                .iter()
                .any(|m| m.role == "tool" && m.content.contains("counted done"))
        );
    }

    #[tokio::test]
    async fn test_tool_approval_rejected() {
        let (mut app, _dir, count) = create_approval_app(Some(PermissionConfig::default())).await;
        type_text(&mut app, "hi").await;
        press_enter_with_approval(&mut app, false).await;

        // 工具未被执行，且模型收到拒绝提示
        assert_eq!(count.load(Ordering::SeqCst), 0);
        assert!(app.approval_mode.is_none());
        assert!(
            app.chat
                .messages()
                .iter()
                .any(|m| m.content.contains("已拒绝执行工具 counted"))
        );
        let messages = app.store.list_messages(&app.session_id).await.unwrap();
        assert!(
            messages
                .iter()
                .any(|m| m.role == "tool" && m.content.contains("用户拒绝执行该工具"))
        );
    }

    #[tokio::test]
    async fn test_tool_runs_without_approval_when_permissions_absent() {
        let (mut app, _dir, count) = create_approval_app(None).await;
        type_text(&mut app, "hi").await;
        press_enter(&mut app).await;

        assert_eq!(count.load(Ordering::SeqCst), 1);
        assert!(app.approval_mode.is_none());
    }

    #[tokio::test]
    async fn test_tool_auto_approved_in_yolo_mode() {
        let (mut app, _dir, count) = create_approval_app(Some(PermissionConfig {
            yolo: true,
            ..Default::default()
        }))
        .await;
        // 初始 yolo_mode 从配置读取
        assert!(app.yolo_mode);
        type_text(&mut app, "hi").await;
        press_enter(&mut app).await;
        assert_eq!(count.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn test_tool_in_auto_approve_list_skips_approval() {
        let (mut app, _dir, count) = create_approval_app(Some(PermissionConfig {
            yolo: false,
            auto_approve: vec!["counted".to_string()],
        }))
        .await;
        type_text(&mut app, "hi").await;
        press_enter(&mut app).await;
        assert_eq!(count.load(Ordering::SeqCst), 1);
        assert!(app.approval_mode.is_none());
    }

    #[tokio::test]
    async fn test_yolo_command_syncs_permissions() {
        let (mut app, _dir, _count) = create_approval_app(Some(PermissionConfig::default())).await;
        assert!(!app.yolo_mode);

        type_text(&mut app, "/yolo").await;
        press_enter(&mut app).await;
        assert!(app.yolo_mode);
        {
            let registry = app.registry.lock().await;
            assert!(registry.context().permissions.as_ref().unwrap().yolo);
        }

        type_text(&mut app, "/yolo").await;
        press_enter(&mut app).await;
        assert!(!app.yolo_mode);
        let registry = app.registry.lock().await;
        assert!(!registry.context().permissions.as_ref().unwrap().yolo);
    }

    #[tokio::test]
    async fn test_approval_key_handling() {
        let (mut app, _dir, _count) = create_approval_app(Some(PermissionConfig::default())).await;
        let (tx, rx) = tokio::sync::oneshot::channel();
        app.approval_mode = Some(PendingApproval {
            name: "fs_write".to_string(),
            arguments: serde_json::json!({}),
            respond: tx,
        });

        // 非 y/n 按键被忽略，不进入输入框
        app.handle_key(KeyEvent::from(KeyCode::Char('x')))
            .await
            .unwrap();
        assert!(app.approval_mode.is_some());
        assert!(app.input.is_empty());

        app.handle_key(KeyEvent::from(KeyCode::Char('n')))
            .await
            .unwrap();
        assert!(app.approval_mode.is_none());
        assert_eq!(rx.await, Ok(false));
        assert!(
            app.chat
                .messages()
                .iter()
                .any(|m| m.content.contains("已拒绝执行工具 fs_write"))
        );
    }

    #[tokio::test]
    async fn test_draw_shows_approval_prompt() {
        let (mut app, _dir, _count) = create_approval_app(Some(PermissionConfig::default())).await;
        let (tx, _rx) = tokio::sync::oneshot::channel();
        app.approval_mode = Some(PendingApproval {
            name: "fs_write".to_string(),
            arguments: serde_json::json!({}),
            respond: tx,
        });
        let backend = ratatui::backend::TestBackend::new(80, 24);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal.draw(|f| app.draw(f)).unwrap();
        let buffer = terminal.backend().buffer();
        let content: String = buffer.content.iter().map(|c| c.symbol()).collect();
        assert!(content.contains("[tool] approve fs_write? (y/n)"));
    }

    #[tokio::test]
    async fn test_handle_key_sessions_command() {
        let (mut app, _dir) = create_test_app().await;
        type_text(&mut app, "/sessions").await;
        press_enter(&mut app).await;
        assert!(app.session_picker.is_some());
        let last = app.chat.messages().last().unwrap();
        assert_eq!(last.role, "system");
        assert!(last.content.contains("会话选择器"));
    }

    #[tokio::test]
    async fn test_session_picker_navigation() {
        let (mut app, _dir) = create_test_app().await;
        // 创建第二个会话
        let other_id = "other-session";
        app.store
            .create_session(other_id, Some("其他会话"))
            .await
            .unwrap();
        type_text(&mut app, "/sessions").await;
        press_enter(&mut app).await;

        let picker = app.session_picker.as_ref().unwrap();
        assert_eq!(picker.sessions.len(), 2);
        assert_eq!(picker.selected, 0);

        app.handle_key(KeyEvent::from(KeyCode::Down)).await.unwrap();
        assert_eq!(app.session_picker.as_ref().unwrap().selected, 1);

        app.handle_key(KeyEvent::from(KeyCode::Up)).await.unwrap();
        assert_eq!(app.session_picker.as_ref().unwrap().selected, 0);
    }

    #[tokio::test]
    async fn test_session_picker_esc_cancels() {
        let (mut app, _dir) = create_test_app().await;
        type_text(&mut app, "/sessions").await;
        press_enter(&mut app).await;
        assert!(app.session_picker.is_some());

        app.handle_key(KeyEvent::from(KeyCode::Esc)).await.unwrap();
        assert!(app.session_picker.is_none());
        let last = app.chat.messages().last().unwrap();
        assert!(last.content.contains("已取消"));
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
    fn test_spinner_char_cycles() {
        let first = spinner_char(0);
        assert_eq!(first, '⠋');
        assert_eq!(spinner_char(9), '⠏');
        assert_eq!(spinner_char(10), '⠋');
    }

    #[test]
    fn test_dots_frame_cycles() {
        assert_eq!(dots_frame(0), "... ");
        assert_eq!(dots_frame(1), "... ");
        assert_eq!(dots_frame(2), ".. .");
        assert_eq!(dots_frame(3), ".. .");
        assert_eq!(dots_frame(4), ". ..");
        assert_eq!(dots_frame(6), " ...");
        // 每两帧换一档，四档（8 tick）一循环
        assert_eq!(dots_frame(8), dots_frame(0));
    }

    #[test]
    fn test_streaming_status_text() {
        let text = streaming_status_text(0, 12);
        assert!(text.contains('⠋'));
        assert!(text.contains("思考中"));
        assert!(text.contains("..."));
        assert!(text.contains("(12s)"));
        assert!(text.contains("Ctrl+C"));
    }

    /// 判断是否为加载占位消息（assistant 角色且内容全为省略号动画帧）。
    fn is_dots_placeholder(msg: &clerk_core::store::Message) -> bool {
        msg.role == "assistant"
            && !msg.content.trim().is_empty()
            && msg.content.trim().chars().all(|c| c == '.')
    }

    /// 构造一个 60 秒后才产出首个输出块的 App，用于观察等待期间的加载状态。
    async fn create_slow_streaming_app() -> (App, TempDir) {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("app.db");
        let store = Store::open(&path).await.unwrap();
        let client: Arc<dyn LlmClient> = Arc::new(SlowStreamingFakeLlm);
        let registry = ToolRegistry::new(ToolContext {
            working_dir: dir.path().to_path_buf(),
            ..Default::default()
        });
        let app = App::new(
            store,
            client,
            Arc::new(Mutex::new(registry)),
            MultimodalConfig::default(),
            test_model_info(),
        )
        .await
        .unwrap();
        (app, dir)
    }

    #[tokio::test]
    async fn test_send_message_shows_loading_placeholder() {
        let (mut app, _dir) = create_slow_streaming_app().await;
        app.input.insert_str("hi");
        app.send_message().await.unwrap();

        assert_eq!(app.status, AppStatus::Streaming);
        assert!(app.stream_started_at.is_some());
        let idx = app.placeholder_index.expect("应存在加载占位消息");
        let placeholder = &app.chat.messages()[idx];
        assert_eq!(placeholder.role, "assistant");
        assert_eq!(placeholder.content, dots_frame(0));

        // 中断并结束后，占位消息被撤下，不残留省略号
        let key = KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL);
        app.handle_key(key).await.unwrap();
        app.drain_stream().await;

        assert!(app.placeholder_index.is_none());
        assert!(app.stream_started_at.is_none());
        assert!(!app.chat.messages().iter().any(is_dots_placeholder));
        assert!(
            app.chat
                .messages()
                .iter()
                .any(|m| m.role == "assistant" && m.content == "生成已取消")
        );
    }

    #[tokio::test]
    async fn test_first_chunk_replaces_placeholder() {
        let (mut app, _dir) = create_slow_streaming_app().await;
        app.input.insert_str("hi");
        app.send_message().await.unwrap();
        assert!(app.placeholder_index.is_some());

        app.handle_stream_event(StreamEvent::Chunk(StreamChunk {
            content: Some("你".to_string()),
            reasoning_content: None,
        }))
        .await
        .unwrap();

        assert!(app.placeholder_index.is_none());
        assert!(app.streaming_message_added);
        assert!(!app.chat.messages().iter().any(is_dots_placeholder));
        let last = app.chat.messages().last().unwrap();
        assert_eq!(last.role, "assistant");
        assert_eq!(last.content, "你");

        let key = KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL);
        app.handle_key(key).await.unwrap();
        app.drain_stream().await;
        assert!(!app.chat.messages().iter().any(is_dots_placeholder));
    }

    #[tokio::test]
    async fn test_placeholder_removed_when_tool_events_arrive_first() {
        let (mut app, _dir) = create_slow_streaming_app().await;
        app.input.insert_str("hi");
        app.send_message().await.unwrap();

        // 工具事件先到：占位消息不再是最后一条
        app.handle_stream_event(StreamEvent::ToolEvent(RunnerEvent::Plan {
            steps: vec!["步骤一".to_string()],
        }))
        .await
        .unwrap();
        assert!(app.placeholder_index.is_some());
        assert_eq!(app.chat.messages().last().unwrap().role, "tool");

        // 首个输出块到达时仍能正确定位并撤下占位消息
        app.handle_stream_event(StreamEvent::Chunk(StreamChunk {
            content: Some("hi".to_string()),
            reasoning_content: None,
        }))
        .await
        .unwrap();

        assert!(app.placeholder_index.is_none());
        assert!(!app.chat.messages().iter().any(is_dots_placeholder));
        assert_eq!(app.chat.messages().last().unwrap().content, "hi");

        let key = KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL);
        app.handle_key(key).await.unwrap();
        app.drain_stream().await;
    }

    #[tokio::test]
    async fn test_draw_shows_streaming_elapsed_and_ctrl_c_hint() {
        let (mut app, _dir) = create_test_app().await;
        app.status = AppStatus::Streaming;
        app.stream_started_at = Some(Instant::now());
        let backend = ratatui::backend::TestBackend::new(100, 24);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal.draw(|f| app.draw(f)).unwrap();
        let content = compact_buffer_text(terminal.backend().buffer());
        assert!(content.contains("思考中"));
        assert!(content.contains("(0s)"));
        assert!(content.contains("Ctrl+C"));
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
            ..Default::default()
        });
        let mut app = App::new(
            store,
            client,
            Arc::new(Mutex::new(registry)),
            MultimodalConfig::default(),
            test_model_info(),
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
            ..Default::default()
        });
        let mut app = App::new(
            store,
            client,
            Arc::new(Mutex::new(registry)),
            MultimodalConfig::default(),
            test_model_info(),
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

    #[tokio::test]
    async fn test_new_command_starts_new_session() {
        let (mut app, _dir) = create_test_app().await;
        let old_session = app.session_id.clone();
        type_text(&mut app, "/help").await;
        press_enter(&mut app).await;
        assert!(!app.chat.messages().is_empty());

        type_text(&mut app, "/new").await;
        press_enter(&mut app).await;

        assert_ne!(app.session_id, old_session);
        assert!(
            app.store
                .get_session(&app.session_id)
                .await
                .unwrap()
                .is_some()
        );
        // 旧消息被清空，只有一条新会话提示
        assert_eq!(app.chat.messages().len(), 1);
        let last = app.chat.messages().last().unwrap();
        assert_eq!(last.role, "system");
        assert!(last.content.contains("已开始新会话"));
        assert!(app.tool_events.is_empty());
        assert_eq!(app.status, AppStatus::Idle);
    }

    #[tokio::test]
    async fn test_model_command_shows_model_and_base_url() {
        let (mut app, _dir) = create_test_app().await;
        type_text(&mut app, "/model").await;
        press_enter(&mut app).await;
        let last = app.chat.messages().last().unwrap();
        assert_eq!(last.role, "system");
        assert!(last.content.contains("test-model"));
        assert!(last.content.contains("https://test.example/v1"));
    }

    #[tokio::test]
    async fn test_draw_shows_status_info() {
        let (app, dir) = create_test_app().await;
        let backend = ratatui::backend::TestBackend::new(100, 24);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal.draw(|f| app.draw(f)).unwrap();
        let content = compact_buffer_text(terminal.backend().buffer());
        assert!(content.contains("test-model"));
        assert!(content.contains(dir.path().to_str().unwrap()));
        assert!(content.contains(&app.session_id[..8]));
        assert!(content.contains("手动审批"));
        assert!(content.contains("就绪"));
    }

    #[tokio::test]
    async fn test_draw_shows_yolo_status_when_enabled() {
        let (mut app, _dir) = create_test_app().await;
        type_text(&mut app, "/yolo").await;
        press_enter(&mut app).await;
        let backend = ratatui::backend::TestBackend::new(100, 24);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal.draw(|f| app.draw(f)).unwrap();
        let content = compact_buffer_text(terminal.backend().buffer());
        assert!(content.contains("YOLO开"));
    }

    #[tokio::test]
    async fn test_draw_shows_welcome_when_chat_empty() {
        let (app, _dir) = create_test_app().await;
        let backend = ratatui::backend::TestBackend::new(100, 30);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal.draw(|f| app.draw(f)).unwrap();
        let content = compact_buffer_text(terminal.backend().buffer());
        assert!(content.contains("Clerk"));
        assert!(content.contains("输入消息开始对话"));
    }

    #[tokio::test]
    async fn test_draw_shows_command_suggestions() {
        let (mut app, _dir) = create_test_app().await;
        type_text(&mut app, "/").await;
        let backend = ratatui::backend::TestBackend::new(100, 24);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal.draw(|f| app.draw(f)).unwrap();
        let content = compact_buffer_text(terminal.backend().buffer());
        assert!(content.contains("/help"));
        assert!(content.contains("显示帮助"));
        assert!(content.contains("/model"));
    }

    #[tokio::test]
    async fn test_draw_shows_placeholder_when_input_empty() {
        let (app, _dir) = create_test_app().await;
        let backend = ratatui::backend::TestBackend::new(100, 24);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal.draw(|f| app.draw(f)).unwrap();
        let content = compact_buffer_text(terminal.backend().buffer());
        assert!(content.contains("输入消息，/查看命令"));
    }

    #[test]
    fn test_short_id() {
        assert_eq!(short_id("abcdefgh-1234-5678"), "abcdefgh");
        assert_eq!(short_id("short"), "short");
    }

    #[test]
    fn test_complete_file_path() {
        let dir = TempDir::new().unwrap();
        std::fs::write(dir.path().join("photo.png"), "img").unwrap();
        std::fs::write(dir.path().join("phone.txt"), "txt").unwrap();
        std::fs::write(dir.path().join("pic.png"), "img").unwrap();
        std::fs::create_dir(dir.path().join("sub")).unwrap();
        std::fs::write(dir.path().join("sub").join("inner.png"), "img").unwrap();
        let wd = dir.path();

        // 唯一文件匹配补全为完整文件名
        assert_eq!(complete_file_path("pi", wd), Some("pic.png".to_string()));
        // 多个匹配补全到最长公共前缀
        assert_eq!(complete_file_path("ph", wd), Some("pho".to_string()));
        // 已是最长公共前缀时不再变化
        assert_eq!(complete_file_path("pho", wd), None);
        // 目录匹配追加 /
        assert_eq!(complete_file_path("su", wd), Some("sub/".to_string()));
        // 子目录内补全
        assert_eq!(
            complete_file_path("sub/in", wd),
            Some("sub/inner.png".to_string())
        );
        // 无匹配
        assert_eq!(complete_file_path("zzz", wd), None);
    }

    #[tokio::test]
    async fn test_attach_path_tab_completion() {
        let (mut app, dir) = create_multimodal_app().await;
        let img_path = dir.path().join("photo.png");
        let img = image::RgbImage::new(10, 10);
        img.save(&img_path).unwrap();

        type_text(&mut app, "/attach ph").await;
        app.handle_key(KeyEvent::from(KeyCode::Tab)).await.unwrap();
        assert_eq!(app.input.text(), "/attach photo.png");
    }
}
