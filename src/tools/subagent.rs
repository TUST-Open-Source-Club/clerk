use std::collections::HashMap;
use std::sync::Arc;

use serde_json::Value;

use crate::agent::subagent_manager::SubagentManager;
use crate::tools::schema::{
    ParameterSchema, Tool, ToolContext, ToolResult, ToolSchema, get_string,
};

pub struct SubagentCreateTool {
    manager: Arc<SubagentManager>,
}

impl SubagentCreateTool {
    pub fn new(manager: Arc<SubagentManager>) -> Self {
        Self { manager }
    }
}

#[async_trait::async_trait]
impl Tool for SubagentCreateTool {
    fn name(&self) -> &str {
        "subagent_create"
    }

    fn description(&self) -> &str {
        "创建一个子 Agent，指定名称、系统提示词与可用工具白名单，返回子 Agent ID。"
    }

    fn schema(&self) -> ToolSchema {
        ToolSchema::new("subagent_create", self.description())
            .with_string("name", "子 Agent 名称", true)
            .with_string("system_prompt", "子 Agent 的系统提示词", true)
            .with_array(
                "allowed_tools",
                ParameterSchema::string("工具名"),
                "允许使用的工具名称列表（为空表示允许全部）",
                false,
            )
    }

    async fn execute(
        &self,
        args: HashMap<String, Value>,
        _ctx: &ToolContext,
    ) -> anyhow::Result<ToolResult> {
        let name = get_string(&args, "name")?;
        let system_prompt = get_string(&args, "system_prompt")?;
        let allowed_tools = args
            .get("allowed_tools")
            .and_then(|v| v.as_array())
            .map(|arr| {
                arr.iter()
                    .filter_map(|v| v.as_str().map(|s| s.to_string()))
                    .collect()
            })
            .unwrap_or_default();

        let id = self
            .manager
            .create(name, system_prompt, allowed_tools)
            .await?;
        Ok(ToolResult::Text(id))
    }
}

pub struct SubagentRunTool {
    manager: Arc<SubagentManager>,
}

impl SubagentRunTool {
    pub fn new(manager: Arc<SubagentManager>) -> Self {
        Self { manager }
    }
}

#[async_trait::async_trait]
impl Tool for SubagentRunTool {
    fn name(&self) -> &str {
        "subagent_run"
    }

    fn description(&self) -> &str {
        "运行一个已创建的子 Agent，传入任务描述，返回执行结果。"
    }

    fn schema(&self) -> ToolSchema {
        ToolSchema::new("subagent_run", self.description())
            .with_string("id", "子 Agent ID", true)
            .with_string("task", "任务描述", true)
            .with_integer("max_iterations", "最大迭代次数（默认 10）", false)
    }

    async fn execute(
        &self,
        args: HashMap<String, Value>,
        _ctx: &ToolContext,
    ) -> anyhow::Result<ToolResult> {
        let id = get_string(&args, "id")?;
        let task = get_string(&args, "task")?;
        let max_iterations = crate::tools::schema::get_i64(&args, "max_iterations", 10) as usize;

        let result = self.manager.run(&id, &task, max_iterations).await?;
        Ok(ToolResult::Json(serde_json::json!({
            "output": result.output,
            "tool_calls": result.tool_calls.len(),
        })))
    }
}

pub struct SubagentListTool {
    manager: Arc<SubagentManager>,
}

impl SubagentListTool {
    pub fn new(manager: Arc<SubagentManager>) -> Self {
        Self { manager }
    }
}

#[async_trait::async_trait]
impl Tool for SubagentListTool {
    fn name(&self) -> &str {
        "subagent_list"
    }

    fn description(&self) -> &str {
        "列出所有已创建的子 Agent。"
    }

    fn schema(&self) -> ToolSchema {
        ToolSchema::new("subagent_list", self.description())
    }

    async fn execute(
        &self,
        _args: HashMap<String, Value>,
        _ctx: &ToolContext,
    ) -> anyhow::Result<ToolResult> {
        let list = self.manager.list().await;
        let items: Vec<Value> = list
            .into_iter()
            .map(|info| {
                serde_json::json!({
                    "id": info.id,
                    "name": info.name,
                    "allowed_tools": info.allowed_tools,
                })
            })
            .collect();
        Ok(ToolResult::Json(Value::Array(items)))
    }
}

pub struct SubagentDeleteTool {
    manager: Arc<SubagentManager>,
}

