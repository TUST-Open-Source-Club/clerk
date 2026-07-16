//! Clerk 桌面 GUI 的 Tauri 后端：复用 clerk-core 的 Plan-Execute runner，
//! 通过命令与事件和纯 HTML/JS 前端交互。
//!
//! - 命令：`send_message` / `get_history` / `attach_media` / `save_file` /
//!   `save_file_as` / `respond_approval`
//! - 事件：`clerk-chunk`（流式输出块）、`clerk-tool`（工具事件）、
//!   `clerk-approval`（工具审批请求）、`clerk-file`（产出的文件）、
//!   `clerk-done`（本轮生成结束）

use anyhow::{Context, Result};
use base64::{Engine as _, engine::general_purpose::STANDARD};
use serde::Serialize;
use serde_json::Value;
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use tauri::{AppHandle, Emitter, Manager, State};
use tokio::sync::{Mutex, mpsc, oneshot};

use clerk_core::agent::llm::LlmClient;
use clerk_core::agent::runner::{ApprovalRequest, PlanExecuteRunner, RunnerEvent};
use clerk_core::agent::session::SessionContext;
use clerk_core::bootstrap::{create_llm_client, create_tool_registry};
use clerk_core::config::{Config, MultimodalConfig};
use clerk_core::media::{media_kind, with_attachments};
use clerk_core::prompt::build_system_prompt;
use clerk_core::store::Store;
use clerk_core::text::format_tool_event;
use clerk_core::tools::registry::ToolRegistry;
use clerk_core::util::expand_tilde;

/// GUI 全局状态：会话、runner 依赖与待处理的附件/审批。
struct GuiState {
    store: Store,
    session_id: String,
    session_ctx: Arc<Mutex<SessionContext>>,
    client: Arc<dyn LlmClient>,
    registry: Arc<Mutex<ToolRegistry>>,
    multimodal: MultimodalConfig,
    context: clerk_core::config::ContextConfig,
    working_dir: PathBuf,
    /// 已附加、等待随下一条消息发送的媒体文件
    attachments: Mutex<Vec<PathBuf>>,
    /// 等待前端审批决定的工具调用响应端
    pending_approval: Mutex<Option<oneshot::Sender<bool>>>,
    /// 是否正在生成回复（防止并发发送）
    streaming: Mutex<bool>,
}

/// 流式输出块事件负载。
#[derive(Clone, Serialize)]
struct ChunkPayload {
    content: Option<String>,
    reasoning: Option<String>,
}

/// 工具事件负载（已格式化为展示文本）。
#[derive(Clone, Serialize)]
struct ToolEventPayload {
    text: String,
}

/// 审批请求事件负载。
#[derive(Clone, Serialize)]
struct ApprovalPayload {
    name: String,
    arguments: Value,
}

/// 生成结束事件负载。
#[derive(Clone, Serialize)]
struct DonePayload {
    ok: bool,
    reply: String,
    error: Option<String>,
}

/// 产出文件事件负载（文件卡片）。
#[derive(Clone, Serialize)]
struct FilePayload {
    path: String,
    name: String,
}

/// 历史消息（get_history 返回给前端）。
#[derive(Serialize)]
struct HistoryMessage {
    role: String,
    content: String,
    created_at: String,
}

/// attach_media 返回给前端的附件信息。
#[derive(Clone, Serialize)]
struct AttachmentPayload {
    path: String,
    name: String,
    kind: String,
    warning: Option<String>,
}

/// 可作为产出文件卡片展示的扩展名。
const OUTPUT_EXTENSIONS: &[&str] = &[
    "pdf", "docx", "xlsx", "pptx", "png", "jpg", "jpeg", "gif", "webp", "html", "md", "csv",
];

/// 初始化 GUI 状态：加载配置、打开存储、创建会话与工具注册表。
async fn init_state() -> Result<GuiState> {
    let config = Config::load(None)?;
    config.validate()?;

    let working_dir = config
        .working_dir
        .clone()
        .map(expand_tilde)
        .unwrap_or_else(|| std::env::current_dir().unwrap());
    std::env::set_current_dir(&working_dir)
        .with_context(|| format!("无法切换到工作目录: {}", working_dir.display()))?;

    let db_path = match &config.storage.db_path {
        Some(p) => p.clone(),
        None => Config::default_db_path()?,
    };
    let store = Store::open(&db_path).await?;

    let client = create_llm_client(&config)?;
    let registry = Arc::new(Mutex::new(create_tool_registry(
        &working_dir,
        client.clone(),
        config.permissions.clone(),
    )));

    let session_id = uuid::Uuid::new_v4().to_string();
    store.create_session(&session_id, Some("GUI 会话")).await?;

    Ok(GuiState {
        store,
        session_id,
        session_ctx: Arc::new(Mutex::new(SessionContext::new(build_system_prompt()))),
        client,
        registry,
        multimodal: config.multimodal.clone(),
        context: config.context.clone(),
        working_dir,
        attachments: Mutex::new(Vec::new()),
        pending_approval: Mutex::new(None),
        streaming: Mutex::new(false),
    })
}

