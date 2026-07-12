use anyhow::{Context, Result};
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::{Mutex, RwLock};

use crate::agent::llm::LlmClient;
use crate::agent::subagent::{Subagent, SubagentResult};
use crate::tools::registry::ToolRegistry;

/// Subagent 元信息，用于列表展示
#[derive(Debug, Clone)]
pub struct SubagentInfo {
    pub id: String,
    pub name: String,
    pub allowed_tools: Vec<String>,
}

/// 管理所有子 Agent 的生命周期与运行。
///
/// 子 Agent 拥有独立的 `ToolRegistry` 副本，因此运行时不会与主 Agent
/// 争夺同一把锁，避免在工具调用链中出现死锁。
pub struct SubagentManager {
    subagents: Arc<RwLock<HashMap<String, Arc<Subagent>>>>,
    client: Arc<dyn LlmClient>,
    base_registry: ToolRegistry,
}

impl SubagentManager {
    pub fn new(client: Arc<dyn LlmClient>, base_registry: ToolRegistry) -> Self {
        Self {
            subagents: Arc::new(RwLock::new(HashMap::new())),
            client,
            base_registry,
        }
    }

    /// 创建并保存一个 Subagent，返回其 ID。
    pub async fn create(
        &self,
        name: String,
        system_prompt: String,
        allowed_tools: Vec<String>,
    ) -> Result<String> {
        let mut child_registry = self.base_registry.clone();
        child_registry.whitelist(&allowed_tools);

        let id = uuid::Uuid::new_v4().to_string();
        let subagent = Arc::new(Subagent::new(
            id.clone(),
            name,
            system_prompt,
            allowed_tools,
            self.client.clone(),
            Arc::new(Mutex::new(child_registry)),
        ));

        self.subagents.write().await.insert(id.clone(), subagent);
        Ok(id)
    }

    /// 运行指定 Subagent 并返回结果。
    pub async fn run(&self, id: &str, task: &str, max_iterations: usize) -> Result<SubagentResult> {
        let subagent = {
            let map = self.subagents.read().await;
            map.get(id)
                .cloned()
                .with_context(|| format!("subagent {} 不存在", id))?
        };
        subagent.run(task, max_iterations).await
    }

    /// 一次性创建并运行 Subagent，不保留在管理器中。
    pub async fn create_and_run(
        &self,
        name: String,
        system_prompt: String,
        allowed_tools: Vec<String>,
        task: &str,
        max_iterations: usize,
    ) -> Result<SubagentResult> {
        let mut child_registry = self.base_registry.clone();
        child_registry.whitelist(&allowed_tools);

        let subagent = Arc::new(Subagent::new(
            uuid::Uuid::new_v4().to_string(),
            name,
            system_prompt,
            allowed_tools,
            self.client.clone(),
            Arc::new(Mutex::new(child_registry)),
        ));

        subagent.run(task, max_iterations).await
    }

    /// 列出所有已创建的 Subagent。
    pub async fn list(&self) -> Vec<SubagentInfo> {
        let map = self.subagents.read().await;
        map.values()
            .map(|s| SubagentInfo {
                id: s.id.clone(),
                name: s.name.clone(),
                allowed_tools: s.allowed_tools.clone(),
            })
            .collect()
    }

    /// 删除指定 Subagent。
    pub async fn delete(&self, id: &str) -> Result<()> {
        self.subagents
            .write()
            .await
            .remove(id)
            .context("subagent 不存在")?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent::llm::{LlmResponse, Message, ToolDefinition};
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

    fn make_manager() -> SubagentManager {
        let client: Arc<dyn LlmClient> = Arc::new(FakeLlm {
            responses: Mutex::new(vec![LlmResponse::Text("ok".to_string())]),
        });
        let mut registry = ToolRegistry::new(ToolContext::default());
        registry.register(Arc::new(FakeTool));
        SubagentManager::new(client, registry)
    }

    #[tokio::test]
    async fn test_create_and_run() {
        let manager = make_manager();
        let id = manager
            .create(
                "tester".to_string(),
                "you test".to_string(),
                vec!["fake".to_string()],
            )
            .await
            .unwrap();
        assert!(!id.is_empty());

        let result = manager.run(&id, "do it", 3).await.unwrap();
        assert_eq!(result.output, "ok");
    }

    #[tokio::test]
    async fn test_list_and_delete() {
        let manager = make_manager();
        let id = manager
            .create("lister".to_string(), "sys".to_string(), vec![])
            .await
            .unwrap();

        let list = manager.list().await;
        assert_eq!(list.len(), 1);
        assert_eq!(list[0].name, "lister");

        manager.delete(&id).await.unwrap();
        assert!(manager.list().await.is_empty());
        assert!(manager.delete(&id).await.is_err());
    }

    #[tokio::test]
    async fn test_run_missing() {
        let manager = make_manager();
        assert!(manager.run("missing", "task", 3).await.is_err());
    }

    #[tokio::test]
    async fn test_create_and_run_no_register() {
        let manager = make_manager();
        let result = manager
            .create_and_run(
                "oneoff".to_string(),
                "sys".to_string(),
                vec!["fake".to_string()],
                "task",
                3,
            )
            .await
            .unwrap();
        assert_eq!(result.output, "ok");
    }
}
