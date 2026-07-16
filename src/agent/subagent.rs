use anyhow::Result;
use std::sync::Arc;
use tokio::sync::Mutex;

use crate::agent::llm::{LlmClient, ToolCall};
use crate::agent::runner::PlanExecuteRunner;
use crate::agent::session::SessionContext;
use crate::tools::registry::ToolRegistry;

/// Subagent 执行结果
#[derive(Debug, Clone)]
pub struct SubagentResult {
    pub output: String,
    pub tool_calls: Vec<ToolCall>,
}

/// 一个轻量级子 Agent，与主 Agent 共享 Tokio runtime，
/// 但拥有独立的会话上下文与经过裁剪的工具集。
pub struct Subagent {
    pub id: String,
    pub name: String,
    pub system_prompt: String,
    pub allowed_tools: Vec<String>,
    client: Arc<dyn LlmClient>,
    registry: Arc<Mutex<ToolRegistry>>,
}

impl Subagent {
    pub fn new(
        id: impl Into<String>,
        name: impl Into<String>,
        system_prompt: impl Into<String>,
        allowed_tools: Vec<String>,
        client: Arc<dyn LlmClient>,
        registry: Arc<Mutex<ToolRegistry>>,
    ) -> Self {
        Self {
            id: id.into(),
            name: name.into(),
            system_prompt: system_prompt.into(),
            allowed_tools,
            client,
            registry,
        }
    }

    /// 在独立会话中运行任务，最多允许 max_iterations 轮工具调用。
    pub async fn run(&self, task: &str, max_iterations: usize) -> Result<SubagentResult> {
        let runner = PlanExecuteRunner::new(self.client.clone(), self.registry.clone())
            .with_max_iterations(max_iterations);
        let mut session = SessionContext::new(&self.system_prompt);
        let output = runner.run(&mut session, task, None).await?;

        let tool_calls: Vec<ToolCall> = session
            .messages
            .iter()
            .filter_map(|m| m.tool_calls.clone())
            .flatten()
            .collect();

        Ok(SubagentResult { output, tool_calls })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent::llm::{FunctionCall, LlmResponse, Message, ToolCall, ToolDefinition};
    use crate::tools::schema::{Tool, ToolContext, ToolResult, ToolSchema};
    use async_trait::async_trait;
    use serde_json::Value;
    use std::collections::HashMap;

    struct FakeLlm {
        responses: Mutex<Vec<LlmResponse>>,
    }

    #[async_trait]
    impl LlmClient for FakeLlm {
        async fn chat(
            &self,
            _messages: Vec<Message>,
            _tools: Vec<ToolDefinition>,
        ) -> Result<LlmResponse> {
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
        ) -> Result<ToolResult> {
            Ok(ToolResult::Text("done".to_string()))
        }
    }

    #[tokio::test]
    async fn test_subagent_text_response() {
        let client: Arc<dyn LlmClient> = Arc::new(FakeLlm {
            responses: Mutex::new(vec![
                LlmResponse::Text(r#"["回答用户问题"]"#.to_string()),
                LlmResponse::Text("step done".to_string()),
                LlmResponse::Text("hello from sub".to_string()),
            ]),
        });
        let mut registry = ToolRegistry::new(ToolContext::default());
        registry.register(Arc::new(FakeTool));
        let subagent = Subagent::new(
            "id1",
            "test",
            "you are a tester",
            vec![],
            client,
            Arc::new(Mutex::new(registry)),
        );

        let result = subagent.run("task", 5).await.unwrap();
        assert_eq!(result.output, "hello from sub");
        assert!(result.tool_calls.is_empty());
    }

    #[tokio::test]
    async fn test_subagent_tool_call() {
        let client: Arc<dyn LlmClient> = Arc::new(FakeLlm {
            responses: Mutex::new(vec![
                LlmResponse::Text(r#"["调用工具完成任务"]"#.to_string()),
                LlmResponse::ToolCalls(vec![ToolCall {
                    id: "1".to_string(),
                    call_type: "function".to_string(),
                    function: FunctionCall {
                        name: "fake".to_string(),
                        arguments: "{}".to_string(),
                    },
                }]),
                LlmResponse::Text("step done".to_string()),
                LlmResponse::Text("after tool".to_string()),
            ]),
        });
        let mut registry = ToolRegistry::new(ToolContext::default());
        registry.register(Arc::new(FakeTool));
        let subagent = Subagent::new(
            "id2",
            "test",
            "you are a tester",
            vec![],
            client,
            Arc::new(Mutex::new(registry)),
        );

        let result = subagent.run("task", 5).await.unwrap();
        assert_eq!(result.output, "after tool");
        assert_eq!(result.tool_calls.len(), 1);
    }
}