/// 发送用户消息：拼接附件描述、持久化，然后驱动 PlanExecuteRunner，
/// 并把流式块/工具事件/审批请求桥接为 Tauri 事件。
#[tauri::command]
async fn send_message(
    app: AppHandle,
    state: State<'_, GuiState>,
    message: String,
) -> Result<(), String> {
    {
        let mut streaming = state.streaming.lock().await;
        if *streaming {
            return Err("正在生成回复，请稍候".to_string());
        }
        *streaming = true;
    }

    let result = run_agent(&app, &state, message).await;

    // 兜底：若生成结束时仍有未应答的审批，按拒绝处理，避免 runner 悬挂
    if let Some(respond) = state.pending_approval.lock().await.take() {
        let _ = respond.send(false);
    }
    *state.streaming.lock().await = false;
    result
}

/// 实际驱动 runner 的内部流程。
async fn run_agent(app: &AppHandle, state: &GuiState, message: String) -> Result<(), String> {
    let text = message.trim().to_string();
    if text.is_empty() && state.attachments.lock().await.is_empty() {
        return Err("消息不能为空".to_string());
    }

    let attachments = std::mem::take(&mut *state.attachments.lock().await);
    let text = with_attachments(text, &attachments).await;

    state
        .store
        .add_message(&state.session_id, "user", &text)
        .await
        .map_err(|e| format!("保存用户消息失败: {:#}", e))?;

    let (chunk_tx, mut chunk_rx) = mpsc::unbounded_channel();
    let (event_tx, mut event_rx) = mpsc::unbounded_channel::<RunnerEvent>();
    let (approval_tx, mut approval_rx) = mpsc::unbounded_channel::<ApprovalRequest>();

    let runner = PlanExecuteRunner::new(state.client.clone(), state.registry.clone())
        .with_approval_tx(approval_tx)
        .with_context_config(state.context.clone());
    let ctx = state.session_ctx.clone();
    let mut run = std::pin::pin!(runner.run_stream(ctx, &text, chunk_tx, Some(event_tx)));

    // 记录每个工具最近一次调用的参数，用于在 ToolResult 时提取产出文件
    let mut call_args: HashMap<String, Value> = HashMap::new();
    let mut emitted_files: HashSet<PathBuf> = HashSet::new();

    let result = loop {
        tokio::select! {
            maybe_chunk = chunk_rx.recv() => {
                if let Some(chunk) = maybe_chunk {
                    let _ = app.emit("clerk-chunk", ChunkPayload {
                        content: chunk.content,
                        reasoning: chunk.reasoning_content,
                    });
                }
            }
            maybe_event = event_rx.recv() => {
                if let Some(event) = maybe_event {
                    handle_runner_event(app, state, &event, &mut call_args, &mut emitted_files).await;
                }
            }
            maybe_approval = approval_rx.recv() => {
                if let Some(req) = maybe_approval {
                    // 理论上同时只有一个审批；旧的按拒绝处理避免悬挂
                    if let Some(old) = state.pending_approval.lock().await.take() {
                        let _ = old.send(false);
                    }
                    *state.pending_approval.lock().await = Some(req.respond);
                    let _ = app.emit("clerk-approval", ApprovalPayload {
                        name: req.name,
                        arguments: req.arguments,
                    });
                }
            }
            r = &mut run => break r,
        }
    };

    // 排干通道中残留的事件与输出块，避免丢失结尾内容
    while let Ok(chunk) = chunk_rx.try_recv() {
        let _ = app.emit(
            "clerk-chunk",
            ChunkPayload {
                content: chunk.content,
                reasoning: chunk.reasoning_content,
            },
        );
    }
    while let Ok(event) = event_rx.try_recv() {
        handle_runner_event(app, state, &event, &mut call_args, &mut emitted_files).await;
    }

    let (ok, reply, error) = match result {
        Ok(text) => (true, text, None),
        Err(e) => (false, String::new(), Some(format!("{:#}", e))),
    };

    let store_content = if ok {
        reply.clone()
    } else {
        format!("处理失败: {}", error.clone().unwrap_or_default())
    };
    let _ = state
        .store
        .add_message(&state.session_id, "assistant", &store_content)
        .await;

    let _ = app.emit("clerk-done", DonePayload { ok, reply, error });
    Ok(())
}

