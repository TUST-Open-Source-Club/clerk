import { defineStore } from 'pinia'
import { ref } from 'vue'

export const useSessionStore = defineStore('session', () => {
  const sessionId = ref('')
  const model = ref('loading...')
  const workingDir = ref('')
  const yoloMode = ref(false)
  const startTime = ref<number | null>(null)

  function setSessionMeta(data: {
    sessionId?: string
    model?: string
    workingDir?: string
    yoloMode?: boolean
  }) {
    if (data.sessionId !== undefined) sessionId.value = data.sessionId
    if (data.model !== undefined) model.value = data.model
    if (data.workingDir !== undefined) workingDir.value = data.workingDir
    if (data.yoloMode !== undefined) yoloMode.value = data.yoloMode
  }

  function resetSession() {
    sessionId.value = ''
    model.value = 'loading...'
    workingDir.value = ''
    yoloMode.value = false
    startTime.value = null
  }

  return {
    sessionId,
    model,
    workingDir,
    yoloMode,
    startTime,
    setSessionMeta,
    resetSession,
  }
})
