# Clerk Tauri 前端重写计划（Vue 3 + Element Plus）

## 一、目标

将 `crates/clerk-gui` 当前的纯 HTML/JS/CSS 前端重写为 Vue 3 + Element Plus 单页应用，保持与现有 Rust 后端（Tauri Commands + Events）完全兼容，实现更现代、可维护的桌面聊天界面。

**本计划已经把技术决策全部做完。执行者（DeepSeek V4 Flash）只需按步骤实现，不要再做技术选型。**

## 二、最终技术栈（禁止更改）

- **框架**: Vue 3（`<script setup>` + Composition API）
- **UI 组件库**: Element Plus（全量引入）
- **构建工具**: Vite 5
- **语言**: TypeScript（strict 模式）
- **状态管理**: Pinia
- **CSS**: Element Plus 默认主题 + 少量自定义 CSS 变量
- **包管理**: pnpm
- **Node 版本**: >= 20

**禁用**: React、Angular、Svelte、Tailwind CSS、UnoCSS、Naive UI、Ant Design Vue、Vuex、JavaScript（必须用 TS）。

## 三、目录结构

```
crates/clerk-gui/
├── src/                     # Rust 后端（不动）
├── ui/                      # 前端源码（本次重写）
│   ├── src/
│   │   ├── main.ts
│   │   ├── App.vue
│   │   ├── types.ts          # 与后端对应的 TS 类型
│   │   ├── api.ts            # Tauri invoke / event 封装
│   │   ├── stores/
│   │   │   ├── chat.ts       # 消息、流式状态、附件
│   │   │   └── session.ts    # 会话、配置
│   │   ├── components/
│   │   │   ├── ChatMessage.vue
│   │   │   ├── ChatInput.vue
│   │   │   ├── ChatList.vue
│   │   │   ├── FileChip.vue
│   │   │   ├── ApprovalBar.vue
│   │   │   └── StatusBar.vue
│   │   └── styles/
│   │       └── index.css
│   ├── index.html
│   ├── package.json
│   ├── tsconfig.json
│   ├── tsconfig.node.json
│   └── vite.config.ts
└── tauri.conf.json
```

## 四、后端接口契约（必须原样使用，禁止修改后端）

### 4.1 Tauri Commands

```ts
// 发送消息
invoke('send_message', { text: string }): Promise<void>
// 获取历史消息
invoke('get_history'): Promise<Message[]>
// 添加媒体附件（base64）
invoke('attach_media', { name: string, data: string }): Promise<string>
// 保存文件到工作目录
invoke('save_file', { path: string }): Promise<string>
// 另存为
invoke('save_file_as', { path: string }): Promise<string>
// 审批响应
invoke('respond_approval', { allow: boolean }): Promise<void>
```

### 4.2 Tauri Events

```ts
// 流式输出块
listen<ChunkPayload>('clerk-chunk', (event) => void)
// ChunkPayload = { content?: string; reasoning?: string }

// 工具事件
listen<ToolEventPayload>('clerk-tool', (event) => void)
// ToolEventPayload = { text: string }

// 审批请求
listen<ApprovalPayload>('clerk-approval', (event) => void)
// ApprovalPayload = { name: string; arguments: any }

// 生成文件通知
listen<FilePayload>('clerk-file', (event) => void)
// FilePayload = { path: string; name: string }

// 完成
listen<DonePayload>('clerk-done', (event) => void)
// DonePayload = { reply: string; ok: boolean }
```

### 4.3 核心类型（`src/types.ts`）

```ts
export interface Message {
  id: number
  session_id: string
  role: 'user' | 'assistant' | 'system' | 'tool'
  content: string
  created_at: string
}

export interface ChunkPayload {
  content?: string
  reasoning?: string
}

export interface ToolEventPayload {
  text: string
}

export interface ApprovalPayload {
  name: string
  arguments: Record<string, unknown>
}

export interface FilePayload {
  path: string
  name: string
}

export interface DonePayload {
  reply: string
  ok: boolean
}
```

## 五、功能需求

### 5.1 聊天界面

- 消息气泡区分 user / assistant / system / tool
- assistant 消息支持 Markdown 渲染（用 `markdown-it` + `highlight.js`）
- assistant 流式输出时显示打字机效果
- reasoning_content 显示为可折叠的灰色“思考过程”块
- tool 消息显示为黄色（调用）、绿色（结果）、红色（错误）
- 支持图片/视频粘贴：监听 paste 事件，读取 `e.clipboardData.files`，转 base64 后调用 `attach_media`
- 输入框支持 Shift+Enter 换行，Enter 发送

### 5.2 文件卡片

- 收到 `clerk-file` 事件时，在对应 assistant 消息下方渲染 `FileChip`
- `FileChip` 显示文件名、扩展名图标、两个按钮：「保存」「另存为」
- 「保存」调用 `save_file`，「另存为」调用 `save_file_as`
- 成功后显示 Element Plus `ElMessage.success`

### 5.3 审批模式

- 收到 `clerk-approval` 时，在聊天区顶部显示 `ApprovalBar`
- 显示工具名和参数 JSON（可折叠）
- 用户点「允许」→ `respond_approval({ allow: true })`
- 用户点「拒绝」→ `respond_approval({ allow: false })`

### 5.4 状态栏

