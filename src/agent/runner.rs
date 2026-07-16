use anyhow::{Context, Result};
use serde_json::Value;
use std::sync::Arc;
use tokio::sync::{Mutex, mpsc};
use tokio_stream::StreamExt;
use tracing::{info, warn};

use crate::agent::llm::client::Role;
use crate::agent::llm::{FunctionCall, LlmClient, LlmResponse, Message, StreamChunk};
use crate::agent::session::SessionContext;
use crate::tools::registry::ToolRegistry;

/// 工具结果字符串的错误前缀，用于识别工具执行失败
const TOOL_ERROR_PREFIX: &str = "工具执行失败";

/// 计划生成提示词：要求 LLM 输出 JSON 字符串数组
const PLAN_PROMPT: &str = "请为上述用户请求制定一个简洁的执行计划。\n\
     要求：\n\
     - 结合可用工具，将任务分解为 1-5 个可独立执行的步骤；\n\
     - 每个步骤用一句话说明要做什么；\n\
     - 只输出 JSON 字符串数组，例如 [\"步骤一\", \"步骤二\"]；\n\
     - 不要输出其它内容，不要调用工具。";

/// 总结提示词：所有步骤执行完毕后生成面向用户的最终回复
const SUMMARY_PROMPT: &str = "以上计划的所有步骤已执行完毕。请根据执行过程和结果，给出面向用户的最终回复，\
     要求简洁、直接回答用户的问题。";

/// 工具调用事件，用于 TUI 展示
#[derive(Debug, Clone)]
pub enum RunnerEvent {
    Plan { steps: Vec<String> },
    ToolCall { name: String, arguments: Value },
    ToolResult { name: String, result: String },
    Error(String),
}

/// Plan-Execute 模式的 Agent 运行器：
/// 先为用户请求生成执行计划，再逐步执行（每步内部是受限的工具调用循环），
/// 步骤失败时允许有限次重计划，最后总结结果回复用户。
#[derive(Clone)]
pub struct PlanExecuteRunner {
    client: Arc<dyn LlmClient>,
    registry: Arc<Mutex<ToolRegistry>>,
    /// 单个步骤内允许的最大工具调用轮数
    max_iterations: usize,
    /// 单次请求允许的最大重计划次数
    max_replans: usize,
}

impl PlanExecuteRunner {
    pub fn new(client: Arc<dyn LlmClient>, registry: Arc<Mutex<ToolRegistry>>) -> Self {
        Self {
            client,
            registry,
            max_iterations: 10,
            max_replans: 1,
        }
    }

    pub fn with_max_iterations(mut self, max: usize) -> Self {
        self.max_iterations = max;
        self
    }

    /// 非流式运行：制定计划 -> 逐步执行 -> 总结结果。
    pub async fn run(
        &self,
        ctx: &mut SessionContext,
        user_input: &str,
        event_tx: Option<mpsc::UnboundedSender<RunnerEvent>>,
    ) -> Result<String> {
        ctx.add_message(Message::user(user_input));
        let steps = self.generate_plan(ctx, event_tx.as_ref()).await?;
        self.execute_plan(ctx, steps, event_tx.as_ref()).await?;
        self.summarize(ctx).await
    }

    /// 流式运行：计划生成与步骤执行使用非流式调用，仅最终总结优先使用流式输出。
    /// 若流式不支持工具调用或流中出现工具调用，则回退到非流式总结。
    pub async fn run_stream(
        &self,
        ctx: Arc<Mutex<SessionContext>>,
        user_input: &str,
        chunk_tx: mpsc::UnboundedSender<StreamChunk>,
        event_tx: Option<mpsc::UnboundedSender<RunnerEvent>>,
    ) -> Result<String> {
        {
            let mut ctx = ctx.lock().await;
            ctx.add_message(Message::user(user_input));
            let steps = self.generate_plan(&mut ctx, event_tx.as_ref()).await?;
            self.execute_plan(&mut ctx, steps, event_tx.as_ref())
                .await?;
        }

        let messages = {
            let ctx = ctx.lock().await;
            let mut messages = ctx.build_messages();
            messages.push(Message::user(SUMMARY_PROMPT));
            messages
        };

        let mut stream = match self.client.chat_stream(messages, vec![]).await {
            Ok(s) => s,
            Err(e) if e.to_string().contains("不支持工具调用") => {
                let mut ctx = ctx.lock().await;
                return self.summarize(&mut ctx).await;
            }
            Err(e) => return Err(e),
        };

        let mut full = String::new();
        while let Some(chunk) = stream.next().await {
            match chunk {
                Ok(chunk) => {
                    if let Some(text) = &chunk.content {
                        full.push_str(text);
                    }
                    let _ = chunk_tx.send(chunk);
                }
                Err(e) if e.to_string().contains("不支持工具调用") => {
                    let mut ctx = ctx.lock().await;
                    return self.summarize(&mut ctx).await;
                }
                Err(e) => return Err(e),
            }
        }

        let mut ctx = ctx.lock().await;
        ctx.add_message(Message::assistant(full.clone()));
        Ok(full)
    }

