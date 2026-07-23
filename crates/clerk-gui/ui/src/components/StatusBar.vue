<template>
  <div class="status-bar">
    <span class="status-item">
      <el-icon><Monitor /></el-icon>
      {{ sessionStore.model }}
    </span>
    <span class="status-item">
      <el-icon><FolderOpened /></el-icon>
      {{ sessionStore.workingDir }}
    </span>
    <span class="status-item" v-if="sessionStore.sessionId">
      <el-icon><Key /></el-icon>
      {{ shortSessionId }}
    </span>
    <span class="status-item" v-if="chatStore.isStreaming">
      <el-icon class="is-loading"><Loading /></el-icon>
      生成中 {{ elapsed }}s
    </span>
    <span class="status-item" v-if="sessionStore.yoloMode">
      <el-tag size="small" type="danger" effect="dark">YOLO</el-tag>
    </span>
  </div>
</template>

<script setup lang="ts">
import { computed, ref, watch, onUnmounted } from 'vue'
import { Monitor, FolderOpened, Key, Loading } from '@element-plus/icons-vue'
import { useChatStore } from '../stores/chat'
import { useSessionStore } from '../stores/session'

const chatStore = useChatStore()
const sessionStore = useSessionStore()
const elapsed = ref(0)
let timer: ReturnType<typeof setInterval> | null = null

const shortSessionId = computed(() => {
  return sessionStore.sessionId.length > 8
    ? sessionStore.sessionId.slice(0, 8) + '…'
    : sessionStore.sessionId
})

// 流式计时
watch(
  () => chatStore.isStreaming,
  (streaming) => {
    if (streaming) {
      elapsed.value = 0
      timer = setInterval(() => {
        elapsed.value++
      }, 1000)
    } else {
      if (timer) {
        clearInterval(timer)
        timer = null
      }
    }
  },
)

onUnmounted(() => {
  if (timer) clearInterval(timer)
})
</script>

<style scoped>
.status-bar {
  display: flex;
  gap: 16px;
  padding: 4px 12px;
  background: var(--clerk-sidebar, #f5f7fa);
  border-top: 1px solid var(--clerk-border, #e4e7ed);
  font-size: 12px;
  color: #909399;
  align-items: center;
  flex-shrink: 0;
}

.status-item {
  display: flex;
  align-items: center;
  gap: 4px;
  white-space: nowrap;
}

.status-item .el-icon {
  font-size: 14px;
}
</style>
