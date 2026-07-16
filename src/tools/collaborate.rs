use std::collections::HashMap;
use std::sync::Arc;

use anyhow::Context;
use serde_json::Value;

use crate::agent::subagent_manager::SubagentManager;
use crate::tools::schema::{Tool, ToolContext, ToolResult, ToolSchema, get_i64};

/// 解析 agents 参数：接受 JSON 数组或 JSON 数组字符串，逐项提取 name/system_prompt/task/allowed_tools。
fn parse_agent_specs(args: &HashMap<String, Value>) -> anyhow::Result<Vec<AgentSpec>> {
    let value = args
        .get("agents")
        .ok_or_else(|| anyhow::anyhow!("缺少参数: agents"))?;

    let arr = match value {
        Value::Array(a) => a.clone(),
        Value::String(s) => serde_json::from_str::<Vec<Value>>(s)
            .with_context(|| format!("agents 不是合法 JSON 数组: {}", s))?,
        _ => anyhow::bail!("agents 必须是数组或 JSON 数组字符串"),
    };

    let mut specs = Vec::new();
    for item in &arr {
        let obj = item
            .as_object()
            .ok_or_else(|| anyhow::anyhow!("agents 数组元素必须是对象"))?;
        let name = obj
            .get("name")
            .and_then(|v| v.as_str())
            .unwrap_or("subagent")
            .to_string();
        let system_prompt = obj
            .get("system_prompt")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        let task = obj
            .get("task")
            .and_then(|v| v.as_str())
            .ok_or_else(|| anyhow::anyhow!("每个 agent 必须提供 task"))?
            .to_string();
        let allowed_tools = obj
            .get("allowed_tools")
            .and_then(|v| v.as_array())
            .map(|arr| {
                arr.iter()
                    .filter_map(|v| v.as_str().map(|s| s.to_string()))
                    .collect()
            })
            .unwrap_or_default();

        specs.push(AgentSpec {
            name,
            system_prompt,
            task,
            allowed_tools,
        });
    }
    if specs.is_empty() {
        anyhow::bail!("agents 数组不能为空");
    }
    Ok(specs)
}

/// 单个子 Agent 的任务规格。
#[derive(Debug, Clone)]
struct AgentSpec {
    name: String,
    system_prompt: String,
    task: String,
    allowed_tools: Vec<String>,
}

/// `collaborate_parallel` 工具：并行派发多个子 Agent，结果按 name 汇总为 JSON。
pub struct CollaborateParallelTool {
    manager: Arc<SubagentManager>,
}

impl CollaborateParallelTool {
    pub fn new(manager: Arc<SubagentManager>) -> Self {
        Self { manager }
    }
}

#[async_trait::async_trait]
impl Tool for CollaborateParallelTool {
    fn name(&self) -> &str {
        "collaborate_parallel"
    }

    fn description(&self) -> &str {
        "并行创建多个子 Agent 分别执行任务，并汇总结果。\
         各子 Agent 独立运行，单个失败不影响其余，结果以 JSON 对象按 name 汇总。"
    }

    fn schema(&self) -> ToolSchema {
        ToolSchema::new("collaborate_parallel", self.description())
            .with_string(
                "agents",
                "子 Agent 列表 JSON 数组，每项包含 name/system_prompt/task/allowed_tools",
                true,
            )
            .with_string(
                "context",
                "所有子 Agent 共享的背景/任务描述（可选），会拼接到每个 agent 的任务前",
                false,
            )
            .with_integer("max_iterations", "最大迭代次数（默认 10）", false)
    }