impl SubagentDeleteTool {
    pub fn new(manager: Arc<SubagentManager>) -> Self {
        Self { manager }
    }
}

#[async_trait::async_trait]
impl Tool for SubagentDeleteTool {
    fn name(&self) -> &str {
        "subagent_delete"
    }

    fn description(&self) -> &str {
        "删除一个已创建的子 Agent。"
    }

    fn schema(&self) -> ToolSchema {
        ToolSchema::new("subagent_delete", self.description()).with_string(
            "id",
            "子 Agent ID",
            true,
        )
    }

    async fn execute(
        &self,
        args: HashMap<String, Value>,
        _ctx: &ToolContext,
    ) -> anyhow::Result<ToolResult> {
        let id = get_string(&args, "id")?;
        self.manager.delete(&id).await?;
        Ok(ToolResult::Text("已删除".to_string()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent::llm::{LlmResponse, Message, ToolDefinition};
    use crate::tools::schema::{Tool, ToolContext, ToolResult, ToolSchema};
    use async_trait::async_trait;
    use serde_json::Value;

    struct FakeLlm {
        responses: tokio::sync::Mutex<Vec<LlmResponse>>,
    }

    #[async_trait]
    impl crate::agent::llm::LlmClient for FakeLlm {
        async fn chat(
            &self,
            _messages: Vec<Message>,
            _tools: Vec<ToolDefinition>,
        ) -> anyhow::Result<LlmResponse> {
            let mut responses = self.responses.lock().await;
            Ok(responses.remove(0))
        }
    }

    struct FakeTool;

    #[async_trait]
    impl Tool for FakeTool {
        fn name(&self) -> &str {
            "fake"
        }
        fn description(&self) -> &str {
            "fake"
        }
        fn schema(&self) -> ToolSchema {
            ToolSchema::new("fake", "fake")
        }
        async fn execute(
            &self,
            _args: HashMap<String, Value>,
            _ctx: &ToolContext,
        ) -> anyhow::Result<ToolResult> {
            Ok(ToolResult::Text("done".to_string()))
        }
    }

    fn make_manager() -> Arc<SubagentManager> {
        let client: Arc<dyn crate::agent::llm::LlmClient> = Arc::new(FakeLlm {
            responses: tokio::sync::Mutex::new(vec![
                LlmResponse::Text(r#"["执行任务"]"#.to_string()),
                LlmResponse::Text("step done".to_string()),
                LlmResponse::Text("ok".to_string()),
            ]),
        });
        let mut registry = crate::tools::registry::ToolRegistry::new(ToolContext::default());
        registry.register(Arc::new(FakeTool));
        Arc::new(SubagentManager::new(client, registry))
    }

    #[tokio::test]
    async fn test_subagent_create_tool() {
        let manager = make_manager();
        let tool = SubagentCreateTool::new(manager);
        let mut args = HashMap::new();
        args.insert("name".to_string(), Value::String("t".to_string()));
        args.insert(
            "system_prompt".to_string(),
            Value::String("sys".to_string()),
        );
        args.insert(
            "allowed_tools".to_string(),
            Value::Array(vec![Value::String("fake".to_string())]),
        );

        let result = tool.execute(args, &ToolContext::default()).await.unwrap();
        let id = result.to_string_for_model();
        assert!(!id.is_empty());
    }

    #[tokio::test]
    async fn test_subagent_run_and_delete_tools() {
        let manager = make_manager();
        let id = manager
            .create("t".to_string(), "sys".to_string(), vec![])
            .await
            .unwrap();

        let run_tool = SubagentRunTool::new(manager.clone());
        let mut args = HashMap::new();
        args.insert("id".to_string(), Value::String(id.clone()));
        args.insert("task".to_string(), Value::String("go".to_string()));
        let result = run_tool
            .execute(args, &ToolContext::default())
            .await
            .unwrap();
        let text = result.to_string_for_model();
        assert!(text.contains("ok"));

        let list_tool = SubagentListTool::new(manager.clone());
        let result = list_tool
            .execute(HashMap::new(), &ToolContext::default())
            .await
            .unwrap();
        assert!(result.to_string_for_model().contains(&id));

        let delete_tool = SubagentDeleteTool::new(manager);
        let mut args = HashMap::new();
        args.insert("id".to_string(), Value::String(id));
        let result = delete_tool
            .execute(args, &ToolContext::default())
            .await
            .unwrap();
        assert_eq!(result.to_string_for_model(), "已删除");
    }
}
