import { invoke } from '@tauri-apps/api/core'
import { listen, type UnlistenFn } from '@tauri-apps/api/event'
import type {
  HistoryMessage,
  AttachmentPayload,
  ChunkPayload,
  ToolEventPayload,
  ApprovalPayload,
  FilePayload,
  DonePayload,
} from './types'

// ── Invoke ──────────────────────────────────────────────

/** 发送用户消息 */
export function sendMessage(text: string): Promise<void> {
  return invoke('send_message', { message: text })
}

/** 获取历史消息 */
export function getHistory(): Promise<HistoryMessage[]> {
  return invoke('get_history')
}

/** 添加媒体附件（base64） */
export function attachMedia(
  name: string,
  mime: string,
  data: string,
): Promise<AttachmentPayload> {
  return invoke('attach_media', { name, mime, data })
}

/** 保存文件到工作目录 */
export function saveFile(path: string): Promise<string> {
  return invoke('save_file', { path })
}

/** 另存为（用户取消时返回 null） */
export function saveFileAs(path: string): Promise<string | null> {
  return invoke('save_file_as', { path })
}

/** 审批响应 */
export function respondApproval(approved: boolean): Promise<void> {
  return invoke('respond_approval', { approved })
}

/** 会话元数据 */
export interface SessionMeta {
  session_id: string
  model: string
  base_url: string
  working_dir: string
  yolo: boolean
}

/** 获取会话元数据 */
export function getSessionMeta(): Promise<SessionMeta> {
  return invoke('get_session_meta')
}

// ── Listen ──────────────────────────────────────────────

/** 流式输出块 */
export function onChunk(
  cb: (payload: ChunkPayload) => void,
): Promise<UnlistenFn> {
  return listen<ChunkPayload>('clerk-chunk', (event) => cb(event.payload))
}

/** 工具事件 */
export function onTool(
  cb: (payload: ToolEventPayload) => void,
): Promise<UnlistenFn> {
  return listen<ToolEventPayload>('clerk-tool', (event) => cb(event.payload))
}

/** 审批请求 */
export function onApproval(
  cb: (payload: ApprovalPayload) => void,
): Promise<UnlistenFn> {
  return listen<ApprovalPayload>('clerk-approval', (event) =>
    cb(event.payload),
  )
}

/** 产出文件通知 */
export function onFile(
  cb: (payload: FilePayload) => void,
): Promise<UnlistenFn> {
  return listen<FilePayload>('clerk-file', (event) => cb(event.payload))
}

/** 生成完成 */
export function onDone(
  cb: (payload: DonePayload) => void,
): Promise<UnlistenFn> {
  return listen<DonePayload>('clerk-done', (event) => cb(event.payload))
}