/// 处理单个 runner 事件：格式化为工具消息事件，并提取产出文件发送文件事件。
async fn handle_runner_event(
    app: &AppHandle,
    state: &GuiState,
    event: &RunnerEvent,
    call_args: &mut HashMap<String, Value>,
    emitted_files: &mut HashSet<PathBuf>,
) {
    match event {
        RunnerEvent::ToolCall { name, arguments } => {
            call_args.insert(name.clone(), arguments.clone());
        }
        RunnerEvent::ToolResult { name, result } => {
            let args = call_args.get(name).cloned().unwrap_or(Value::Null);
            for path in extract_output_files(name, &args, result, &state.working_dir) {
                if emitted_files.insert(path.clone()) {
                    let file_name = path
                        .file_name()
                        .map(|n| n.to_string_lossy().to_string())
                        .unwrap_or_default();
                    let _ = app.emit(
                        "clerk-file",
                        FilePayload {
                            path: path.to_string_lossy().to_string(),
                            name: file_name,
                        },
                    );
                }
            }
        }
        _ => {}
    }

    let _ = app.emit(
        "clerk-tool",
        ToolEventPayload {
            text: format_tool_event(event),
        },
    );
}

/// 返回当前会话的历史消息。
#[tauri::command]
async fn get_history(state: State<'_, GuiState>) -> Result<Vec<HistoryMessage>, String> {
    state
        .store
        .list_messages(&state.session_id)
        .await
        .map(|messages| {
            messages
                .iter()
                .map(|m| HistoryMessage {
                    role: m.role.clone(),
                    content: m.content.clone(),
                    created_at: m.created_at.to_rfc3339(),
                })
                .collect()
        })
        .map_err(|e| format!("读取历史消息失败: {:#}", e))
}

/// 接收前端粘贴/上传的媒体文件（base64），写入附件目录并登记为待发送附件。
#[tauri::command]
async fn attach_media(
    state: State<'_, GuiState>,
    name: String,
    mime: String,
    data: String,
) -> Result<AttachmentPayload, String> {
    let bytes = STANDARD
        .decode(data)
        .map_err(|e| format!("base64 解码失败: {}", e))?;

    let dir = state.working_dir.join(".clerk-attachments");
    tokio::fs::create_dir_all(&dir)
        .await
        .map_err(|e| format!("创建附件目录失败: {}", e))?;

    let file_name = attachment_file_name(&name, &mime);
    let path = dir.join(format!("{}-{}", uuid::Uuid::new_v4().simple(), file_name));
    tokio::fs::write(&path, &bytes)
        .await
        .map_err(|e| format!("写入附件失败: {}", e))?;

    let Some(kind) = media_kind(&path) else {
        let _ = tokio::fs::remove_file(&path).await;
        return Err("仅支持图片或视频附件".to_string());
    };

    let warning = if kind == "image" && !state.multimodal.supports_images {
        Some("当前模型未启用图片输入支持".to_string())
    } else if kind == "video" && !state.multimodal.supports_video {
        Some("当前模型未启用视频输入支持".to_string())
    } else {
        None
    };

    state.attachments.lock().await.push(path.clone());
    Ok(AttachmentPayload {
        path: path.to_string_lossy().to_string(),
        name: file_name,
        kind: kind.to_string(),
        warning,
    })
}

/// 保存产出文件：若已在工作目录中则直接返回；否则复制到工作目录。
#[tauri::command]
async fn save_file(state: State<'_, GuiState>, path: String) -> Result<String, String> {
    let src = resolve_in_working_dir(&state.working_dir, &path);
    if !src.is_file() {
        return Err(format!("文件不存在: {}", src.display()));
    }

    if src.starts_with(&state.working_dir) {
        return Ok(src.to_string_lossy().to_string());
    }

    let dest = unique_dest(&state.working_dir, &src);
    tokio::fs::copy(&src, &dest)
        .await
        .map_err(|e| format!("保存文件失败: {}", e))?;
    Ok(dest.to_string_lossy().to_string())
}

