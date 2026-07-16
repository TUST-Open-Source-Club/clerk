use std::collections::HashMap;
use std::sync::Arc;

use crate::agent::llm::ToolDefinition;
use crate::tools::schema::{Tool, ToolContext};
use serde_json::Value;

/// 工具注册表：管理本地工具与 MCP 工具
#[derive(Clone)]
pub struct ToolRegistry {
    tools: HashMap<String, Arc<dyn Tool>>,
    context: ToolContext,
}

impl ToolRegistry {
    /// 创建空的注册表，携带工具执行上下文。
    pub fn new(context: ToolContext) -> Self {
        Self {
            tools: HashMap::new(),
            context,
        }
    }

    /// 注册工具，同名工具会被覆盖。
    pub fn register(&mut self, tool: Arc<dyn Tool>) {
        self.tools.insert(tool.name().to_string(), tool);
    }

    pub fn get(&self, name: &str) -> Option<&dyn Tool> {
        self.tools.get(name).map(|t| t.as_ref())
    }

    pub fn names(&self) -> Vec<String> {
        self.tools.keys().cloned().collect()
    }

    /// 收集所有工具的 OpenAI 格式定义。
    pub fn tool_definitions(&self) -> Vec<ToolDefinition> {
        self.tools
            .values()
            .map(|t| t.schema().into_tool_definition())
            .collect()
    }

    pub fn context(&self) -> &ToolContext {
        &self.context
    }

    /// 判断指定工具执行前是否需要用户审批。
    /// 未配置 permissions 时一律不需要审批（向后兼容）。
    pub fn requires_approval(&self, name: &str) -> bool {
        self.context
            .permissions
            .as_ref()
            .is_some_and(|p| p.requires_approval(name))
    }

    pub fn set_context(&mut self, context: ToolContext) {
        self.context = context;
    }

    /// 移除指定工具，用于构造子 Agent 的工具白名单。
    pub fn remove(&mut self, name: &str) {
        self.tools.remove(name);
    }

    /// 仅保留白名单中的工具，返回自身以便链式调用。
    pub fn whitelist(&mut self, names: &[String]) -> &mut Self {
        if names.is_empty() {
            return self;
        }
        let to_remove: Vec<String> = self
            .tools
            .keys()
            .filter(|k| !names.contains(k))
            .cloned()
            .collect();
        for name in to_remove {
            self.tools.remove(&name);
        }
        self
    }

    /// 按名称执行工具，未知名称返回错误。
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
        registry.register(Arc::new(EchoTool));
        assert_eq!(registry.names(), vec!["echo"]);
        assert_eq!(registry.tool_definitions().len(), 1);
    }

    #[tokio::test]
    async fn test_execute() {
        let mut registry = ToolRegistry::new(ToolContext::default());
        registry.register(Arc::new(EchoTool));

        let mut args = HashMap::new();
        args.insert("msg".to_string(), Value::String("hello".to_string()));

        let result = registry.execute("echo", args).await.unwrap();
        assert_eq!(result.to_string_for_model(), "hello");
    }

    #[tokio::test]
    async fn test_context_and_remove() {
        let mut registry = ToolRegistry::new(ToolContext::default());
        registry.register(Arc::new(EchoTool));
        assert!(registry.context().working_dir.as_os_str().is_empty());

        let new_ctx = ToolContext {
            working_dir: std::path::PathBuf::from("/tmp"),
            permissions: None,
        };
        registry.set_context(new_ctx.clone());
        assert_eq!(registry.context().working_dir, new_ctx.working_dir);

        registry.remove("echo");
        assert!(registry.names().is_empty());
        assert!(registry.execute("echo", HashMap::new()).await.is_err());
    }

    #[test]
    fn test_requires_approval_without_permissions() {
        let registry = ToolRegistry::new(ToolContext::default());
        assert!(!registry.requires_approval("shell"));
    }

    #[test]
    fn test_requires_approval_with_permissions() {
        let registry = ToolRegistry::new(ToolContext {
            permissions: Some(crate::config::PermissionConfig::default()),
            ..Default::default()
        });
        assert!(!registry.requires_approval("fs_read"));
        assert!(registry.requires_approval("shell"));

        let yolo_registry = ToolRegistry::new(ToolContext {
            permissions: Some(crate::config::PermissionConfig {
                yolo: true,
                ..Default::default()
            }),
            ..Default::default()
        });
        assert!(!yolo_registry.requires_approval("shell"));
    }
}
