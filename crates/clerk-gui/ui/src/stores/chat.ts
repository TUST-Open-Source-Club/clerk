import { defineStore } from 'pinia'
import { ref, computed } from 'vue'
import type {
  Message,
  ApprovalPayload,
  FilePayload,
  AttachmentPayload,
  ChunkPayload,
  DonePayload,
} from '../types'
import * as api from '../api'

export const useChatStore = defineStore('chat', () => {
  // ── State ──
  const messages = ref<Message[]>([])
  const streamingReply = ref('')
  const streamingReasoning = ref('')
  const isStreaming = ref(false)
  const attachments = ref<AttachmentPayload[]>([])
  const pendingApproval = ref<ApprovalPayload | null>(null)
  const streamingFiles = ref<FilePayload[]>([])
  const unlistenFns = ref<Array<() => void>>([])

  // ── Getters ──
  const displayMessages = computed(() => {
    if (!isStreaming.value) return messages.value
    // 构建一个临时 assistant 消息包含流式内容
    const msgs = [...messages.value]
    if (streamingReply.value || streamingReasoning.value || streamingFiles.value.length) {
      msgs.push({
        role: 'assistant',
        content: streamingReply.value,
        reasoning: streamingReasoning.value || undefined,
        created_at: new Date().toISOString(),
        files: streamingFiles.value,
      })
    }
    return msgs
  })

  // ── Actions ──
  function addMessage(msg: Message) {
    messages.value.push(msg)
  }

  function updateStreaming(chunk: ChunkPayload) {
    if (chunk.content) {
      streamingReply.value += chunk.content
    }
    if (chunk.reasoning) {
      streamingReasoning.value += chunk.reasoning
    }
  }

  function finalizeStreaming(_done: DonePayload) {
    if (streamingReply.value || streamingReasoning.value || streamingFiles.value.length) {
      messages.value.push({
        role: 'assistant',
        content: streamingReply.value,
        reasoning: streamingReasoning.value || undefined,
        created_at: new Date().toISOString(),
        files: streamingFiles.value,
      })
    }
    streamingReply.value = ''
    streamingReasoning.value = ''
    streamingFiles.value = []
    isStreaming.value = false
    pendingApproval.value = null
  }

  function addFileToLastMessage(file: FilePayload) {
    if (isStreaming.value) {
      streamingFiles.value.push(file)
      return
    }
    const idx = messages.value.length - 1
    if (idx >= 0) {
      const msg = messages.value[idx]
      if (!msg.files) msg.files = []
      msg.files.push(file)
    }
  }

  function addAttachment(att: AttachmentPayload) {
    attachments.value.push(att)
  }

  function clearAttachments() {
    attachments.value = []
  }

  function setPendingApproval(payload: ApprovalPayload | null) {
    pendingApproval.value = payload
  }

  function clearPendingApproval() {
    pendingApproval.value = null
  }

  // ── 注册事件监听 ──
  async function setupListeners() {
    // 取消旧监听
    unlistenFns.value.forEach((fn) => fn())
    unlistenFns.value = []

    unlistenFns.value.push(
      await api.onChunk((chunk) => {
        updateStreaming(chunk)
      }),
    )

    unlistenFns.value.push(
      await api.onTool((payload) => {
        // tool 事件作为 tool 角色消息显示
        messages.value.push({
          role: 'tool',
          content: payload.text,
          created_at: new Date().toISOString(),
        })
      }),
    )

    unlistenFns.value.push(
      await api.onApproval((payload) => {
        setPendingApproval(payload)
      }),
    )

    unlistenFns.value.push(
      await api.onFile((file) => {
        addFileToLastMessage(file)
      }),
    )

    unlistenFns.value.push(
      await api.onDone((done) => {
        finalizeStreaming(done)
      }),
    )
  }

  // ── 清理 ──
  function teardown() {
    unlistenFns.value.forEach((fn) => fn())
    unlistenFns.value = []
  }

  return {
    messages,
    streamingReply,
    streamingReasoning,
    isStreaming,
    attachments,
    pendingApproval,
    streamingFiles,
    displayMessages,
    addMessage,
    updateStreaming,
    finalizeStreaming,
    addFileToLastMessage,
    addAttachment,
    clearAttachments,
    setPendingApproval,
    clearPendingApproval,
    setupListeners,
    teardown,
  }
})