- 底部状态栏显示：当前模型、工作目录、会话 ID（短 ID）、是否 YOLO 模式、流式状态（含 spinner + 已耗时秒数）

### 5.5 会话管理

- 左侧侧边栏列出会话（从 `get_history` 获取当前会话消息；会话列表后端暂无 command，可先用 `send_message('/sessions')` 模拟，或留 TODO）
- 支持新建会话、切换会话（切换后调用 `get_history` 刷新消息）

### 5.6 样式

- 深色/浅色主题跟随系统，可用 Element Plus 的 `dark` CSS 变量切换
- 整体风格简洁、现代，类似 Kimi Code / Claude Code 桌面版

## 六、实现步骤

### Step 1: 初始化 Vite + Vue 3 项目

在 `crates/clerk-gui/ui` 下：

```bash
cd crates/clerk-gui/ui
pnpm create vite . --template vue-ts
pnpm install
pnpm add element-plus pinia @tauri-apps/api markdown-it highlight.js
pnpm add -D @types/markdown-it
```

### Step 2: 配置 Tauri 与 Vite

`vite.config.ts`:

```ts
import { defineConfig } from 'vite'
import vue from '@vitejs/plugin-vue'

export default defineConfig({
  plugins: [vue()],
  clearScreen: false,
  server: {
    strictPort: true,
  },
  build: {
    target: ['es2021', 'chrome100', 'safari13'],
    outDir: 'dist',
    emptyOutDir: true,
  },
})
```

`tauri.conf.json` 中 `build.devPath` 改为 `http://localhost:5173`，`build.distDir` 改为 `ui/dist`。

### Step 3: 写 `src/api.ts`

封装所有 `invoke` 和 `listen`，导出类型安全的函数。事件监听返回 `unlisten` 函数。

### Step 4: 写 `src/types.ts`

照抄本文档 4.3 节的类型。

### Step 5: 写 Pinia stores

`chat.ts` 管理：
- `messages: Message[]`
- `streamingReply: string`
- `streamingReasoning: string`
- `isStreaming: boolean`
- `attachments: string[]`
- `pendingApproval: ApprovalPayload | null`

`session.ts` 管理：
- `sessionId: string`
- `model: string`
- `workingDir: string`

### Step 6: 写组件

- `App.vue`: 布局（侧边栏 + 主聊天区）
- `ChatList.vue`: 渲染消息列表，自动滚动到底部
- `ChatMessage.vue`: 单条消息（角色、Markdown、思考过程折叠、文件卡片）
- `ChatInput.vue`: 输入框、粘贴处理、发送按钮
- `FileChip.vue`: 文件卡片
- `ApprovalBar.vue`: 审批条
- `StatusBar.vue`: 状态栏

### Step 7: 样式

在 `src/styles/index.css` 中引入 Element Plus 样式和少量自定义变量：

```css
:root {
  --clerk-bg: #ffffff;
  --clerk-sidebar: #f5f7fa;
  --clerk-border: #e4e7ed;
}

.dark {
  --clerk-bg: #1f1f1f;
  --clerk-sidebar: #2a2a2a;
  --clerk-border: #444;
}
```

### Step 8: 构建与集成

```bash
cd crates/clerk-gui/ui
pnpm build
cd ../..
cargo build --release --package clerk-gui
```

## 七、禁止事项

1. **禁止修改 Rust 后端代码**（`crates/clerk-gui/src/**/*.rs` 和 `crates/clerk-core/**`）。
2. **禁止使用 Tailwind CSS / UnoCSS**，必须用 Element Plus。
3. **禁止不使用 TypeScript**。
4. **禁止改变事件/命令名称和 payload 结构**。
5. **禁止引入重型状态库（Vuex）或 UI 框架（Naive UI、Ant Design Vue）**。
6. **禁止把 `send_message` 改为 REST 请求**，必须用 Tauri Commands。
7. **禁止省略附件粘贴、文件卡片、审批条、流式 reasoning 显示**。
8. **禁止在组件里直接写 `window.__TAURI__`**，必须通过 `src/api.ts` 封装。
9. **禁止用 `any`**，除 `ApprovalPayload.arguments` 这种确实动态的结构可用 `Record<string, unknown>`。
10. **禁止提交未格式化的代码**，完成后必须 `pnpm exec prettier --write src/`（如果配置了 prettier）或至少 `pnpm exec eslint --fix src/`。

## 八、验收标准

- [ ] `cargo build --release --package clerk-gui` 成功
- [ ] GUI 能正常启动并显示聊天窗口
- [ ] 发送消息后能看到流式回复和思考过程
- [ ] 能粘贴图片并作为附件发送
- [ ] 生成文件时显示文件卡片，能点保存/另存为
- [ ] 审批模式能显示审批条并响应允许/拒绝
- [ ] 状态栏显示模型、目录、会话、YOLO 状态
- [ ] 代码通过 TypeScript strict 编译，无 `any`（除明确允许的动态结构）

## 九、给 DeepSeek V4 Flash 的提示

- 你不需要做技术选型，上面已经全部决定。
- 你不需要设计接口，接口契约在第四节。
- 你不需要决定文件放哪，目录结构在第三节。
- 你只需要按第六节 Step 1 → Step 8 顺序实现。
- 如果遇到不确定，回到本计划文档查找，不要自己发明。
