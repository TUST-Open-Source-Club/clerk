# Clerk 开发指南

## 项目结构

本仓库是 Cargo workspace：

- `crates/clerk-core/`：核心库（agent、tools、mcp、skills、store、config、启动装配与提示词），TUI 与 GUI 共享
- `src/`：TUI 二进制（包名 `clerk`），含 `ui/`、`app.rs`、`cli.rs`、`main.rs`
- `crates/clerk-gui/`：Tauri 2.x 桌面 GUI（`src/lib.rs` 为后端命令与事件桥接，`ui/` 为纯 HTML/JS 前端）
- `skills/`：内置 Skills

## 提交规范

- 每个可独立编译、运行的阶段完成后提交一次。
- 提交信息使用中文简洁描述，例如：
  - `feat: 初始化 Cargo 工程与 TUI 骨架`
  - `feat: 实现 SQLite 会话持久化`
  - `feat: 接入 OpenAI 兼容 LLM 与工具调用框架`

## 测试要求

- 核心模块必须包含单元测试。
- 使用 `tempfile` 避免测试污染文件系统。
- 运行 `cargo test --workspace` 确保全部通过。
- 提交前确认 `cargo clippy --workspace --all-features -- -D warnings -A dead_code` 与 `cargo fmt --all -- --check` 干净。