    async fn execute(
        &self,
        args: HashMap<String, Value>,
        _ctx: &ToolContext,
    ) -> anyhow::Result<ToolResult> {
        let specs = parse_agent_specs(&args)?;
        let max_iterations = get_i64(&args, "max_iterations", 10) as usize;
        let shared_context = args
            .get("context")
            .and_then(|v| v.as_str())
            .filter(|s| !s.trim().is_empty())
            .map(|s| s.to_string());

        let mut handles = Vec::new();
        for spec in specs {
            let name = spec.name.clone();
            let manager = self.manager.clone();
            let context = shared_context.clone();
            let handle = tokio::spawn(async move {
                let task = match &context {
                    Some(c) => format!("共享背景：\n{}\n\n任务：\n{}", c, spec.task),
                    None => spec.task,
                };
                manager
                    .create_and_run(
                        spec.name,
                        spec.system_prompt,
                        spec.allowed_tools,
                        &task,
                        max_iterations,
                    )
                    .await
            });
            handles.push((name, handle));
        }

        // 逐个收集结果：单个子 Agent 失败（或任务 panic/取消）只标记该 Agent，
        // 不影响其余 Agent 的结果汇总。
        let mut results = serde_json::Map::new();
        for (name, handle) in handles {
            let key = unique_key(&results, &name);
            let entry = match handle.await {
                Ok(Ok(result)) => serde_json::json!({
                    "status": "ok",
                    "result": result.output,
                    "tool_calls": result.tool_calls.len(),
                }),
                Ok(Err(e)) => serde_json::json!({
                    "status": "error",
                    "error": format!("{:#}", e),
                }),
                Err(e) => serde_json::json!({
                    "status": "error",
                    "error": format!("子 Agent 任务异常终止: {}", e),
                }),
            };
            results.insert(key, entry);
        }

        Ok(ToolResult::Json(Value::Object(results)))
    }
}

/// 生成不重复的结果 key：name 冲突时追加 -2、-3 等后缀。
fn unique_key(map: &serde_json::Map<String, Value>, name: &str) -> String {
    if !map.contains_key(name) {
        return name.to_string();
    }
    let mut i = 2;
    loop {
        let candidate = format!("{}-{}", name, i);
        if !map.contains_key(&candidate) {
            return candidate;
        }
        i += 1;
    }
}

/// `collaborate_sequential` 工具：顺序执行多个子 Agent，后者可读取前者输出。
pub struct CollaborateSequentialTool {
    manager: Arc<SubagentManager>,
}

impl CollaborateSequentialTool {
    pub fn new(manager: Arc<SubagentManager>) -> Self {
        Self { manager }
    }
}

#[async_trait::async_trait]
impl Tool for CollaborateSequentialTool {
    fn name(&self) -> &str {
        "collaborate_sequential"
    }

    fn description(&self) -> &str {
        "顺序创建多个子 Agent 执行任务，后一个可以读取前一个的输出。"
    }

    fn schema(&self) -> ToolSchema {
        ToolSchema::new("collaborate_sequential", self.description())
            .with_string(
                "agents",
                "子 Agent 列表 JSON 数组，每项包含 name/system_prompt/task/allowed_tools",
                true,
            )
            .with_integer("max_iterations", "最大迭代次数（默认 10）", false)
    }

