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

/// 工具调用事件，用于 TUI 展示
#[derive(Debug, Clone)]
pub enum RunnerEvent {
    ToolCall { name: String, arguments: Value },
    ToolResult { name: String, result: String },
    Error(String),
}

#[derive(Clone)]
pub struct ReActRunner {
    client: Arc<dyn LlmClient>,
    registry: Arc<Mutex<ToolRegistry>>,
    max_iterations: usize,
}

impl ReActRunner {
    pub fn new(client: Arc<dyn LlmClient>, registry: Arc<Mutex<ToolRegistry>>) -> Self {
        Self {
            client,
            registry,
            max_iterations: 10,
        }
    }

    pub fn with_max_iterations(mut self, max: usize) -> Self {
        self.max_iterations = max;
        self
    }

    pub async fn run(
        &self,
        ctx: &mut SessionContext,
        user_input: &str,
        event_tx: Option<tokio::sync::mpsc::UnboundedSender<RunnerEvent>>,
    ) -> Result<String> {
        ctx.add_message(Message::user(user_input));

        for iteration in 0..self.max_iterations {
            info!("ReAct 迭代 {}", iteration + 1);

            let messages = ctx.build_messages();
            let tools = {
                let registry = self.registry.lock().await;
                registry.tool_definitions()
            };

            let response = self.client.chat(messages, tools).await?;

            match response {
                LlmResponse::Text(text) => {
                    ctx.add_message(Message::assistant(text.clone()));
                    return Ok(text);
                }
                LlmResponse::ToolCalls(tool_calls) => {
                    let mut assistant_msg = Message::assistant("".to_string());
                    assistant_msg.tool_calls = Some(tool_calls.clone());
                    ctx.add_message(assistant_msg);

                    for tool_call in tool_calls {
                        let result = self
                            .execute_tool_call(&tool_call, event_tx.as_ref())
                            .await?;
                        ctx.add_message(Message::tool(tool_call.id.clone(), result.clone()));
                    }
                }
            }
        }

        warn!("达到最大迭代次数，返回最后一条消息");
        Ok("达到最大工具调用次数限制，请简化请求。".to_string())
    }

