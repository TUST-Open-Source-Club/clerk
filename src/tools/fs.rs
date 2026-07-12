use anyhow::{Context, Result};
use async_trait::async_trait;
use serde_json::{Value, json};
use std::collections::HashMap;
use std::fs;
use std::path::PathBuf;

use crate::tools::schema::{
    Tool, ToolContext, ToolResult, ToolSchema, get_bool, get_i64, get_string,
};

pub struct ReadFileTool;

#[async_trait]
impl Tool for ReadFileTool {
    fn name(&self) -> &str {
        "fs_read"
    }

    fn description(&self) -> &str {
        "读取指定路径的文本文件内容。如果文件过大，可设置 limit 限制返回行数。"
    }

    fn schema(&self) -> ToolSchema {
        ToolSchema::new("fs_read", "读取文本文件内容")
            .with_string("path", "相对于工作目录的文件路径", true)
            .with_integer("limit", "最大返回行数，0 表示不限制", false)
    }

    async fn execute(&self, args: HashMap<String, Value>, ctx: &ToolContext) -> Result<ToolResult> {
        let path_str = get_string(&args, "path")?;
        let limit = get_i64(&args, "limit", 0);
        let path = resolve_path(&ctx.working_dir, &path_str)?;

        let content = fs::read_to_string(&path)
            .with_context(|| format!("读取文件失败: {}", path.display()))?;

        let output = if limit > 0 {
            content
                .lines()
                .take(limit as usize)
                .collect::<Vec<_>>()
                .join("\n")
        } else {
            content
        };

        Ok(ToolResult::Text(output))
    }
}

pub struct WriteFileTool;

#[async_trait]
impl Tool for WriteFileTool {
    fn name(&self) -> &str {
        "fs_write"
    }

    fn description(&self) -> &str {
        "将内容写入指定路径的文本文件。如果文件已存在会被覆盖。"
    }

    fn schema(&self) -> ToolSchema {
        ToolSchema::new("fs_write", "写入文本文件")
            .with_string("path", "相对于工作目录的文件路径", true)
            .with_string("content", "文件内容", true)
    }

    async fn execute(&self, args: HashMap<String, Value>, ctx: &ToolContext) -> Result<ToolResult> {
        let path_str = get_string(&args, "path")?;
        let content = get_string(&args, "content")?;
        let path = resolve_path(&ctx.working_dir, &path_str)?;

        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)
                .with_context(|| format!("创建目录失败: {}", parent.display()))?;
        }

        fs::write(&path, content).with_context(|| format!("写入文件失败: {}", path.display()))?;

        Ok(ToolResult::Text(format!("已写入: {}", path.display())))
    }
}

pub struct ListDirTool;

#[async_trait]
impl Tool for ListDirTool {
    fn name(&self) -> &str {
        "fs_list"
    }

    fn description(&self) -> &str {
        "列出指定目录下的文件和子目录。"
    }

    fn schema(&self) -> ToolSchema {
        ToolSchema::new("fs_list", "列出目录内容")
            .with_string("path", "相对于工作目录的目录路径，默认为工作目录", false)
            .with_boolean("recursive", "是否递归列出", false)
    }

    async fn execute(&self, args: HashMap<String, Value>, ctx: &ToolContext) -> Result<ToolResult> {
        let path_str = get_string(&args, "path").unwrap_or_else(|_| ".".to_string());
        let recursive = get_bool(&args, "recursive", false);
        let path = resolve_path(&ctx.working_dir, &path_str)?;

        let entries = if recursive {
            list_recursive(&path, &path)?
        } else {
            list_dir(&path)?
        };

        Ok(ToolResult::Json(json!(entries)))
    }
}

fn resolve_path(working_dir: &std::path::Path, input: &str) -> Result<PathBuf> {
    let path = PathBuf::from(input);
    let resolved = if path.is_absolute() {
        path
    } else {
        working_dir.join(path)
    };
    Ok(resolved.canonicalize().unwrap_or(resolved))
}

fn list_dir(path: &std::path::Path) -> Result<Vec<String>> {
    let mut entries = Vec::new();
    for entry in fs::read_dir(path).with_context(|| format!("读取目录失败: {}", path.display()))?
    {
        let entry = entry?;
        let name = entry.file_name().to_string_lossy().to_string();
        let typ = if entry.file_type()?.is_dir() {
            "dir"
        } else {
            "file"
        };
        entries.push(format!("{} ({})", name, typ));
    }
    Ok(entries)
}

fn list_recursive(base: &std::path::Path, path: &std::path::Path) -> Result<Vec<String>> {
    let mut entries = Vec::new();
    for entry in fs::read_dir(path).with_context(|| format!("读取目录失败: {}", path.display()))?
    {
        let entry = entry?;
        let relative = entry
            .path()
            .strip_prefix(base)
            .unwrap_or(&entry.path())
            .display()
            .to_string();
        let typ = if entry.file_type()?.is_dir() {
            "dir"
        } else {
            "file"
        };
        entries.push(format!("{} ({})", relative, typ));
        if entry.file_type()?.is_dir() {
            entries.extend(list_recursive(base, &entry.path())?);
        }
    }
    Ok(entries)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn ctx(dir: &TempDir) -> ToolContext {
        ToolContext {
            working_dir: dir.path().to_path_buf(),
        }
    }

    #[tokio::test]
    async fn test_read_write_file() {
        let dir = TempDir::new().unwrap();
        let tool = WriteFileTool;
        let mut args = HashMap::new();
        args.insert("path".to_string(), Value::String("test.txt".to_string()));
        args.insert(
            "content".to_string(),
            Value::String("hello world".to_string()),
        );
        let result = tool.execute(args, &ctx(&dir)).await.unwrap();
        assert!(result.to_string_for_model().contains("已写入"));

        let read_tool = ReadFileTool;
        let mut args = HashMap::new();
        args.insert("path".to_string(), Value::String("test.txt".to_string()));
        let result = read_tool.execute(args, &ctx(&dir)).await.unwrap();
        assert_eq!(result.to_string_for_model(), "hello world");
    }

    #[tokio::test]
    async fn test_list_dir() {
        let dir = TempDir::new().unwrap();
        fs::write(dir.path().join("a.txt"), "a").unwrap();
        fs::create_dir(dir.path().join("sub")).unwrap();

        let tool = ListDirTool;
        let mut args = HashMap::new();
        args.insert("path".to_string(), Value::String(".".to_string()));
        let result = tool.execute(args, &ctx(&dir)).await.unwrap();
        let text = result.to_string_for_model();
        assert!(text.contains("a.txt"));
        assert!(text.contains("sub (dir)"));
    }
}