    /// 生成执行计划：要求 LLM 基于可用工具定义输出 JSON 字符串数组；
    /// 解析失败时回退为以用户请求为唯一步骤的单步计划。
    async fn generate_plan(
        &self,
        ctx: &mut SessionContext,
        event_tx: Option<&mpsc::UnboundedSender<RunnerEvent>>,
    ) -> Result<Vec<String>> {
        info!("生成执行计划");
        let mut messages = ctx.build_messages();
        messages.push(Message::user(PLAN_PROMPT));
        let tools = self.tool_definitions().await;

        let steps = match self.client.chat(messages, tools).await? {
            LlmResponse::Text(text) => parse_plan(&text),
            LlmResponse::ToolCalls(_) => {
                warn!("计划生成返回了工具调用，回退为单步计划");
                Vec::new()
            }
        };

        let steps = if steps.is_empty() {
            let fallback = ctx
                .messages
                .last()
                .map(|m| m.content.clone())
                .unwrap_or_default();
            vec![fallback]
        } else {
            steps
        };

        info!("执行计划: {:?}", steps);
        ctx.add_message(Message::assistant(format!(
            "执行计划：\n{}",
            format_steps(&steps)
        )));
        if let Some(tx) = event_tx {
            let _ = tx.send(RunnerEvent::Plan {
                steps: steps.clone(),
            });
        }
        Ok(steps)
    }

    /// 逐步执行计划；步骤失败或出现工具错误时，允许有限次重计划以调整剩余步骤。
    async fn execute_plan(
        &self,
        ctx: &mut SessionContext,
        mut steps: Vec<String>,
        event_tx: Option<&mpsc::UnboundedSender<RunnerEvent>>,
    ) -> Result<()> {
        let mut replans_left = self.max_replans;
        let mut i = 0;
        while i < steps.len() {
            info!("执行计划步骤 {}/{}", i + 1, steps.len());
            let outcome = self.execute_step(ctx, i, &steps[i], event_tx).await?;

            if outcome.had_error && replans_left > 0 {
                // 已完成的步骤从下一步开始调整；未完成的步骤重试当前步
                let start = if outcome.done { i + 1 } else { i };
                if start < steps.len() {
                    replans_left -= 1;
                    match self.replan(ctx, &steps[i], &steps[start..]).await {
                        Ok(new_remaining) if !new_remaining.is_empty() => {
                            let mut revised = steps[..start].to_vec();
                            revised.extend(new_remaining);
                            steps = revised;
                            info!("调整后剩余计划: {:?}", &steps[start..]);
                            if let Some(tx) = event_tx {
                                let _ = tx.send(RunnerEvent::Plan {
                                    steps: steps[start..].to_vec(),
                                });
                            }
                            i = start;
                            continue;
                        }
                        Ok(_) => {
                            // 重计划认为无需更多步骤，直接结束
                            steps.truncate(start);
                            i = start;
                            continue;
                        }
                        Err(e) => {
                            warn!("重计划失败，按原计划继续: {:#}", e);
                        }
                    }
                }
            }
            i += 1;
        }
        Ok(())
    }