    /// 流式运行：优先使用 LLM 流式输出。若流式响应中出现工具调用或不支持工具调用，
    /// 则回退到非流式的 `run()` 方法。
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
        }

        let messages = {
            let ctx = ctx.lock().await;
            ctx.build_messages()
        };
        let tools = {
            let registry = self.registry.lock().await;
            registry.tool_definitions()
        };

        let mut stream = match self.client.chat_stream(messages, tools).await {
            Ok(s) => s,
            Err(e) if e.to_string().contains("不支持工具调用") => {
                return self.run_fallback(ctx, event_tx).await;
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
                    return self.run_fallback(ctx, event_tx).await;
                }
                Err(e) => return Err(e),
            }
        }

        {
            let mut ctx = ctx.lock().await;
            ctx.add_message(Message::assistant(full.clone()));
        }
        Ok(full)
    }

    async fn run_fallback(
        &self,
        ctx: Arc<Mutex<SessionContext>>,
        event_tx: Option<mpsc::UnboundedSender<RunnerEvent>>,
    ) -> Result<String> {
        let user_input = {
            let mut ctx = ctx.lock().await;
            let last = ctx.messages.pop();
            debug_assert!(
                last.as_ref()
                    .map(|m| matches!(m.role, Role::User))
                    .unwrap_or(true),
                "回退时期望最后一条是用户消息"
            );
            last.map(|m| m.content).unwrap_or_default()
        };
        let mut ctx = ctx.lock().await;
        self.run(&mut ctx, &user_input, event_tx).await
    }

    async fn execute_tool_call(
        &self,
        tool_call: &crate::agent::llm::ToolCall,
        event_tx: Option<&tokio::sync::mpsc::UnboundedSender<RunnerEvent>>,
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
                let err = format!("工具执行失败: {}", e);
                if let Some(tx) = event_tx {
                    let _ = tx.send(RunnerEvent::Error(err.clone()));
                }
                err
            }
        };

        Ok(result_str)
    }
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

    #[tokio::test]
    async fn test_run_text_response() {
        let client = Arc::new(FakeLlm {
            responses: Mutex::new(vec![LlmResponse::Text("hi".to_string())]),
        });
        let mut registry = ToolRegistry::new(ToolContext::default());
        registry.register(Arc::new(FakeTool));
        let runner = ReActRunner::new(client, Arc::new(Mutex::new(registry)));
        let mut ctx = SessionContext::new("sys");

        let result = runner.run(&mut ctx, "hello", None).await.unwrap();
        assert_eq!(result, "hi");
        assert_eq!(ctx.messages.len(), 2);
    }

    #[tokio::test]
    async fn test_run_tool_call() {
        let client = Arc::new(FakeLlm {
            responses: Mutex::new(vec![
                LlmResponse::ToolCalls(vec![ToolCall {
                    id: "1".to_string(),
                    call_type: "function".to_string(),
                    function: crate::agent::llm::FunctionCall {
                        name: "fake".to_string(),
                        arguments: "{}".to_string(),
                    },
                }]),
                LlmResponse::Text("result".to_string()),
            ]),
        });
        let mut registry = ToolRegistry::new(ToolContext::default());
        registry.register(Arc::new(FakeTool));
        let runner = ReActRunner::new(client, Arc::new(Mutex::new(registry)));
        let mut ctx = SessionContext::new("sys");

        let result = runner.run(&mut ctx, "call", None).await.unwrap();
        assert_eq!(result, "result");
    }

    #[tokio::test]
    async fn test_run_max_iterations() {
        let client = Arc::new(FakeLlm {
            responses: Mutex::new(
                (0..3)
                    .map(|_| {
                        LlmResponse::ToolCalls(vec![ToolCall {
                            id: "1".to_string(),
                            call_type: "function".to_string(),
                            function: crate::agent::llm::FunctionCall {
                                name: "fake".to_string(),
                                arguments: "{}".to_string(),
                            },
                        }])
                    })
                    .collect(),
            ),
        });
        let mut registry = ToolRegistry::new(ToolContext::default());
        registry.register(Arc::new(FakeTool));
        let runner =
            ReActRunner::new(client, Arc::new(Mutex::new(registry))).with_max_iterations(3);
        let mut ctx = SessionContext::new("sys");

        let result = runner.run(&mut ctx, "call", None).await.unwrap();
        assert_eq!(result, "达到最大工具调用次数限制，请简化请求。");
    }

    #[tokio::test]
    async fn test_run_tool_failure() {
        let client = Arc::new(FakeLlm {
            responses: Mutex::new(vec![
                LlmResponse::ToolCalls(vec![ToolCall {
                    id: "1".to_string(),
                    call_type: "function".to_string(),
                    function: crate::agent::llm::FunctionCall {
                        name: "unknown".to_string(),
                        arguments: "{}".to_string(),
                    },
                }]),
                LlmResponse::Text("fallback".to_string()),
            ]),
        });
        let registry = ToolRegistry::new(ToolContext::default());
        let runner = ReActRunner::new(client, Arc::new(Mutex::new(registry)));
        let mut ctx = SessionContext::new("sys");

        let result = runner.run(&mut ctx, "call", None).await.unwrap();
        assert_eq!(result, "fallback");

        let tool_contents: Vec<String> = ctx
            .messages
            .iter()
            .filter(|m| matches!(m.role, Role::Tool))
            .map(|m| m.content.clone())
            .collect();
        assert!(tool_contents.iter().any(|c| c.contains("工具执行失败")));
        assert!(tool_contents.iter().any(|c| c.contains("未知工具")));
    }

    #[tokio::test]
    async fn test_run_event_channel() {
        let client = Arc::new(FakeLlm {
            responses: Mutex::new(vec![
                LlmResponse::ToolCalls(vec![ToolCall {
                    id: "1".to_string(),
                    call_type: "function".to_string(),
                    function: crate::agent::llm::FunctionCall {
                        name: "fake".to_string(),
                        arguments: "{}".to_string(),
                    },
                }]),
                LlmResponse::Text("done".to_string()),
            ]),
        });
        let mut registry = ToolRegistry::new(ToolContext::default());
        registry.register(Arc::new(FakeTool));
        let runner = ReActRunner::new(client, Arc::new(Mutex::new(registry)));
        let mut ctx = SessionContext::new("sys");
        let (event_tx, mut event_rx) = tokio::sync::mpsc::unbounded_channel::<RunnerEvent>();

        let result = runner.run(&mut ctx, "call", Some(event_tx)).await.unwrap();
        assert_eq!(result, "done");

        let mut saw_tool_call = false;
        let mut saw_tool_result = false;
        while let Ok(event) = event_rx.try_recv() {
            match event {
                RunnerEvent::ToolCall { name, .. } if name == "fake" => saw_tool_call = true,
                RunnerEvent::ToolResult { name, .. } if name == "fake" => saw_tool_result = true,
                _ => {}
            }
        }
        assert!(saw_tool_call, "should receive ToolCall event");
        assert!(saw_tool_result, "should receive ToolResult event");
    }

    #[tokio::test]
    async fn test_run_stream_text() {
        let client = Arc::new(ChunkedFakeLlm {
            chunks: vec!["Hello".to_string(), ", ".to_string(), "world!".to_string()],
        });
        let registry = ToolRegistry::new(ToolContext::default());
        let runner = ReActRunner::new(client, Arc::new(Mutex::new(registry)));
        let ctx = Arc::new(Mutex::new(SessionContext::new("sys")));
        let (chunk_tx, mut chunk_rx) = tokio::sync::mpsc::unbounded_channel::<StreamChunk>();

        let result = runner
            .run_stream(ctx.clone(), "hi", chunk_tx, None)
            .await
            .unwrap();

        assert_eq!(result, "Hello, world!");
        let ctx = ctx.lock().await;
        assert_eq!(ctx.messages.len(), 2);
        assert_eq!(ctx.messages[0].role, Role::User);
        assert_eq!(ctx.messages[0].content, "hi");
        assert_eq!(ctx.messages[1].role, Role::Assistant);
        assert_eq!(ctx.messages[1].content, "Hello, world!");

        let mut chunks = Vec::new();
        while let Ok(chunk) = chunk_rx.try_recv() {
            chunks.push(chunk);
        }
        let contents: Vec<String> = chunks.into_iter().filter_map(|c| c.content).collect();
        assert_eq!(contents, vec!["Hello", ", ", "world!"]);
    }

    #[tokio::test]
    async fn test_run_stream_fallback_to_run_on_tool_call() {
        struct ToolCallStreamFakeLlm;

        #[async_trait]
        impl LlmClient for ToolCallStreamFakeLlm {
            async fn chat(
                &self,
                _messages: Vec<Message>,
                _tools: Vec<ToolDefinition>,
            ) -> Result<LlmResponse> {
                Ok(LlmResponse::ToolCalls(vec![ToolCall {
                    id: "1".to_string(),
                    call_type: "function".to_string(),
                    function: crate::agent::llm::FunctionCall {
                        name: "fake".to_string(),
                        arguments: "{}".to_string(),
                    },
                }]))
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

        let client = Arc::new(ToolCallStreamFakeLlm);
        let mut registry = ToolRegistry::new(ToolContext::default());
        registry.register(Arc::new(FakeTool));
        let runner = ReActRunner::new(client, Arc::new(Mutex::new(registry)));
        let ctx = Arc::new(Mutex::new(SessionContext::new("sys")));
        let (chunk_tx, _chunk_rx) = tokio::sync::mpsc::unbounded_channel::<StreamChunk>();

        let result = runner
            .run_stream(ctx.clone(), "call", chunk_tx, None)
            .await
            .unwrap();

        assert_eq!(result, "达到最大工具调用次数限制，请简化请求。");
        let ctx = ctx.lock().await;
        assert!(ctx.messages.iter().any(|m| m.role == Role::Tool));
    }

    #[tokio::test]
    async fn test_run_stream_error_from_chat_stream_falls_back() {
        struct ToolCallStreamFakeLlm;

        #[async_trait]
        impl LlmClient for ToolCallStreamFakeLlm {
            async fn chat(
                &self,
                _messages: Vec<Message>,
                _tools: Vec<ToolDefinition>,
            ) -> Result<LlmResponse> {
                Ok(LlmResponse::ToolCalls(vec![ToolCall {
                    id: "1".to_string(),
                    call_type: "function".to_string(),
                    function: crate::agent::llm::FunctionCall {
                        name: "fake".to_string(),
                        arguments: "{}".to_string(),
                    },
                }]))
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

        let client = Arc::new(ToolCallStreamFakeLlm);
        let mut registry = ToolRegistry::new(ToolContext::default());
        registry.register(Arc::new(FakeTool));
        let runner = ReActRunner::new(client, Arc::new(Mutex::new(registry)));
        let ctx = Arc::new(Mutex::new(SessionContext::new("sys")));
        let (chunk_tx, _chunk_rx) = tokio::sync::mpsc::unbounded_channel::<StreamChunk>();

        let result = runner
            .run_stream(ctx.clone(), "call", chunk_tx, None)
            .await
            .unwrap();
        assert_eq!(result, "达到最大工具调用次数限制，请简化请求。");
    }

    #[tokio::test]
    async fn test_run_stream_chunk_error_falls_back() {
        struct ToolCallChunkFakeLlm;

        #[async_trait]
        impl LlmClient for ToolCallChunkFakeLlm {
            async fn chat(
                &self,
                _messages: Vec<Message>,
                _tools: Vec<ToolDefinition>,
            ) -> Result<LlmResponse> {
                Ok(LlmResponse::ToolCalls(vec![ToolCall {
                    id: "1".to_string(),
                    call_type: "function".to_string(),
                    function: crate::agent::llm::FunctionCall {
                        name: "fake".to_string(),
                        arguments: "{}".to_string(),
                    },
                }]))
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

        let client = Arc::new(ToolCallChunkFakeLlm);
        let mut registry = ToolRegistry::new(ToolContext::default());
        registry.register(Arc::new(FakeTool));
        let runner = ReActRunner::new(client, Arc::new(Mutex::new(registry)));
        let ctx = Arc::new(Mutex::new(SessionContext::new("sys")));
        let (chunk_tx, _chunk_rx) = tokio::sync::mpsc::unbounded_channel::<StreamChunk>();

        let result = runner
            .run_stream(ctx.clone(), "call", chunk_tx, None)
            .await
            .unwrap();
        assert_eq!(result, "达到最大工具调用次数限制，请简化请求。");
        let ctx = ctx.lock().await;
        assert!(ctx.messages.iter().any(|m| m.role == Role::Tool));
    }

    #[test]
    fn test_with_max_iterations() {
        let client = Arc::new(FakeLlm {
            responses: Mutex::new(vec![]),
        });
        let registry = ToolRegistry::new(ToolContext::default());
        let runner =
            ReActRunner::new(client, Arc::new(Mutex::new(registry))).with_max_iterations(42);
        assert_eq!(runner.max_iterations, 42);
    }
}
