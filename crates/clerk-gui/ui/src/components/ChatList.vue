<template>
  <div ref="listRef" class="message-list">
    <ChatMessage
      v-for="(msg, idx) in chatStore.displayMessages"
      :key="idx"
      :msg="msg"
    />
  </div>
</template>

<script setup lang="ts">
import { ref, watch, nextTick } from 'vue'
import { useChatStore } from '../stores/chat'
import ChatMessage from './ChatMessage.vue'

const chatStore = useChatStore()
const listRef = ref<HTMLDivElement>()

// 自动滚动到底部
watch(
  () => chatStore.displayMessages.length,
  async () => {
    await nextTick()
    scrollToBottom()
  },
)

watch(
  () => chatStore.streamingReply,
  async () => {
    await nextTick()
    scrollToBottom()
  },
)

function scrollToBottom() {
  if (listRef.value) {
    listRef.value.scrollTop = listRef.value.scrollHeight
  }
}
</script>

<style scoped>
.message-list {
  flex: 1;
  overflow-y: auto;
  padding: 16px;
}
</style>