    /// 执行单个步骤：在步骤上下文中循环调用工具，直到模型给出文本结果。
    /// 达到最大迭代次数时标记步骤失败并记录说明。
    async fn execute_step(
        &self,
        ctx: &mut SessionContext,
        index: usize,
        step: &str,
        event_tx: Option<&mpsc::UnboundedSender<RunnerEvent>>,
    ) -> Result<StepOutcome> {
        ctx.add_message(Message::user(format!(
            "请执行计划中的第 {} 步：{}",
            index + 1,
            step
        )));

        let mut had_error = false;
        for iteration in 0..self.max_iterations {
            info!("步骤 {} 迭代 {}", index + 1, iteration + 1);

            let messages = ctx.build_messages();
            let tools = self.tool_definitions().await;

            match self.client.chat(messages, tools).await? {
                LlmResponse::Text(text) => {
                    ctx.add_message(Message::assistant(text));
                    return Ok(StepOutcome {
                        done: true,
                        had_error,
                    });
                }
                LlmResponse::ToolCalls(tool_calls) => {
                    let mut assistant_msg = Message::assistant(String::new());
                    assistant_msg.tool_calls = Some(tool_calls.clone());
                    ctx.add_message(assistant_msg);

                    for tool_call in tool_calls {
                        let result = self.execute_tool_call(&tool_call, event_tx).await?;
                        if result.starts_with(TOOL_ERROR_PREFIX) {
                            had_error = true;
                        }
                        ctx.add_message(Message::tool(tool_call.id.clone(), result));
                    }
                }
            }
        }

        warn!("步骤 {} 达到最大工具调用次数限制", index + 1);
        ctx.add_message(Message::assistant(format!(
            "第 {} 步执行达到最大工具调用次数限制，未能完成。",
            index + 1
        )));
        Ok(StepOutcome {
            done: false,
            had_error: true,
        })
    }

    /// 根据执行进度调整剩余计划，返回新的剩余步骤（空表示无需更多步骤）。
    async fn replan(
        &self,
        ctx: &mut SessionContext,
        failed_step: &str,
        remaining: &[String],
    ) -> Result<Vec<String>> {
        info!("调整剩余计划（失败步骤: {}）", failed_step);
        let mut messages = ctx.build_messages();
        messages.push(Message::user(format!(
            "执行计划时，步骤「{}」失败或出现错误。原计划剩余步骤：\n{}\n\
             请根据已完成的进度调整剩余步骤，只输出 JSON 字符串数组，\
             例如 [\"步骤一\", \"步骤二\"]，不要输出其它内容，不要调用工具。\
             若无需更多步骤，输出空数组 []。",
            failed_step,
            format_steps(remaining)
        )));
        let tools = self.tool_definitions().await;

        let steps = match self.client.chat(messages, tools).await? {
            LlmResponse::Text(text) => parse_plan(&text),
            LlmResponse::ToolCalls(_) => Vec::new(),
        };

        ctx.add_message(Message::assistant(if steps.is_empty() {
            "调整后的剩余计划：（无更多步骤）".to_string()
        } else {
            format!("调整后的剩余计划：\n{}", format_steps(&steps))
        }));
        Ok(steps)
    }

    /// 总结执行结果，生成面向用户的最终回复。
    async fn summarize(&self, ctx: &mut SessionContext) -> Result<String> {
        info!("总结执行结果");
        let mut messages = ctx.build_messages();
        messages.push(Message::user(SUMMARY_PROMPT));

        let text = match self.client.chat(messages, vec![]).await? {
            LlmResponse::Text(text) => text,
            LlmResponse::ToolCalls(_) => {
                warn!("总结返回了工具调用，回退为最后一条步骤结果");
                ctx.messages
                    .iter()
                    .rev()
                    .find(|m| matches!(m.role, Role::Assistant) && !m.content.is_empty())
                    .map(|m| m.content.clone())
                    .unwrap_or_else(|| "计划已执行完成。".to_string())
            }
        };
        ctx.add_message(Message::assistant(text.clone()));
        Ok(text)
    }

    async fn tool_definitions(&self) -> Vec<crate::agent::llm::ToolDefinition> {
        let registry = self.registry.lock().await;
        registry.tool_definitions()
    }

    async fn execute_tool_call(
        &self,
        tool_call: &crate::agent::llm::ToolCall,
        event_tx: Option<&mpsc::UnboundedSender<RunnerEvent>>,
    ) -> Result<String> {
        let FunctionCall { name, arguments } = &tool_call.function;
        let args: Value = serde_json::from_str(arguments)
            .with_context(|| format!("解析工具参数失败: {}", arguments))?;
        let args_map = args
            .as_object()
            .cloned()
            .unwrap_or_default()
            .into_iter()
            .collect::<std::collections::HashMap<_, _>>();

        if let Some(tx) = event_tx {
            let _ = tx.send(RunnerEvent::ToolCall {
                name: name.clone(),
                arguments: args.clone(),
            });
        }

        info!("执行工具: {} 参数: {}", name, args);
        let registry = self.registry.lock().await;
        let result = registry.execute(name, args_map).await;

        let result_str = match result {
            Ok(r) => {
                let s = r.to_string_for_model();
                if let Some(tx) = event_tx {
                    let _ = tx.send(RunnerEvent::ToolResult {
                        name: name.clone(),
                        result: s.clone(),
                    });
                }
                s
            }
            Err(e) => {
                let err = format!("{}: {}", TOOL_ERROR_PREFIX, e);
                if let Some(tx) = event_tx {
                    let _ = tx.send(RunnerEvent::Error(err.clone()));
                }
                err
            }
        };

        Ok(result_str)
    }
}

