<template>
  <el-container class="app-container">
    <!-- 侧边栏 -->
    <el-aside class="sidebar" width="260px">
      <div class="sidebar-header">
        <h2 class="sidebar-title">Clerk</h2>
      </div>
      <div class="sidebar-actions">
        <el-button type="primary" size="small" style="width: 100%" @click="handleNewSession">
          新建会话
        </el-button>
      </div>
      <el-divider style="margin: 8px 0" />
      <div class="sidebar-sessions">
        <div
          v-for="(session, idx) in sessionList"
          :key="idx"
          class="session-item"
          @click="handleSwitchSession(session)"
        >
          <el-icon><ChatDotRound /></el-icon>
          <span class="session-name">{{ session }}</span>
        </div>
        <el-empty v-if="!sessionList.length" :description="'暂无会话'" :image-size="60" />
      </div>
    </el-aside>

    <!-- 主聊天区 -->
    <el-container class="chat-main">
      <!-- 审批条 -->
      <ApprovalBar />

      <!-- 消息列表 -->
      <ChatList />

      <!-- 输入区 -->
      <ChatInput />
    </el-container>

    <!-- 状态栏 -->
    <StatusBar />
  </el-container>
</template>

<script setup lang="ts">
import { ref, onMounted, onUnmounted } from 'vue'
import { ElMessage } from 'element-plus'
import { ChatDotRound } from '@element-plus/icons-vue'
import { useChatStore } from './stores/chat'
import { useSessionStore } from './stores/session'
import * as api from './api'
import ChatList from './components/ChatList.vue'
import ChatInput from './components/ChatInput.vue'
import ApprovalBar from './components/ApprovalBar.vue'
import StatusBar from './components/StatusBar.vue'

const chatStore = useChatStore()
const sessionStore = useSessionStore()

// 会话列表（暂存本地，后端暂无完整会话列表 command）
const sessionList = ref<string[]>(['当前会话'])

onMounted(async () => {
  // 设置事件监听
  await chatStore.setupListeners()

  // 加载会话元数据
  try {
    const meta = await api.getSessionMeta()
    sessionStore.setSessionMeta({
      sessionId: meta.session_id,
      model: meta.model,
      workingDir: meta.working_dir,
      yoloMode: meta.yolo,
    })
  } catch (err) {
    console.error('加载会话元数据失败:', err)
  }

  // 加载历史消息
  try {
    const history = await api.getHistory()
    for (const h of history) {
      chatStore.addMessage({
        role: h.role as 'user' | 'assistant' | 'system' | 'tool',
        content: h.content,
        created_at: h.created_at,
      })
    }
  } catch (err) {
    console.error('加载历史消息失败:', err)
  }
})

onUnmounted(() => {
  chatStore.teardown()
})

async function handleNewSession() {
  // 清空当前消息
  chatStore.messages = []
  chatStore.streamingReply = ''
  chatStore.streamingReasoning = ''
  chatStore.isStreaming = false
  chatStore.pendingApproval = null
  ElMessage.success('已创建新会话')
}

async function handleSwitchSession(_session: string) {
  // TODO: 切换会话 — 需要后端提供会话列表 command 支持
  ElMessage.info('会话切换功能待实现')
}
</script>

<style scoped>
.app-container {
  height: 100vh;
  display: flex;
  flex-direction: column;
  background: var(--clerk-bg, #fff);
}

.sidebar {
  background: var(--clerk-sidebar, #f5f7fa);
  border-right: 1px solid var(--clerk-border, #e4e7ed);
  display: flex;
  flex-direction: column;
  padding: 12px;
}

.sidebar-header {
  margin-bottom: 12px;
}

.sidebar-title {
  font-size: 18px;
  font-weight: 600;
  color: #303133;
  margin: 0;
}

.sidebar-actions {
  margin-bottom: 4px;
}

.sidebar-sessions {
  flex: 1;
  overflow-y: auto;
}

.session-item {
  display: flex;
  align-items: center;
  gap: 8px;
  padding: 8px;
  border-radius: 4px;
  cursor: pointer;
  font-size: 13px;
  color: #606266;
  transition: background 0.2s;
}

.session-item:hover {
  background: #e4e7ed;
}

.session-name {
  overflow: hidden;
  text-overflow: ellipsis;
  white-space: nowrap;
}

.chat-main {
  flex: 1;
  display: flex;
  flex-direction: column;
  overflow: hidden;
}
</style>
