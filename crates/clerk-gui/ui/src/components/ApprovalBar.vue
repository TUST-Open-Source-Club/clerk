<template>
  <div v-if="store.pendingApproval" class="approval-bar">
    <el-alert
      :title="`工具审批: ${store.pendingApproval.name}`"
      type="warning"
      :closable="false"
      show-icon
    >
      <template #default>
        <div class="approval-content">
          <el-collapse class="approval-args-collapse">
            <el-collapse-item title="查看参数" name="args">
              <pre class="approval-args">{{ formattedArgs }}</pre>
            </el-collapse-item>
          </el-collapse>
          <div class="approval-actions">
            <el-button type="success" size="small" @click="handleAllow">
              允许
            </el-button>
            <el-button type="danger" size="small" @click="handleDeny">
              拒绝
            </el-button>
          </div>
        </div>
      </template>
    </el-alert>
  </div>
</template>

<script setup lang="ts">
import { computed } from 'vue'
import { ElMessage } from 'element-plus'
import { useChatStore } from '../stores/chat'
import * as api from '../api'

const store = useChatStore()

const formattedArgs = computed(() => {
  if (!store.pendingApproval) return ''
  try {
    return JSON.stringify(store.pendingApproval.arguments, null, 2)
  } catch {
    return String(store.pendingApproval.arguments)
  }
})

async function handleAllow() {
  try {
    await api.respondApproval(true)
    store.clearPendingApproval()
  } catch (err) {
    ElMessage.error(`审批响应失败: ${err}`)
  }
}

async function handleDeny() {
  try {
    await api.respondApproval(false)
    store.clearPendingApproval()
  } catch (err) {
    ElMessage.error(`审批响应失败: ${err}`)
  }
}
</script>

<style scoped>
.approval-bar {
  padding: 8px 16px;
  border-bottom: 1px solid var(--clerk-border, #e4e7ed);
}

.approval-content {
  margin-top: 8px;
}

.approval-args-collapse {
  margin-bottom: 8px;
}

.approval-args {
  font-size: 12px;
  background: #f5f7fa;
  padding: 8px;
  border-radius: 4px;
  max-height: 150px;
  overflow-y: auto;
  white-space: pre-wrap;
  word-break: break-all;
}

.approval-actions {
  display: flex;
  gap: 8px;
  justify-content: flex-end;
}
</style>
