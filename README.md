# Clerk

Clerk 是一个使用 Rust 编写的终端交互式办公 AI Agent（TUI），定位类似 Kimi Code，但专注于办公自动化场景：处理 Office 文档、制作海报、网页抓取与浏览器操作、PDF 生成、文书起草、数据分析等。

## 功能特性（规划中）

- **对话式 TUI**：基于 Ratatui 的终端聊天界面，支持多行输入、消息滚动、会话管理。
- **工具调用框架**：本地工具 + MCP（Model Context Protocol）外部工具统一注册与调用。
- **Skills 系统**：通过 SKILL.md 文件注入系统提示词、示例和工具白名单。
- **Office 文档处理**：Excel 读写（calamine / rust_xlsxwriter）、Word 读写（docx-rs）。
- **网页能力**：curl 简单抓取 + Chromium 无头浏览器复杂渲染与操作。
- **PDF / 海报**：HTML + CSS 排版后通过浏览器转 PDF/PNG。
- **文书起草**：基于 Tera 模板引擎的合同、报告、公文模板。
- **数据分析**：CSV/Excel 统计与 plotters 图表生成。

## 快速开始

```bash
# 1. 克隆仓库
git clone https://github.com/mikesolar/clerk.git
cd clerk

# 2. 配置
cp config.example.toml ~/.config/clerk/config.toml
# 编辑 config.toml，填入 LLM API key

# 3. 运行
cargo run

# 4. 命令行模式
cargo run -- -x "你好"
```

## 快捷键

- `Enter`：发送消息
- `Shift + Enter`：换行
- `Shift + ↑/↓`：滚动聊天窗口
- `Esc` / `Ctrl + C`：退出

## 配置

配置路径优先级：
1. `--config <FILE>` 命令行参数
2. `~/.config/clerk/config.toml`
3. 默认配置（未配置 API key 时仅本地功能可用）

## 开发

```bash
# 运行测试
cargo test

# 检查配置
cargo run -- --check-config
```

## 许可证

MIT