/// 单步执行结果
struct StepOutcome {
    /// 步骤是否产出了文本结果（false 表示达到最大迭代次数）
    done: bool,
    /// 执行过程中是否出现过工具错误
    had_error: bool,
}

/// 解析 LLM 返回的计划文本：优先按 JSON 字符串数组解析，其次按编号列表解析。
/// 两种格式都无法识别时返回空列表，由调用方决定回退策略。
fn parse_plan(text: &str) -> Vec<String> {
    let trimmed = text.trim();

    if let (Some(start), Some(end)) = (trimmed.find('['), trimmed.rfind(']'))
        && start < end
        && let Ok(items) = serde_json::from_str::<Vec<String>>(&trimmed[start..=end])
    {
        let steps: Vec<String> = items
            .iter()
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .collect();
        if !steps.is_empty() {
            return steps;
        }
    }

    trimmed
        .lines()
        .filter_map(|line| strip_step_marker(line.trim()))
        .collect()
}

/// 去掉步骤行的编号/符号前缀（如 "1."、"2)"、"3、"、"-"），无法识别时返回 None。
fn strip_step_marker(line: &str) -> Option<String> {
    if line.is_empty() {
        return None;
    }

    for prefix in ["- ", "* "] {
        if let Some(rest) = line.strip_prefix(prefix) {
            let rest = rest.trim();
            return if rest.is_empty() {
                None
            } else {
                Some(rest.to_string())
            };
        }
    }

    let digit_len = line.len() - line.trim_start_matches(|c: char| c.is_ascii_digit()).len();
    if digit_len == 0 {
        return None;
    }
    let rest = &line[digit_len..];
    for sep in [". ", ".", "、", ") ", ")", "：", ": ", ":"] {
        if let Some(rest) = rest.strip_prefix(sep) {
            let rest = rest.trim();
            return if rest.is_empty() {
                None
            } else {
                Some(rest.to_string())
            };
        }
    }
    None
}

