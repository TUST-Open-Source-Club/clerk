use std::collections::HashMap;

use serde_json::Value;

use crate::skills::writer::SkillWriter;
use crate::tools::schema::{Tool, ToolContext, ToolResult, ToolSchema, get_string};

/// `write_skill` 工具：把 SKILL.md 内容写入项目 skills 目录以便复用。
pub struct WriteSkillTool;

impl WriteSkillTool {
    pub fn new() -> Self {
        Self
    }
}

#[async_trait::async_trait]
impl Tool for WriteSkillTool {
    fn name(&self) -> &str {
        "write_skill"
    }

    fn description(&self) -> &str {
        "将一段 SKILL.md 内容写入项目的 skills 目录，供后续会话加载复用。"
    }

    fn schema(&self) -> ToolSchema {
        ToolSchema::new("write_skill", self.description())
            .with_string("name", "Skill 名称（决定目录名）", true)
            .with_string("content", "SKILL.md 完整内容", true)
    }

    async fn execute(
        &self,
        args: HashMap<String, Value>,
        ctx: &ToolContext,
    ) -> anyhow::Result<ToolResult> {
        let name = get_string(&args, "name")?;
        let content = get_string(&args, "content")?;

        let path = SkillWriter::write(&ctx.working_dir, &name, &content)?;
        Ok(ToolResult::Text(format!(
            "Skill 已保存: {}",
            path.display()
        )))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[tokio::test]
    async fn test_write_skill_tool() {
        let dir = TempDir::new().unwrap();
        let ctx = ToolContext {
            working_dir: dir.path().to_path_buf(),
            ..Default::default()
        };
        let tool = WriteSkillTool::new();
        let mut args = HashMap::new();
        args.insert(
            "name".to_string(),
            Value::String("poster-design".to_string()),
        );
        args.insert(
            "content".to_string(),
            Value::String("---\nname: poster\n---\n# Poster\n".to_string()),
        );

        let result = tool.execute(args, &ctx).await.unwrap();
        let text = result.to_string_for_model();
        assert!(text.contains("Skill 已保存"));
        assert!(text.contains("skills"));
        assert!(text.contains("poster-design"));
    }

    #[tokio::test]
    async fn test_write_skill_missing_args() {
        let ctx = ToolContext {
            working_dir: std::env::temp_dir(),
            ..Default::default()
        };
        let tool = WriteSkillTool::new();
        let result = tool.execute(HashMap::new(), &ctx).await;
        assert!(result.is_err());
    }
}
