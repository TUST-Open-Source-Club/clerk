# Clerk 开发指南

## 项目结构

- `src/ui/`：TUI 组件
- `src/agent/`：Agent 编排与 LLM 适配
- `src/tools/`：本地工具实现
- `src/mcp/`：MCP 客户端
- `src/skills/`：Skills 系统
- `src/store/`：SQLite 持久化
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
- 运行 `cargo test` 确保全部通过。
