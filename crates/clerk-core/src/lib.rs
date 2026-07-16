//! Clerk 核心库：Agent 编排、工具、MCP、Skills、持久化与配置，
//! 供 TUI 与 GUI 两个前端共享。
//!
//! 不包含任何终端渲染/输入相关代码（TUI 专有代码保留在根包的
//! `src/app.rs` 与 `src/ui/` 中）。

pub mod agent;
pub mod bootstrap;
pub mod config;
pub mod mcp;
pub mod media;
pub mod prompt;
pub mod skills;
pub mod store;
pub mod text;
pub mod tools;
pub mod util;
