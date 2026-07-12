use std::collections::HashMap;

use crate::agent::llm::ToolDefinition;
use crate::tools::schema::{Tool, ToolContext};
use serde_json::Value;

/// 工具注册表：管理本地工具与 MCP 工具
pub struct ToolRegistry {
    tools: HashMap<String, Box<dyn Tool>>,
    context: ToolContext,
}

impl ToolRegistry {
    pub fn new(context: ToolContext) -> Self {
        Self {
            tools: HashMap::new(),
            context,
        }
    }

    pub fn register(&mut self, tool: Box<dyn Tool>) {
        self.tools.insert(tool.name().to_string(), tool);
    }

    pub fn get(&self, name: &str) -> Option<&dyn Tool> {
        self.tools.get(name).map(|t| t.as_ref())
    }

    pub fn names(&self) -> Vec<String> {
        self.tools.keys().cloned().collect()
    }

    pub fn tool_definitions(&self) -> Vec<ToolDefinition> {
        self.tools
            .values()
            .map(|t| t.schema().into_tool_definition())
            .collect()
    }

    pub fn context(&self) -> &ToolContext {
        &self.context
    }

    pub fn set_context(&mut self, context: ToolContext) {
        self.context = context;
    }

    pub async fn execute(
        &self,
        name: &str,
        args: HashMap<String, Value>,
    ) -> anyhow::Result<crate::tools::schema::ToolResult> {
        match self.tools.get(name) {
            Some(tool) => tool.execute(args, &self.context).await,
            None => Err(anyhow::anyhow!("未知工具: {}", name)),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tools::schema::{ToolResult, ToolSchema};

    struct EchoTool;

    #[async_trait::async_trait]
    impl Tool for EchoTool {
        fn name(&self) -> &str {
            "echo"
        }

        fn description(&self) -> &str {
            "echo"
        }

        fn schema(&self) -> ToolSchema {
            ToolSchema::new("echo", "echo").with_string("msg", "message", true)
        }

        async fn execute(
            &self,
            args: HashMap<String, Value>,
            _ctx: &ToolContext,
        ) -> anyhow::Result<ToolResult> {
            Ok(ToolResult::Text(
                args.get("msg").unwrap().as_str().unwrap().to_string(),
            ))
        }
    }

    #[test]
    fn test_register_and_definitions() {
        let mut registry = ToolRegistry::new(ToolContext::default());
        registry.register(Box::new(EchoTool));
        assert_eq!(registry.names(), vec!["echo"]);
        assert_eq!(registry.tool_definitions().len(), 1);
    }

    #[tokio::test]
    async fn test_execute() {
        let mut registry = ToolRegistry::new(ToolContext::default());
        registry.register(Box::new(EchoTool));

        let mut args = HashMap::new();
        args.insert("msg".to_string(), Value::String("hello".to_string()));

        let result = registry.execute("echo", args).await.unwrap();
        assert_eq!(result.to_string_for_model(), "hello");
    }
}
