import { createApp } from 'vue'
import { createPinia } from 'pinia'
import ElementPlus from 'element-plus'
import 'element-plus/dist/index.css'
import 'element-plus/theme-chalk/dark/css-vars.css'
import App from './App.vue'
import './styles/index.css'

// 跟随系统深色模式
function applyDarkMode() {
  const prefersDark = window.matchMedia('(prefers-color-scheme: dark)').matches
  document.documentElement.classList.toggle('dark', prefersDark)
}
applyDarkMode()
window.matchMedia('(prefers-color-scheme: dark)').addEventListener('change', applyDarkMode)

const app = createApp(App)
app.use(createPinia())
app.use(ElementPlus)
app.mount('#app')
