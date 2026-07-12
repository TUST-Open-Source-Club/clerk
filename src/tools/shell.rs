use anyhow::{Context, Result};
use async_trait::async_trait;
use serde_json::Value;
use std::collections::HashMap;
use tokio::process::Command;

use crate::tools::schema::{Tool, ToolContext, ToolResult, ToolSchema, get_string};

pub struct ShellTool;

#[async_trait]
impl Tool for ShellTool {
    fn name(&self) -> &str {
        "shell"
    }

    fn description(&self) -> &str {
        "在工作目录下执行 shell 命令。仅用于用户明确授权的场景，请谨慎使用。"
    }

    fn schema(&self) -> ToolSchema {
        ToolSchema::new("shell", "执行 shell 命令")
            .with_string("command", "要执行的 shell 命令", true)
            .with_string("cwd", "工作目录，默认为当前工作目录", false)
    }

    async fn execute(&self, args: HashMap<String, Value>, ctx: &ToolContext) -> Result<ToolResult> {
        let command_str = get_string(&args, "command")?;
        let cwd =
            get_string(&args, "cwd").unwrap_or_else(|_| ctx.working_dir.display().to_string());

        let output = Command::new("sh")
            .arg("-c")
            .arg(&command_str)
            .current_dir(&cwd)
            .output()
            .await
            .with_context(|| format!("执行命令失败: {}", command_str))?;

        let stdout = String::from_utf8_lossy(&output.stdout).to_string();
        let stderr = String::from_utf8_lossy(&output.stderr).to_string();

        if !output.status.success() {
            return Ok(ToolResult::Error(format!(
                "命令退出码 {}\nstdout:\n{}\nstderr:\n{}",
                output.status.code().unwrap_or(-1),
                stdout,
                stderr
            )));
        }

        let result = if stderr.is_empty() {
            stdout
        } else {
            format!("{}\nstderr:\n{}", stdout, stderr)
        };

        Ok(ToolResult::Text(result))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[tokio::test]
    async fn test_shell_echo() {
        let dir = TempDir::new().unwrap();
        let tool = ShellTool;
        let mut args = HashMap::new();
        args.insert(
            "command".to_string(),
            Value::String("echo hello".to_string()),
        );
        args.insert(
            "cwd".to_string(),
            Value::String(dir.path().display().to_string()),
        );

        let ctx = ToolContext {
            working_dir: dir.path().to_path_buf(),
        };
        let result = tool.execute(args, &ctx).await.unwrap();
        assert!(result.to_string_for_model().contains("hello"));
    }

    #[tokio::test]
    async fn test_shell_failure() {
        let dir = TempDir::new().unwrap();
        let tool = ShellTool;
        let mut args = HashMap::new();
        args.insert("command".to_string(), Value::String("exit 42".to_string()));

        let ctx = ToolContext {
            working_dir: dir.path().to_path_buf(),
        };
        let result = tool.execute(args, &ctx).await.unwrap();
        match result {
            ToolResult::Error(e) => assert!(e.contains("42")),
            _ => panic!("期望错误结果"),
        }
    }
}
