<template>
  <el-card class="file-chip" shadow="never" :body-style="{ padding: '8px 12px' }">
    <div class="file-chip-inner">
      <el-icon class="file-icon"><Document /></el-icon>
      <span class="file-name">{{ file.name }}</span>
      <div class="file-actions">
        <el-button size="small" type="primary" link @click="handleSave">
          保存
        </el-button>
        <el-button size="small" type="primary" link @click="handleSaveAs">
          另存为
        </el-button>
      </div>
    </div>
  </el-card>
</template>

<script setup lang="ts">
import { Document } from '@element-plus/icons-vue'
import { ElMessage } from 'element-plus'
import type { FilePayload } from '../types'
import * as api from '../api'

const props = defineProps<{
  file: FilePayload
}>()

async function handleSave() {
  try {
    const result = await api.saveFile(props.file.path)
    ElMessage.success(`已保存: ${result}`)
  } catch (err) {
    ElMessage.error(`保存失败: ${err}`)
  }
}

async function handleSaveAs() {
  try {
    const result = await api.saveFileAs(props.file.path)
    if (result) {
      ElMessage.success(`已保存为: ${result}`)
    }
    // 用户取消对话框，result 为 null，不做操作
  } catch (err) {
    ElMessage.error(`另存为失败: ${err}`)
  }
}
</script>

<style scoped>
.file-chip {
  width: 280px;
  border: 1px solid var(--clerk-border, #e4e7ed);
}

.file-chip-inner {
  display: flex;
  align-items: center;
  gap: 8px;
}

.file-icon {
  font-size: 20px;
  color: #409eff;
}

.file-name {
  flex: 1;
  font-size: 13px;
  color: #303133;
  overflow: hidden;
  text-overflow: ellipsis;
  white-space: nowrap;
}

.file-actions {
  display: flex;
  gap: 4px;
}
</style>