/// 另存为：通过 Tauri dialog 插件弹出保存对话框，把产出文件复制到所选位置。
/// 用户取消时返回 None。
#[tauri::command]
async fn save_file_as(
    app: AppHandle,
    state: State<'_, GuiState>,
    path: String,
) -> Result<Option<String>, String> {
    use tauri_plugin_dialog::DialogExt;

    let src = resolve_in_working_dir(&state.working_dir, &path);
    if !src.is_file() {
        return Err(format!("文件不存在: {}", src.display()));
    }

    let file_name = src
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_else(|| "output".to_string());

    let app_clone = app.clone();
    let target = tauri::async_runtime::spawn_blocking(move || {
        app_clone
            .dialog()
            .file()
            .set_file_name(&file_name)
            .blocking_save_file()
    })
    .await
    .map_err(|e| format!("保存对话框失败: {}", e))?;

    let Some(target) = target else {
        return Ok(None);
    };
    let target_path = target
        .into_path()
        .map_err(|e| format!("无效的保存路径: {}", e))?;

    tokio::fs::copy(&src, &target_path)
        .await
        .map_err(|e| format!("保存文件失败: {}", e))?;
    Ok(Some(target_path.to_string_lossy().to_string()))
}

/// 前端对工具审批请求做出决定：true 批准，false 拒绝。
#[tauri::command]
async fn respond_approval(state: State<'_, GuiState>, approved: bool) -> Result<(), String> {
    match state.pending_approval.lock().await.take() {
        Some(respond) => {
            let _ = respond.send(approved);
            Ok(())
        }
        None => Err("当前没有待审批的工具调用".to_string()),
    }
}

/// 从工具调用参数与结果文本中提取产出文件路径（限已知输出扩展名且文件已存在）。
fn extract_output_files(
    tool_name: &str,
    arguments: &Value,
    result: &str,
    working_dir: &Path,
) -> Vec<PathBuf> {
    let mut found: Vec<PathBuf> = Vec::new();

    // 1. 已知产出工具的输出参数
    let keys: &[&str] = match tool_name {
        "fs_write" | "office_write_excel" | "office_write_word" => &["path"],
        "office_render" | "pdf_merge" | "pdf_split" | "poster" | "render_to_image" => &["output"],
        _ => &[],
    };
    if let Some(map) = arguments.as_object() {
        for key in keys {
            if let Some(s) = map.get(*key).and_then(|v| v.as_str()) {
                push_candidate(&mut found, s, working_dir);
            }
        }
    }

    // 2. 结果文本中提及的文件路径
    for token in result.split(|c: char| c.is_whitespace() || c == '"' || c == '\'') {
        let trimmed =
            token.trim_matches(|c| matches!(c, ',' | ';' | '。' | '：' | ':' | ')' | '('));
        push_candidate(&mut found, trimmed, working_dir);
    }

    found
}

/// 若字符串指向一个已存在且扩展名已知的目标文件，则加入候选列表（去重）。
fn push_candidate(found: &mut Vec<PathBuf>, s: &str, working_dir: &Path) {
    let path = Path::new(s);
    let Some(ext) = path.extension().and_then(|e| e.to_str()) else {
        return;
    };
    if !OUTPUT_EXTENSIONS.contains(&ext.to_lowercase().as_str()) {
        return;
    }
    let resolved = resolve_in_working_dir(working_dir, s);
    if !resolved.is_file() || found.contains(&resolved) {
        return;
    }
    found.push(resolved);
}

/// 相对路径基于工作目录解析，绝对路径原样返回（支持 ~ 展开）。
fn resolve_in_working_dir(working_dir: &Path, input: &str) -> PathBuf {
    let path = expand_tilde(input);
    if path.is_absolute() {
        path
    } else {
        working_dir.join(path)
    }
}

/// 在工作目录中为源文件构造不重名的目标路径（必要时追加 (1) 等后缀）。
fn unique_dest(working_dir: &Path, src: &Path) -> PathBuf {
    let stem = src
        .file_stem()
        .map(|s| s.to_string_lossy().to_string())
        .unwrap_or_else(|| "file".to_string());
    let ext = src.extension().map(|e| e.to_string_lossy().to_string());

    for i in 0..1000 {
        let name = match (&ext, i) {
            (None, 0) => stem.clone(),
            (Some(e), 0) => format!("{}.{}", stem, e),
            (None, n) => format!("{} ({})", stem, n),
            (Some(e), n) => format!("{} ({}).{}", stem, n, e),
        };
        let dest = working_dir.join(&name);
        if !dest.exists() {
            return dest;
        }
    }
    working_dir.join(format!("{}-{}", uuid::Uuid::new_v4().simple(), stem))
}

