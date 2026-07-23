<template>
  <div class="message-bubble" :class="[`role-${msg.role}`, { 'is-streaming': isStreaming }]">
    <!-- 角色标签 -->
    <div class="message-role">
      <el-tag :type="roleTagType" size="small" effect="plain">
        {{ roleLabel }}
      </el-tag>
    </div>

    <!-- reasoning 思考过程（可折叠） -->
    <el-collapse v-if="msg.reasoning" class="reasoning-collapse">
      <el-collapse-item title="思考过程" name="reasoning">
        <pre class="reasoning-content">{{ msg.reasoning }}</pre>
      </el-collapse-item>
    </el-collapse>

    <!-- 内容（Markdown 渲染） -->
    <div class="message-content" v-html="renderedContent" />

    <!-- 文件卡片 -->
    <div v-if="msg.files && msg.files.length" class="message-files">
      <FileChip
        v-for="file in msg.files"
        :key="file.path"
        :file="file"
      />
    </div>
  </div>
</template>

<script setup lang="ts">
import { computed } from 'vue'
import type { Message } from '../types'
import MarkdownIt from 'markdown-it'
import FileChip from './FileChip.vue'

const md = new MarkdownIt({
  html: true,
  linkify: true,
  typographer: true,
})

const props = defineProps<{
  msg: Message
  isStreaming?: boolean
}>()

const roleTagType = computed(() => {
  switch (props.msg.role) {
    case 'user':
      return 'primary'
    case 'assistant':
      return 'success'
    case 'system':
      return 'info'
    case 'tool':
      return 'warning'
    default:
      return 'info'
  }
})

const roleLabel = computed(() => {
  switch (props.msg.role) {
    case 'user':
      return '你'
    case 'assistant':
      return 'Clerk'
    case 'system':
      return '系统'
    case 'tool':
      return '工具'
    default:
      return props.msg.role
  }
})

const renderedContent = computed(() => {
  return md.render(props.msg.content)
})
</script>

<style scoped>
.message-bubble {
  margin-bottom: 16px;
  max-width: 85%;
}

.message-bubble.role-user {
  margin-left: auto;
}

.message-bubble.role-assistant {
  margin-right: auto;
}

.message-role {
  margin-bottom: 4px;
}

.reasoning-collapse {
  margin-bottom: 8px;
}

.reasoning-content {
  font-size: 13px;
  color: #909399;
  background: #f5f7fa;
  padding: 8px;
  border-radius: 4px;
  white-space: pre-wrap;
  word-break: break-all;
  max-height: 200px;
  overflow-y: auto;
}

.message-content {
  font-size: 14px;
  line-height: 1.6;
  color: #303133;
}

.message-content :deep(p) {
  margin: 0.5em 0;
}

.message-content :deep(pre) {
  background: #f5f7fa;
  border-radius: 4px;
  padding: 12px;
  overflow-x: auto;
}

.message-content :deep(code) {
  font-family: 'SFMono-Regular', Consolas, 'Liberation Mono', Menlo, monospace;
  font-size: 13px;
}

.message-content :deep(img) {
  max-width: 100%;
  border-radius: 4px;
}

.message-files {
  margin-top: 8px;
  display: flex;
  flex-wrap: wrap;
  gap: 8px;
}

.dark .reasoning-content {
  background: #2a2a2a;
}

.dark .message-content {
  color: #e0e0e0;
}

.dark .message-content :deep(pre) {
  background: #2a2a2a;
}

.message-bubble.role-user .message-content {
  background: #ecf5ff;
  border-radius: 8px;
  padding: 8px 12px;
}

.message-bubble.role-tool .message-content {
  background: #fdf6ec;
  border-radius: 8px;
  padding: 8px 12px;
  font-size: 13px;
  color: #7c6c3a;
}
</style>
