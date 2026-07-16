# Clerk

Clerk 是一个使用 Rust 编写的终端交互式办公 AI Agent（TUI），采用 Plan-Execute（计划-执行）模式工作：先为用户的请求制定执行计划，再逐步调用工具执行，最后总结结果。它专注于办公自动化场景——Office 文档处理、PDF 合并拆分、海报制作、网页抓取与浏览器操作、文书起草等，同时支持子 Agent、多 Agent 协作、Skills 知识复用与 MCP 外部工具扩展。

## 功能特性

- **对话式 TUI**：基于 Ratatui 的终端聊天界面，支持多行输入、消息滚动、Markdown 渲染（标题、加粗、代码块、表格等）。
- **Plan-Execute 模式**：先规划后执行，步骤失败时自动重计划，支持流式输出与模型推理内容（reasoning content / `<think>` 标签）展示。
- **丰富的本地工具**：
  - Office：Excel/Word 读写、Pandoc 渲染复杂 Word/PDF/PPT
  - PDF：合并、拆分、转图片
  - 海报：HTML 渲染为 PDF/PNG 海报
  - 浏览器：无头 Chromium 网页操作、截图、生成 PDF
  - 系统：shell 命令执行、文件读写、目录遍历
  - 网络：HTTP GET 抓取（可转 Markdown）、POST 请求
  - 媒体：读取图片/视频并返回 base64 数据 URL（支持大图自动压缩、视频首帧提取）
  - 渲染：将 HTML/PDF/Office/图片渲染为 PNG 预览图
- **子 Agent（Subagent）**：创建拥有独立会话与裁剪工具集的轻量子 Agent，委派子任务执行。
- **多 Agent 协作**：`collaborate_parallel` 并行派发多个子任务并汇总结果；`collaborate_sequential` 顺序执行、后者可读取前者输出。
- **Skills 系统**：通过 SKILL.md 注入领域知识（系统提示词、示例、工具白名单），支持内置/用户/项目三级加载与 `write_skill` 工具沉淀复用。
- **MCP 客户端**：支持 stdio/SSE 传输，可接入 Model Context Protocol 外部工具服务。
- **权限与审批**：YOLO 模式全自动批准，或按 `auto_approve` 白名单自动批准、其余工具执行前逐个由用户确认（y/n）。
- **斜杠命令**：会话内快捷操作（见下文），支持 Tab 补全与命令提示。
- **多模态配置**：可声明模型是否支持图片/视频输入，支持 `/attach` 附件与粘贴媒体路径直接发送。
- **首次运行向导**：未检测到配置时交互式引导填写 API 地址、模型、Key、多模态能力等并保存配置。

## 安装与构建

需要 Rust 工具链（edition 2024）。部分工具依赖外部命令，按需安装：Chromium/Chrome（browser、poster、render_to_image）、Pandoc（office_render）、pdftk/qpdf/pypdf（pdf 工具）、pdftoppm/mutool（PDF 转图）、LibreOffice（Office 转图）、ffmpeg（视频信息提取）。

```bash
# 克隆仓库
git clone https://github.com/TUST-Open-Source-Club/clerk.git
cd clerk

# 构建
cargo build --release

# 运行测试
cargo test

# 直接运行（debug）
cargo run
```

## 配置

配置文件优先级：`--config <FILE>` 命令行参数 > `~/.config/clerk/config.toml`。首次启动（或 `clerk --setup`）会进入配置向导。

参考 [config.example.toml](config.example.toml)，最小配置示例：

```toml
[llm]
model = "gpt-4o-mini"
base_url = "https://api.openai.com/v1"
api_key = "sk-xxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxx"
timeout_seconds = 600
temperature = 0.7

[multimodal]
supports_images = true
supports_video = true

# 工具审批：配置后，除 auto_approve 外的工具执行前需要用户确认（yolo = true 则全部自动批准）
[permissions]
yolo = false
auto_approve = ["fs_read", "fs_list", "web_fetch"]
```

使用 `clerk --check-config` 校验配置。

## 使用

### TUI 交互模式

```bash
clerk                # 启动交互界面（常用参数：-c 指定配置、-w 指定工作目录）
```

快捷键：

- `Enter`：发送消息
- `Shift + Enter`：换行
- `Shift + ↑/↓`：滚动聊天窗口
- `Tab`：补全斜杠命令
- 审批提示时按 `y` / `n`：批准 / 拒绝工具调用
- 生成中按 `Ctrl + C`：中断当前生成

### 非交互模式

```bash
clerk -x "把 a.xlsx 的销售额汇总并写入 summary.docx"
```

直接执行一条命令，将最终回复打印到 stdout 后退出。

## 斜杠命令

| 命令 | 说明 |
| --- | --- |
| `/help` | 显示帮助 |
| `/exit` | 退出应用 |
| `/clear` | 清空聊天与工具事件 |
| `/yolo` | 切换 YOLO 模式（工具调用免确认） |
| `/sessions` | 列出最近会话 |
| `/attach <path>` | 附加图片/视频到下一条消息 |
| `/attachments` | 列出已附加的媒体 |
| `/clear_attachments` | 清除所有附件 |

## 架构概览

- `src/ui/`：TUI 组件（聊天面板、输入区、Markdown 渲染）
- `src/agent/`：Agent 编排与 LLM 适配（Plan-Execute runner、会话上下文、子 Agent 及其管理器、OpenAI 兼容客户端）
- `src/tools/`：本地工具实现与注册表（Office/PDF/海报/浏览器/shell/fs/web/媒体/渲染/协作/skill）
- `src/mcp/`：MCP 客户端（JSON-RPC 类型、stdio/SSE 传输）
- `src/skills/`：Skills 系统（SKILL.md 解析、加载、相关性注入、写入）
- `src/store/`：SQLite 会话与消息持久化
- `src/app.rs`：TUI 主事件循环（键盘处理、流式输出、审批交互）
- `src/main.rs`：启动入口（配置加载/向导、工具注册、交互与非交互模式分发）

## 许可证

[GPLv3](LICENSE)