/// 生成附件文件名：保留原扩展名，缺省时按 MIME 推断；清理不安全字符。
fn attachment_file_name(name: &str, mime: &str) -> String {
    let sanitized: String = name
        .chars()
        .map(|c| {
            if c.is_alphanumeric() || matches!(c, '.' | '-' | '_') {
                c
            } else {
                '_'
            }
        })
        .collect();
    let sanitized = sanitized.trim_matches('.').to_string();

    if Path::new(&sanitized).extension().is_some() && !sanitized.is_empty() {
        return sanitized;
    }

    let ext = match mime {
        "image/png" => "png",
        "image/jpeg" => "jpg",
        "image/gif" => "gif",
        "image/webp" => "webp",
        "image/bmp" => "bmp",
        "video/mp4" => "mp4",
        "video/webm" => "webm",
        "video/quicktime" => "mov",
        _ => "bin",
    };
    let base = if sanitized.is_empty() {
        "pasted".to_string()
    } else {
        sanitized
    };
    format!("{}.{}", base, ext)
}

/// 启动 Tauri 应用。
#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_dialog::init())
        .setup(|app| {
            let state = tauri::async_runtime::block_on(init_state())?;
            app.manage(state);
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            send_message,
            get_history,
            attach_media,
            save_file,
            save_file_as,
            respond_approval,
        ])
        .run(tauri::generate_context!())
        .expect("运行 clerk-gui 失败");
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_attachment_file_name_keeps_extension() {
        assert_eq!(attachment_file_name("photo.png", "image/png"), "photo.png");
        assert_eq!(
            attachment_file_name("我的 图.jpg", "image/jpeg"),
            "我的_图.jpg"
        );
    }

    #[test]
    fn test_attachment_file_name_from_mime() {
        assert_eq!(attachment_file_name("", "image/png"), "pasted.png");
        assert_eq!(attachment_file_name("clip", "video/mp4"), "clip.mp4");
        assert_eq!(
            attachment_file_name("file", "application/octet-stream"),
            "file.bin"
        );
    }

    #[test]
    fn test_extract_output_files_from_args() {
        let dir = tempfile::TempDir::new().unwrap();
        let out = dir.path().join("report.pdf");
        std::fs::write(&out, b"%PDF").unwrap();

        let args = serde_json::json!({"output": "report.pdf"});
        let found = extract_output_files("pdf_merge", &args, "完成", dir.path());
        assert_eq!(found, vec![out]);
    }

    #[test]
    fn test_extract_output_files_from_result_text() {
        let dir = tempfile::TempDir::new().unwrap();
        let out = dir.path().join("海报.png");
        std::fs::write(&out, b"png").unwrap();

        let result = format!("海报已生成: {}", out.display());
        let found = extract_output_files("poster", &Value::Null, &result, dir.path());
        assert_eq!(found, vec![out]);
    }

    #[test]
    fn test_extract_output_files_ignores_input_and_missing() {
        let dir = tempfile::TempDir::new().unwrap();
        let input = dir.path().join("input.md");
        std::fs::write(&input, b"md").unwrap();

        // fs_read 不是产出工具，path 参数不应当作产出文件
        let args = serde_json::json!({"path": "input.md"});
        let found = extract_output_files("fs_read", &args, "内容", dir.path());
        assert!(found.is_empty());

        // 不存在的文件不产生卡片
        let args = serde_json::json!({"output": "missing.pdf"});
        let found = extract_output_files("pdf_merge", &args, "完成", dir.path());
        assert!(found.is_empty());
    }

    #[test]
    fn test_unique_dest_avoids_overwrite() {
        let dir = tempfile::TempDir::new().unwrap();
        let src = dir.path().join("a.pdf");
        std::fs::write(&src, b"1").unwrap();

        let dest1 = unique_dest(dir.path(), &src);
        assert_eq!(dest1, dir.path().join("a (1).pdf"));
        std::fs::write(&dest1, b"2").unwrap();
        let dest2 = unique_dest(dir.path(), &src);
        assert_eq!(dest2, dir.path().join("a (2).pdf"));
    }

    #[test]
    fn test_resolve_in_working_dir() {
        let wd = Path::new("/tmp/wd");
        assert_eq!(resolve_in_working_dir(wd, "a/b.pdf"), wd.join("a/b.pdf"));
        assert_eq!(
            resolve_in_working_dir(wd, "/abs/c.pdf"),
            PathBuf::from("/abs/c.pdf")
        );
    }
}
