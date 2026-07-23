/** 历史消息（get_history 返回） */
export interface HistoryMessage {
  role: string
  content: string
  created_at: string
}

/** 流式输出块事件负载 */
export interface ChunkPayload {
  content?: string
  reasoning?: string
}

/** 工具事件负载 */
export interface ToolEventPayload {
  text: string
}

/** 审批请求事件负载 */
export interface ApprovalPayload {
  name: string
  arguments: Record<string, unknown>
}

/** 产出文件事件负载 */
export interface FilePayload {
  path: string
  name: string
}

/** 生成结束事件负载 */
export interface DonePayload {
  ok: boolean
  reply: string
  error?: string | null
}

/** 附件信息（attach_media 返回） */
export interface AttachmentPayload {
  path: string
  name: string
  kind: string
  warning?: string
}

/** 本地渲染用的消息类型 */
export interface Message {
  role: 'user' | 'assistant' | 'system' | 'tool'
  content: string
  reasoning?: string
  created_at: string
  files?: FilePayload[]
}
