//! 工具事件与参数的展示格式化，TUI 与 GUI 共用。

use serde_json::Value;

use crate::agent::runner::RunnerEvent;

/// 将 runner 工具事件格式化为聊天面板展示文本。
pub fn format_tool_event(event: &RunnerEvent) -> String {
    match event {
        RunnerEvent::Plan { steps } => {
            let list = steps
                .iter()
                .enumerate()
                .map(|(i, s)| format!("{}. {}", i + 1, s))
                .collect::<Vec<_>>()
                .join("\n");
            format!("执行计划：\n{}", list)
        }
        RunnerEvent::ToolCall { name, arguments } => {
            let args = format_tool_arguments(name, arguments);
            format!("调用工具 {}: {}", name, args)
        }
        RunnerEvent::ToolResult { name, result } => {
            let summary = result.chars().take(200).collect::<String>();
            let ellipsis = if result.chars().count() > 200 {
                " ..."
            } else {
                ""
            };
            format!("工具 {} 结果: {}{}", name, summary, ellipsis)
        }
        RunnerEvent::Error(e) => format!("工具错误: {}", e),
    }
}

/// 将工具参数 JSON 格式化为 `k=v` 列表，供审批与事件展示。
pub fn format_tool_arguments(name: &str, arguments: &Value) -> String {
    match arguments.as_object() {
        Some(map) => {
            let parts: Vec<String> = map
                .iter()
                .map(|(k, v)| format!("{}={}", k, format_arg_value(name, k, v)))
                .collect();
            if parts.is_empty() {
                "(无参数)".to_string()
            } else {
                parts.join(", ")
            }
        }
        None => arguments.to_string(),
    }
}

/// 格式化单个参数值；shell 命令、文件内容等长字段超过 120 字符时截断展示。
fn format_arg_value(tool_name: &str, key: &str, value: &serde_json::Value) -> String {
    // shell 命令、文件内容等长字段需要截断
    let is_long_field = matches!(
        (tool_name, key),
        ("shell", "command")
            | ("fs_write", "content")
            | ("web_fetch", "url")
            | ("web_post", "url")
            | ("browser", "url")
            | ("poster", "input")
    );

    let s = value.to_string();
    if is_long_field && s.len() > 120 {
        format!("{}...（共 {} 字符）", &s[..120], s.len())
    } else {
        s
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_format_tool_event_shows_details() {
        let event = RunnerEvent::ToolCall {
            name: "fs_write".to_string(),
            arguments: serde_json::json!({
                "path": "/tmp/foo.html",
                "content": "hello world"
            }),
        };
        let text = format_tool_event(&event);
        assert!(text.contains("fs_write"));
        assert!(text.contains("/tmp/foo.html"));

        let event = RunnerEvent::ToolCall {
            name: "shell".to_string(),
            arguments: serde_json::json!({"command": "ls -la"}),
        };
        let text = format_tool_event(&event);
        assert!(text.contains("shell"));
        assert!(text.contains("ls -la"));
    }

    #[test]
    fn test_format_plan_event_shows_steps() {
        let event = RunnerEvent::Plan {
            steps: vec!["读取文件".to_string(), "总结内容".to_string()],
        };
        let text = format_tool_event(&event);
        assert!(text.contains("执行计划"));
        assert!(text.contains("1. 读取文件"));
        assert!(text.contains("2. 总结内容"));
    }
}
