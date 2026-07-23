<template>
  <div class="chat-input-wrapper">
    <!-- 附件预览 -->
    <div v-if="chatStore.attachments.length" class="attachment-preview">
      <el-tag
        v-for="att in chatStore.attachments"
        :key="att.path"
        closable
        :type="att.warning ? 'warning' : 'info'"
        :disable-transitions="true"
        @close="removeAttachment(att.path)"
      >
        {{ att.name }}
      </el-tag>
    </div>

    <div class="input-row">
      <el-input
        ref="inputRef"
        v-model="inputText"
        type="textarea"
        :rows="3"
        :disabled="chatStore.isStreaming"
        placeholder="输入消息，Enter 发送，Shift+Enter 换行"
        resize="none"
        @keydown.enter.exact="handleSend"
        @keydown.shift.enter="handleShiftEnter"
        @paste="handlePaste"
      />
      <el-button
        type="primary"
        :loading="chatStore.isStreaming"
        :disabled="!inputText.trim() && !chatStore.attachments.length"
        @click="handleSend"
      >
        发送
      </el-button>
    </div>
  </div>
</template>

<script setup lang="ts">
import { ref } from 'vue'
import { ElMessage } from 'element-plus'
import { useChatStore } from '../stores/chat'
import * as api from '../api'

const chatStore = useChatStore()
const inputRef = ref()
const inputText = ref('')

function handleShiftEnter(_e: KeyboardEvent) {
  // 让默认的 Shift+Enter 插入换行
  // 无需额外处理
}

async function handleSend() {
  const text = inputText.value.trim()
  if (!text && !chatStore.attachments.length) return

  // 添加用户消息到列表
  chatStore.addMessage({
    role: 'user',
    content: text || '(附件)',
    created_at: new Date().toISOString(),
  })

  inputText.value = ''
  chatStore.isStreaming = true

  try {
    await api.sendMessage(text)
  } catch (err) {
    ElMessage.error(`发送失败: ${err}`)
    chatStore.isStreaming = false
  }

  // 清空附件
  chatStore.clearAttachments()
}

async function handlePaste(e: ClipboardEvent) {
  const items = e.clipboardData?.items
  if (!items) return

  for (const item of Array.from(items)) {
    if (item.kind !== 'file') continue
    const file = item.getAsFile()
    if (!file) continue

    const type = file.type
    // 只处理图片和视频
    if (!type.startsWith('image/') && !type.startsWith('video/')) continue

    e.preventDefault()

    const reader = new FileReader()
    reader.onload = async () => {
      const dataUrl = reader.result as string
      // data:image/png;base64,xxxx → 取 base64 部分
      const base64 = dataUrl.split(',')[1]
      try {
        const result = await api.attachMedia(file.name, type, base64)
        chatStore.addAttachment(result)
        if (result.warning) {
          ElMessage.warning(result.warning)
        }
      } catch (err) {
        ElMessage.error(`附件上传失败: ${err}`)
      }
    }
    reader.readAsDataURL(file)
  }
}

function removeAttachment(path: string) {
  chatStore.attachments = chatStore.attachments.filter((a) => a.path !== path)
}
</script>

<style scoped>
.chat-input-wrapper {
  padding: 12px 16px;
  border-top: 1px solid var(--clerk-border, #e4e7ed);
  background: var(--clerk-bg, #fff);
}

.attachment-preview {
  display: flex;
  flex-wrap: wrap;
  gap: 6px;
  margin-bottom: 8px;
}

.input-row {
  display: flex;
  gap: 8px;
  align-items: flex-end;
}

.input-row .el-input {
  flex: 1;
}

.input-row .el-button {
  height: 74px; /* match textarea 3 rows */
}
</style>