    async fn execute(
        &self,
        args: HashMap<String, Value>,
        _ctx: &ToolContext,
    ) -> anyhow::Result<ToolResult> {
        let specs = parse_agent_specs(&args)?;
        let max_iterations = get_i64(&args, "max_iterations", 10) as usize;

        let mut results = Vec::new();
        let mut previous_output = String::new();

        for spec in specs {
            let task = if previous_output.is_empty() {
                spec.task
            } else {
                format!("{}\n\n前一个步骤的输出:\n{}", spec.task, previous_output)
            };

            let result = self
                .manager
                .create_and_run(
                    spec.name,
                    spec.system_prompt,
                    spec.allowed_tools,
                    &task,
                    max_iterations,
                )
                .await?;
            previous_output = result.output.clone();
            results.push(serde_json::json!({
                "output": result.output,
                "tool_calls": result.tool_calls.len(),
            }));
        }

        Ok(ToolResult::Json(Value::Array(results)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent::llm::{LlmResponse, Message, ToolDefinition};
    use crate::tools::registry::ToolRegistry;
    use crate::tools::schema::{Tool, ToolContext, ToolResult, ToolSchema};
    use async_trait::async_trait;

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
        // 每个子 Agent 一次运行消耗 3 条响应：计划、步骤结果、最终总结。
        // 所有响应都包含 first/second，保证并行消费顺序不确定时断言仍成立。
        let client: Arc<dyn crate::agent::llm::LlmClient> = Arc::new(FakeLlm {
            responses: tokio::sync::Mutex::new(vec![
                LlmResponse::Text("first plan".to_string()),
                LlmResponse::Text("first step".to_string()),
                LlmResponse::Text("first".to_string()),
                LlmResponse::Text("second plan".to_string()),
                LlmResponse::Text("second step".to_string()),
                LlmResponse::Text("second".to_string()),
            ]),
        });
        let mut registry = ToolRegistry::new(ToolContext::default());
        registry.register(Arc::new(FakeTool));
        Arc::new(SubagentManager::new(client, registry))
    }

    /// 根据任务内容决定行为的 LLM：消息中包含 "boom" 时模拟调用失败。
    struct TaskAwareFakeLlm;

    #[async_trait]
    impl crate::agent::llm::LlmClient for TaskAwareFakeLlm {
        async fn chat(
            &self,
            messages: Vec<Message>,
            _tools: Vec<ToolDefinition>,
        ) -> anyhow::Result<LlmResponse> {
            let combined: String = messages.iter().map(|m| m.content.clone()).collect();
            if combined.contains("boom") {
                anyhow::bail!("模拟 LLM 调用失败");
            }
            Ok(LlmResponse::Text("ok".to_string()))
        }
    }

    /// 记录所有收到的消息内容，用于验证共享 context 是否传递给子 Agent。
    struct RecordingFakeLlm {
        seen: Arc<tokio::sync::Mutex<Vec<String>>>,
    }

    #[async_trait]
    impl crate::agent::llm::LlmClient for RecordingFakeLlm {
        async fn chat(
            &self,
            messages: Vec<Message>,
            _tools: Vec<ToolDefinition>,
        ) -> anyhow::Result<LlmResponse> {
            let mut seen = self.seen.lock().await;
            for m in &messages {
                seen.push(m.content.clone());
            }
            Ok(LlmResponse::Text("ok".to_string()))
        }
    }

    fn manager_with_client(client: Arc<dyn crate::agent::llm::LlmClient>) -> Arc<SubagentManager> {
        let mut registry = ToolRegistry::new(ToolContext::default());
        registry.register(Arc::new(FakeTool));
        Arc::new(SubagentManager::new(client, registry))
    }

    #[tokio::test]
    async fn test_collaborate_parallel() {
        let tool = CollaborateParallelTool::new(make_manager());
        let mut args = HashMap::new();
        args.insert(
            "agents".to_string(),
            Value::Array(vec![
                serde_json::json!({"name": "a", "task": "t1"}),
                serde_json::json!({"name": "b", "task": "t2"}),
            ]),
        );

        let result = tool.execute(args, &ToolContext::default()).await.unwrap();
        let ToolResult::Json(Value::Object(map)) = &result else {
            panic!("结果应为 JSON 对象");
        };
        assert_eq!(map.len(), 2);
        assert!(map.contains_key("a"));
        assert!(map.contains_key("b"));
        for entry in map.values() {
            assert_eq!(entry["status"], "ok");
            assert!(entry["result"].is_string());
            assert!(entry["tool_calls"].is_number());
        }
    }

    #[tokio::test]
    async fn test_collaborate_parallel_partial_failure() {
        let tool = CollaborateParallelTool::new(manager_with_client(Arc::new(TaskAwareFakeLlm)));
        let mut args = HashMap::new();
        args.insert(
            "agents".to_string(),
            Value::Array(vec![
                serde_json::json!({"name": "good", "task": "正常任务"}),
                serde_json::json!({"name": "bad", "task": "boom 任务"}),
            ]),
        );

        // 单个 agent 失败不应导致整个工具失败
        let result = tool.execute(args, &ToolContext::default()).await.unwrap();
        let ToolResult::Json(Value::Object(map)) = &result else {
            panic!("结果应为 JSON 对象");
        };
        assert_eq!(map["good"]["status"], "ok");
        assert_eq!(map["good"]["result"], "ok");
        assert_eq!(map["bad"]["status"], "error");
        assert!(
            map["bad"]["error"]
                .as_str()
                .unwrap()
                .contains("模拟 LLM 调用失败")
        );
    }

    #[tokio::test]
    async fn test_collaborate_parallel_shared_context() {
        let seen = Arc::new(tokio::sync::Mutex::new(Vec::new()));
        let tool = CollaborateParallelTool::new(manager_with_client(Arc::new(RecordingFakeLlm {
            seen: seen.clone(),
        })));
        let mut args = HashMap::new();
        args.insert(
            "agents".to_string(),
            Value::Array(vec![
                serde_json::json!({"name": "a", "task": "t1"}),
                serde_json::json!({"name": "b", "task": "t2"}),
            ]),
        );
        args.insert(
            "context".to_string(),
            Value::String("共享背景信息-XYZ".to_string()),
        );

        let result = tool.execute(args, &ToolContext::default()).await.unwrap();
        let ToolResult::Json(Value::Object(map)) = &result else {
            panic!("结果应为 JSON 对象");
        };
        assert_eq!(map.len(), 2);

        // 两个子 Agent 都应在其消息中看到共享 context
        let seen = seen.lock().await;
        let hits = seen
            .iter()
            .filter(|c| c.contains("共享背景信息-XYZ"))
            .count();
        assert!(hits >= 2, "两个子 Agent 都应看到共享 context");
    }

    #[tokio::test]
    async fn test_collaborate_sequential() {
        let tool = CollaborateSequentialTool::new(make_manager());
        let mut args = HashMap::new();
        args.insert(
            "agents".to_string(),
            Value::Array(vec![
                serde_json::json!({"name": "a", "task": "t1"}),
                serde_json::json!({"name": "b", "task": "t2"}),
            ]),
        );

        let result = tool.execute(args, &ToolContext::default()).await.unwrap();
        let text = result.to_string_for_model();
        assert!(text.contains("first"));
        assert!(text.contains("second"));
    }

    #[tokio::test]
    async fn test_missing_agents() {
        let tool = CollaborateParallelTool::new(make_manager());
        let result = tool.execute(HashMap::new(), &ToolContext::default()).await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn test_missing_task() {
        let tool = CollaborateParallelTool::new(make_manager());
        let mut args = HashMap::new();
        args.insert(
            "agents".to_string(),
            Value::Array(vec![serde_json::json!({"name": "a"})]),
        );
        let result = tool.execute(args, &ToolContext::default()).await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn test_agents_as_json_string() {
        let tool = CollaborateParallelTool::new(make_manager());
        let mut args = HashMap::new();
        args.insert(
            "agents".to_string(),
            Value::String(
                r#"[{"name": "a", "task": "t1", "allowed_tools": ["fake"]}]"#.to_string(),
            ),
        );
        let result = tool.execute(args, &ToolContext::default()).await.unwrap();
        assert!(result.to_string_for_model().contains("first"));
    }

    #[tokio::test]
    async fn test_agents_invalid_element() {
        let tool = CollaborateParallelTool::new(make_manager());
        let mut args = HashMap::new();
        args.insert(
            "agents".to_string(),
            Value::Array(vec![Value::String("not object".to_string())]),
        );
        let result = tool.execute(args, &ToolContext::default()).await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn test_agents_empty_array() {
        let tool = CollaborateParallelTool::new(make_manager());
        let mut args = HashMap::new();
        args.insert("agents".to_string(), Value::Array(vec![]));
        let result = tool.execute(args, &ToolContext::default()).await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn test_agents_invalid_type() {
        let tool = CollaborateParallelTool::new(make_manager());
        let mut args = HashMap::new();
        args.insert("agents".to_string(), Value::Number(42.into()));
        let result = tool.execute(args, &ToolContext::default()).await;
        assert!(result.is_err());
    }
}