fn format_steps(steps: &[String]) -> String {
    steps
        .iter()
        .enumerate()
        .map(|(i, s)| format!("{}. {}", i + 1, s))
        .collect::<Vec<_>>()
        .join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent::llm::client::Role;
    use crate::agent::llm::{LlmResponse, Message, ToolCall, ToolDefinition};
    use crate::tools::registry::ToolRegistry;
    use crate::tools::schema::{Tool, ToolContext, ToolResult, ToolSchema};
    use async_trait::async_trait;
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

    struct ChunkedFakeLlm {
        chunks: Vec<String>,
    }

    #[async_trait]
    impl LlmClient for ChunkedFakeLlm {
        async fn chat(
            &self,
            _messages: Vec<Message>,
            _tools: Vec<ToolDefinition>,
        ) -> Result<LlmResponse> {
            Ok(LlmResponse::Text(self.chunks.join("")))
        }

        async fn chat_stream(
            &self,
            _messages: Vec<Message>,
            _tools: Vec<ToolDefinition>,
        ) -> anyhow::Result<
            Box<dyn tokio_stream::Stream<Item = anyhow::Result<StreamChunk>> + Send + Unpin>,
        > {
            let chunks: Vec<anyhow::Result<StreamChunk>> = self
                .chunks
                .iter()
                .cloned()
                .map(|s| {
                    Ok(StreamChunk {
                        content: Some(s),
                        reasoning_content: None,
                    })
                })
                .collect();
            Ok(Box::new(tokio_stream::iter(chunks)))
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

    fn fake_tool_call(name: &str) -> ToolCall {
        ToolCall {
            id: "1".to_string(),
            call_type: "function".to_string(),
            function: crate::agent::llm::FunctionCall {
                name: name.to_string(),
                arguments: "{}".to_string(),
            },
        }
    }

    fn registry_with_fake_tool() -> ToolRegistry {
        let mut registry = ToolRegistry::new(ToolContext::default());
        registry.register(Arc::new(FakeTool));
        registry
    }

    // ---- 计划解析 ----

    #[test]
    fn test_parse_plan_json_array() {
        let steps = parse_plan(r#"["读取文件", "总结内容"]"#);
        assert_eq!(steps, vec!["读取文件".to_string(), "总结内容".to_string()]);
    }

    #[test]
    fn test_parse_plan_json_with_prose_and_fence() {
        let text = "这是计划：\n```json\n[\"步骤一\", \"步骤二\"]\n```";
        let steps = parse_plan(text);
        assert_eq!(steps, vec!["步骤一".to_string(), "步骤二".to_string()]);
    }

    #[test]
    fn test_parse_plan_numbered() {
        let text = "计划如下：\n1. 读取文件\n2) 分析内容\n3、写入结果\n- 回复用户";
        let steps = parse_plan(text);
        assert_eq!(
            steps,
            vec![
                "读取文件".to_string(),
                "分析内容".to_string(),
                "写入结果".to_string(),
                "回复用户".to_string()
            ]
        );
    }

    #[test]
    fn test_parse_plan_plain_text_returns_empty() {
        assert!(parse_plan("直接回答用户问题").is_empty());
        assert!(parse_plan("").is_empty());
        assert!(parse_plan("[]").is_empty());
    }

    // ---- 计划生成与事件 ----

    #[tokio::test]
    async fn test_run_generates_plan_and_sends_event() {
        let client = Arc::new(FakeLlm {
            responses: Mutex::new(vec![
                LlmResponse::Text(r#"["第一步", "第二步"]"#.to_string()),
                LlmResponse::Text("step1 done".to_string()),
                LlmResponse::Text("step2 done".to_string()),
                LlmResponse::Text("summary".to_string()),
            ]),
        });
        let runner =
            PlanExecuteRunner::new(client, Arc::new(Mutex::new(registry_with_fake_tool())));
        let mut ctx = SessionContext::new("sys");
        let (event_tx, mut event_rx) = tokio::sync::mpsc::unbounded_channel::<RunnerEvent>();

        let result = runner.run(&mut ctx, "do it", Some(event_tx)).await.unwrap();
        assert_eq!(result, "summary");

        let mut plans = Vec::new();
        while let Ok(event) = event_rx.try_recv() {
            if let RunnerEvent::Plan { steps } = event {
                plans.push(steps);
            }
        }
        assert_eq!(plans.len(), 1);
        assert_eq!(plans[0], vec!["第一步".to_string(), "第二步".to_string()]);

        assert!(ctx.messages.iter().any(|m| m.content.contains("执行计划")));
    }

    #[tokio::test]
    async fn test_run_plan_fallback_when_llm_returns_tool_calls() {
        let client = Arc::new(FakeLlm {
            responses: Mutex::new(vec![
                LlmResponse::ToolCalls(vec![fake_tool_call("fake")]),
                LlmResponse::Text("direct answer".to_string()),
                LlmResponse::Text("final".to_string()),
            ]),
        });
        let runner =
            PlanExecuteRunner::new(client, Arc::new(Mutex::new(registry_with_fake_tool())));
        let mut ctx = SessionContext::new("sys");
        let (event_tx, mut event_rx) = tokio::sync::mpsc::unbounded_channel::<RunnerEvent>();

        let result = runner
            .run(&mut ctx, "直接请求", Some(event_tx))
            .await
            .unwrap();
        assert_eq!(result, "final");

        let mut plan_steps = None;
        while let Ok(event) = event_rx.try_recv() {
            if let RunnerEvent::Plan { steps } = event {
                plan_steps = Some(steps);
            }
        }
        assert_eq!(plan_steps, Some(vec!["直接请求".to_string()]));
    }

    // ---- 步骤执行 ----

    #[tokio::test]
    async fn test_run_executes_steps_with_tools() {
        let client = Arc::new(FakeLlm {
            responses: Mutex::new(vec![
                LlmResponse::Text(r#"["调用工具"]"#.to_string()),
                LlmResponse::ToolCalls(vec![fake_tool_call("fake")]),
                LlmResponse::Text("step done".to_string()),
                LlmResponse::Text("result".to_string()),
            ]),
        });
        let runner =
            PlanExecuteRunner::new(client, Arc::new(Mutex::new(registry_with_fake_tool())));
        let mut ctx = SessionContext::new("sys");

        let result = runner.run(&mut ctx, "call", None).await.unwrap();
        assert_eq!(result, "result");

        let tool_msgs: Vec<&Message> = ctx
            .messages
            .iter()
            .filter(|m| matches!(m.role, Role::Tool))
            .collect();
        assert_eq!(tool_msgs.len(), 1);
        assert_eq!(tool_msgs[0].content, "done");
    }

    #[tokio::test]
    async fn test_run_event_channel() {
        let client = Arc::new(FakeLlm {
            responses: Mutex::new(vec![
                LlmResponse::Text(r#"["调用工具"]"#.to_string()),
                LlmResponse::ToolCalls(vec![fake_tool_call("fake")]),
                LlmResponse::Text("step done".to_string()),
                LlmResponse::Text("done".to_string()),
            ]),
        });
        let runner =
            PlanExecuteRunner::new(client, Arc::new(Mutex::new(registry_with_fake_tool())));
        let mut ctx = SessionContext::new("sys");
        let (event_tx, mut event_rx) = tokio::sync::mpsc::unbounded_channel::<RunnerEvent>();

        let result = runner.run(&mut ctx, "call", Some(event_tx)).await.unwrap();
        assert_eq!(result, "done");

        let mut saw_plan = false;
        let mut saw_tool_call = false;
        let mut saw_tool_result = false;
        while let Ok(event) = event_rx.try_recv() {
            match event {
                RunnerEvent::Plan { .. } => saw_plan = true,
                RunnerEvent::ToolCall { name, .. } if name == "fake" => saw_tool_call = true,
                RunnerEvent::ToolResult { name, .. } if name == "fake" => saw_tool_result = true,
                _ => {}
            }
        }
        assert!(saw_plan, "should receive Plan event");
        assert!(saw_tool_call, "should receive ToolCall event");
        assert!(saw_tool_result, "should receive ToolResult event");
    }

    // ---- 重计划 ----

    #[tokio::test]
    async fn test_run_replans_on_tool_error() {
        let client = Arc::new(FakeLlm {
            responses: Mutex::new(vec![
                LlmResponse::Text(r#"["失败步骤", "后续步骤"]"#.to_string()),
                LlmResponse::ToolCalls(vec![fake_tool_call("unknown")]),
                LlmResponse::Text("step1 done with error".to_string()),
                LlmResponse::Text(r#"["新步骤"]"#.to_string()),
                LlmResponse::Text("new step done".to_string()),
                LlmResponse::Text("final".to_string()),
            ]),
        });
        let runner =
            PlanExecuteRunner::new(client, Arc::new(Mutex::new(registry_with_fake_tool())));
        let mut ctx = SessionContext::new("sys");
        let (event_tx, mut event_rx) = tokio::sync::mpsc::unbounded_channel::<RunnerEvent>();

        let result = runner.run(&mut ctx, "call", Some(event_tx)).await.unwrap();
        assert_eq!(result, "final");

        let mut plan_events = 0;
        while let Ok(event) = event_rx.try_recv() {
            if matches!(event, RunnerEvent::Plan { .. }) {
                plan_events += 1;
            }
        }
        assert_eq!(plan_events, 2, "初始计划与重计划各发送一次 Plan 事件");

        let tool_contents: Vec<String> = ctx
            .messages
            .iter()
            .filter(|m| matches!(m.role, Role::Tool))
            .map(|m| m.content.clone())
            .collect();
        assert!(tool_contents.iter().any(|c| c.contains("工具执行失败")));
        assert!(tool_contents.iter().any(|c| c.contains("未知工具")));
        assert!(
            ctx.messages
                .iter()
                .any(|m| m.content.contains("调整后的剩余计划"))
        );
    }

    #[tokio::test]
    async fn test_run_replans_only_once() {
        let client = Arc::new(FakeLlm {
            responses: Mutex::new(vec![
                LlmResponse::Text(r#"["步骤一", "步骤二"]"#.to_string()),
                LlmResponse::ToolCalls(vec![fake_tool_call("unknown")]),
                LlmResponse::Text("step1 done".to_string()),
                LlmResponse::Text(r#"["新步骤二"]"#.to_string()),
                LlmResponse::ToolCalls(vec![fake_tool_call("unknown")]),
                LlmResponse::Text("step2 done".to_string()),
                LlmResponse::Text("final".to_string()),
            ]),
        });
        let runner =
            PlanExecuteRunner::new(client, Arc::new(Mutex::new(registry_with_fake_tool())));
        let mut ctx = SessionContext::new("sys");
        let (event_tx, mut event_rx) = tokio::sync::mpsc::unbounded_channel::<RunnerEvent>();

        let result = runner.run(&mut ctx, "call", Some(event_tx)).await.unwrap();
        assert_eq!(result, "final");

        let mut plan_events = 0;
        while let Ok(event) = event_rx.try_recv() {
            if matches!(event, RunnerEvent::Plan { .. }) {
                plan_events += 1;
            }
        }
        assert_eq!(plan_events, 2, "最多只允许一次重计划");
    }

    #[tokio::test]
    async fn test_run_step_max_iterations_then_empty_replan() {
        let mut responses = vec![LlmResponse::Text(r#"["唯一步骤"]"#.to_string())];
        for _ in 0..3 {
            responses.push(LlmResponse::ToolCalls(vec![fake_tool_call("fake")]));
        }
        responses.push(LlmResponse::Text("[]".to_string()));
        responses.push(LlmResponse::Text("final".to_string()));
        let client = Arc::new(FakeLlm {
            responses: Mutex::new(responses),
        });
        let runner =
            PlanExecuteRunner::new(client, Arc::new(Mutex::new(registry_with_fake_tool())))
                .with_max_iterations(3);
        let mut ctx = SessionContext::new("sys");

        let result = runner.run(&mut ctx, "call", None).await.unwrap();
        assert_eq!(result, "final");
        assert!(
            ctx.messages
                .iter()
                .any(|m| m.content.contains("达到最大工具调用次数限制"))
        );
    }

    // ---- 流式运行 ----

    #[tokio::test]
    async fn test_run_stream_text() {
        let client = Arc::new(ChunkedFakeLlm {
            chunks: vec!["Hello".to_string(), ", ".to_string(), "world!".to_string()],
        });
        let registry = ToolRegistry::new(ToolContext::default());
        let runner = PlanExecuteRunner::new(client, Arc::new(Mutex::new(registry)));
        let ctx = Arc::new(Mutex::new(SessionContext::new("sys")));
        let (chunk_tx, mut chunk_rx) = tokio::sync::mpsc::unbounded_channel::<StreamChunk>();

        let result = runner
            .run_stream(ctx.clone(), "hi", chunk_tx, None)
            .await
            .unwrap();

        assert_eq!(result, "Hello, world!");
        let ctx = ctx.lock().await;
        assert_eq!(ctx.messages[0].role, Role::User);
        assert_eq!(ctx.messages[0].content, "hi");
        let last = ctx.messages.last().unwrap();
        assert_eq!(last.role, Role::Assistant);
        assert_eq!(last.content, "Hello, world!");

        let mut chunks = Vec::new();
        while let Ok(chunk) = chunk_rx.try_recv() {
            chunks.push(chunk);
        }
        let contents: Vec<String> = chunks.into_iter().filter_map(|c| c.content).collect();
        assert_eq!(contents, vec!["Hello", ", ", "world!"]);
    }

    #[tokio::test]
    async fn test_run_stream_executes_tools_before_summary() {
        struct QueueStreamFakeLlm {
            responses: Mutex<Vec<LlmResponse>>,
            chunks: Vec<String>,
        }

        #[async_trait]
        impl LlmClient for QueueStreamFakeLlm {
            async fn chat(
                &self,
                _messages: Vec<Message>,
                _tools: Vec<ToolDefinition>,
            ) -> Result<LlmResponse> {
                let mut responses = self.responses.lock().await;
                Ok(responses.remove(0))
            }

            async fn chat_stream(
                &self,
                _messages: Vec<Message>,
                _tools: Vec<ToolDefinition>,
            ) -> anyhow::Result<
                Box<dyn tokio_stream::Stream<Item = anyhow::Result<StreamChunk>> + Send + Unpin>,
            > {
                let chunks: Vec<anyhow::Result<StreamChunk>> = self
                    .chunks
                    .iter()
                    .cloned()
                    .map(|s| {
                        Ok(StreamChunk {
                            content: Some(s),
                            reasoning_content: None,
                        })
                    })
                    .collect();
                Ok(Box::new(tokio_stream::iter(chunks)))
            }
        }

        let client = Arc::new(QueueStreamFakeLlm {
            responses: Mutex::new(vec![
                LlmResponse::Text(r#"["调用工具"]"#.to_string()),
                LlmResponse::ToolCalls(vec![fake_tool_call("fake")]),
                LlmResponse::Text("step done".to_string()),
            ]),
            chunks: vec!["sum".to_string(), "mary".to_string()],
        });
        let runner =
            PlanExecuteRunner::new(client, Arc::new(Mutex::new(registry_with_fake_tool())));
        let ctx = Arc::new(Mutex::new(SessionContext::new("sys")));
        let (chunk_tx, mut chunk_rx) = tokio::sync::mpsc::unbounded_channel::<StreamChunk>();

        let result = runner
            .run_stream(ctx.clone(), "call", chunk_tx, None)
            .await
            .unwrap();

        assert_eq!(result, "summary");
        let ctx = ctx.lock().await;
        assert!(ctx.messages.iter().any(|m| m.role == Role::Tool));
        drop(ctx);

        let mut contents = Vec::new();
        while let Ok(chunk) = chunk_rx.try_recv() {
            if let Some(c) = chunk.content {
                contents.push(c);
            }
        }
        assert_eq!(contents, vec!["sum", "mary"]);
    }

    #[tokio::test]
    async fn test_run_stream_summary_falls_back_when_stream_unsupported() {
        struct NoStreamFakeLlm;

        #[async_trait]
        impl LlmClient for NoStreamFakeLlm {
            async fn chat(
                &self,
                _messages: Vec<Message>,
                _tools: Vec<ToolDefinition>,
            ) -> Result<LlmResponse> {
                Ok(LlmResponse::Text("plain".to_string()))
            }

            async fn chat_stream(
                &self,
                _messages: Vec<Message>,
                _tools: Vec<ToolDefinition>,
            ) -> anyhow::Result<
                Box<dyn tokio_stream::Stream<Item = anyhow::Result<StreamChunk>> + Send + Unpin>,
            > {
                Err(anyhow::anyhow!("streaming 不支持工具调用"))
            }
        }

        let client = Arc::new(NoStreamFakeLlm);
        let registry = ToolRegistry::new(ToolContext::default());
        let runner = PlanExecuteRunner::new(client, Arc::new(Mutex::new(registry)));
        let ctx = Arc::new(Mutex::new(SessionContext::new("sys")));
        let (chunk_tx, mut chunk_rx) = tokio::sync::mpsc::unbounded_channel::<StreamChunk>();

        let result = runner
            .run_stream(ctx.clone(), "hi", chunk_tx, None)
            .await
            .unwrap();

        assert_eq!(result, "plain");
        let ctx = ctx.lock().await;
        let last = ctx.messages.last().unwrap();
        assert_eq!(last.role, Role::Assistant);
        assert_eq!(last.content, "plain");
        assert!(chunk_rx.try_recv().is_err(), "回退时不应收到流式 chunk");
    }

    #[tokio::test]
    async fn test_run_stream_chunk_error_falls_back() {
        struct MidStreamErrorFakeLlm;

        #[async_trait]
        impl LlmClient for MidStreamErrorFakeLlm {
            async fn chat(
                &self,
                _messages: Vec<Message>,
                _tools: Vec<ToolDefinition>,
            ) -> Result<LlmResponse> {
                Ok(LlmResponse::Text("plain".to_string()))
            }

            async fn chat_stream(
                &self,
                _messages: Vec<Message>,
                _tools: Vec<ToolDefinition>,
            ) -> anyhow::Result<
                Box<dyn tokio_stream::Stream<Item = anyhow::Result<StreamChunk>> + Send + Unpin>,
            > {
                Ok(Box::new(tokio_stream::iter(vec![Err(anyhow::anyhow!(
                    "streaming 不支持工具调用"
                ))])))
            }
        }

        let client = Arc::new(MidStreamErrorFakeLlm);
        let registry = ToolRegistry::new(ToolContext::default());
        let runner = PlanExecuteRunner::new(client, Arc::new(Mutex::new(registry)));
        let ctx = Arc::new(Mutex::new(SessionContext::new("sys")));
        let (chunk_tx, _chunk_rx) = tokio::sync::mpsc::unbounded_channel::<StreamChunk>();

        let result = runner
            .run_stream(ctx.clone(), "hi", chunk_tx, None)
            .await
            .unwrap();

        assert_eq!(result, "plain");
        let ctx = ctx.lock().await;
        let last = ctx.messages.last().unwrap();
        assert_eq!(last.content, "plain");
    }

    #[test]
    fn test_with_max_iterations() {
        let client = Arc::new(FakeLlm {
            responses: Mutex::new(vec![]),
        });
        let registry = ToolRegistry::new(ToolContext::default());
        let runner =
            PlanExecuteRunner::new(client, Arc::new(Mutex::new(registry))).with_max_iterations(42);
        assert_eq!(runner.max_iterations, 42);
    }
}
