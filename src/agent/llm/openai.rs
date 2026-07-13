use anyhow::{Context, Result};
use async_openai::{
    Client,
    config::OpenAIConfig,
    types::{
        ChatCompletionRequestMessage, ChatCompletionRequestSystemMessage,
        ChatCompletionRequestToolMessage, ChatCompletionRequestUserMessage,
        ChatCompletionRequestUserMessageContent, ChatCompletionTool, ChatCompletionToolType,
        CreateChatCompletionRequestArgs, FunctionObject,
    },
};
use async_trait::async_trait;
use futures_util::StreamExt;
use std::time::Duration;
use tracing::{debug, info};

use crate::agent::llm::client::{
    ChatStream, FunctionCall, LlmClient, LlmResponse, Message, Role as ClerkRole, StreamChunk,
    ToolCall, ToolDefinition,
};

pub struct OpenAiClient {
    client: Client<OpenAIConfig>,
    api_key: String,
    api_base: String,
    model: String,
    timeout: Duration,
    temperature: f32,
}

impl OpenAiClient {
    pub fn new(
        base_url: impl Into<String>,
        api_key: impl Into<String>,
        model: impl Into<String>,
        timeout_seconds: u64,
        temperature: f32,
    ) -> Result<Self> {
        let api_key = api_key.into();
        let base_url = base_url.into();
        let config = OpenAIConfig::new()
            .with_api_key(api_key.clone())
            .with_api_base(base_url.clone());

        Ok(Self {
            client: Client::with_config(config),
            api_key,
            api_base: base_url,
            model: model.into(),
            timeout: Duration::from_secs(timeout_seconds.max(5)),
            temperature,
        })
    }

    pub fn from_config(config: &crate::config::LlmConfig) -> Result<Self> {
        Self::new(
            config.base_url.clone(),
            config.api_key.clone(),
            config.model.clone(),
            config.timeout_seconds,
            config.temperature,
        )
    }

    fn convert_message(msg: Message) -> ChatCompletionRequestMessage {
        match msg.role {
            ClerkRole::System => ChatCompletionRequestSystemMessage {
                content: async_openai::types::ChatCompletionRequestSystemMessageContent::Text(
                    msg.content,
                ),
                name: None,
            }
            .into(),
            ClerkRole::User => ChatCompletionRequestUserMessage {
                content: ChatCompletionRequestUserMessageContent::Text(msg.content),
                name: None,
            }
            .into(),
            ClerkRole::Assistant => {
                #[allow(deprecated)]
                let mut assistant_msg =
                    async_openai::types::ChatCompletionRequestAssistantMessage {
                        content: Some(
                            async_openai::types::ChatCompletionRequestAssistantMessageContent::Text(
                                msg.content,
                            ),
                        ),
                        name: None,
                        tool_calls: None,
                        function_call: None,
                        refusal: None,
                        audio: None,
                    };
                if let Some(calls) = msg.tool_calls {
                    assistant_msg.tool_calls = Some(
                        calls
                            .into_iter()
                            .map(|c| async_openai::types::ChatCompletionMessageToolCall {
                                id: c.id,
                                function: async_openai::types::FunctionCall {
                                    name: c.function.name,
                                    arguments: c.function.arguments,
                                },
                                r#type: ChatCompletionToolType::Function,
                            })
                            .collect(),
                    );
                }
                assistant_msg.into()
            }
            ClerkRole::Tool => ChatCompletionRequestToolMessage {
                content: msg.content.into(),
                tool_call_id: msg.tool_call_id.unwrap_or_default(),
            }
            .into(),
        }
    }

    fn convert_tool(tool: ToolDefinition) -> ChatCompletionTool {
        ChatCompletionTool {
            r#type: ChatCompletionToolType::Function,
            function: FunctionObject {
                name: tool.function.name,
                description: Some(tool.function.description),
                parameters: Some(tool.function.parameters),
                strict: None,
            },
        }
    }

    fn extract_response(choice: async_openai::types::ChatChoice) -> LlmResponse {
        if let Some(calls) = choice.message.tool_calls
            && !calls.is_empty()
        {
            let tool_calls = calls
                .into_iter()
                .map(|c| ToolCall {
                    id: c.id,
                    call_type: "function".to_string(),
                    function: FunctionCall {
                        name: c.function.name,
                        arguments: c.function.arguments,
                    },
                })
                .collect();
            return LlmResponse::ToolCalls(tool_calls);
        }

        let content = choice.message.content.unwrap_or_default();
        LlmResponse::Text(content)
    }
}

#[async_trait]
impl LlmClient for OpenAiClient {
    async fn chat(
        &self,
        messages: Vec<Message>,
        tools: Vec<ToolDefinition>,
    ) -> Result<LlmResponse> {
        if self.api_key.is_empty() {
            return Ok(LlmResponse::Text(
                "LLM API key 未配置，无法调用模型。".to_string(),
            ));
        }

        let messages: Vec<ChatCompletionRequestMessage> =
            messages.into_iter().map(Self::convert_message).collect();
        let tools: Vec<ChatCompletionTool> = tools.into_iter().map(Self::convert_tool).collect();

        let mut builder = CreateChatCompletionRequestArgs::default();
        builder.model(self.model.clone());
        builder.messages(messages);
        if !tools.is_empty() {
            builder.tools(tools);
        }
        builder.temperature(self.temperature);

        let request = builder.build().context("构建 LLM 请求失败")?;

        debug!(
            "发送 LLM 请求: {}",
            serde_json::to_string_pretty(&request).unwrap_or_default()
        );

        let response = tokio::time::timeout(self.timeout, self.client.chat().create(request))
            .await
            .context("LLM 请求超时")?
            .with_context(|| {
                format!(
                    "请求 LLM 失败 (base_url={}, model={})",
                    self.api_base, self.model
                )
            })?;

        let choice = response
            .choices
            .into_iter()
            .next()
            .context("LLM 响应为空")?;

        Ok(Self::extract_response(choice))
    }

    async fn chat_stream(
        &self,
        messages: Vec<Message>,
        tools: Vec<ToolDefinition>,
    ) -> Result<ChatStream> {
        if self.api_key.is_empty() {
            let text = "LLM API key 未配置，无法调用模型。".to_string();
            return Ok(Box::new(tokio_stream::iter(vec![Ok(StreamChunk {
                content: Some(text),
                reasoning_content: None,
            })])));
        }

        let messages: Vec<ChatCompletionRequestMessage> =
            messages.into_iter().map(Self::convert_message).collect();
        let tools: Vec<ChatCompletionTool> = tools.into_iter().map(Self::convert_tool).collect();

        let request_body = serde_json::json!({
            "model": self.model,
            "messages": messages,
            "tools": tools,
            "temperature": self.temperature,
            "stream": true,
            "stream_options": {
                "include_usage": true
            }
        });

        info!(
            "LLM 流式请求超时: {} 秒，目标: {}",
            self.timeout.as_secs(),
            self.api_base
        );
        debug!(
            "发送 LLM 流式请求: {}",
            serde_json::to_string_pretty(&request_body).unwrap_or_default()
        );

        let http_client = reqwest::Client::builder()
            .timeout(self.timeout)
            .build()
            .context("构建 HTTP 客户端失败")?;

        let url = if self.api_base.ends_with('/') {
            format!("{}chat/completions", self.api_base)
        } else {
            format!("{}/chat/completions", self.api_base)
        };

        let response = tokio::time::timeout(
            self.timeout,
            http_client
                .post(&url)
                .header("Authorization", format!("Bearer {}", self.api_key))
                .header("Content-Type", "application/json")
                .json(&request_body)
                .send(),
        )
        .await
        .context("LLM 流式请求超时")?
        .with_context(|| {
            format!(
                "请求 LLM 流式失败 (base_url={}, model={})",
                self.api_base, self.model
            )
        })?;

        if !response.status().is_success() {
            let status = response.status();
            let body = response
                .text()
                .await
                .unwrap_or_else(|_| "无法读取错误响应".to_string());
            anyhow::bail!("请求 LLM 流式失败 (status={}): {}", status, body);
        }

        let byte_stream = response.bytes_stream();
        let (tx, rx) = tokio::sync::mpsc::unbounded_channel::<anyhow::Result<StreamChunk>>();

        tokio::spawn(async move {
            let mut content_buffer = String::new();
            let mut reasoning_buffer = String::new();
            let mut line_buffer = String::new();
            let mut byte_stream = std::pin::pin!(byte_stream);

            while let Some(chunk) = byte_stream.next().await {
                let chunk = match chunk {
                    Ok(c) => c,
                    Err(e) => {
                        let _ = tx.send(Err(anyhow::anyhow!(e)));
                        break;
                    }
                };
                let text = String::from_utf8_lossy(&chunk);
                line_buffer.push_str(&text);

                while let Some((line, rest)) = line_buffer.split_once('\n') {
                    let line = line.trim().to_string();
                    line_buffer = rest.to_string();
                    if line.is_empty() {
                        continue;
                    }
                    if !line.starts_with("data: ") {
                        continue;
                    }
                    let data = &line["data: ".len()..];
                    if data == "[DONE]" {
                        break;
                    }

                    let json: serde_json::Value = match serde_json::from_str(data) {
                        Ok(j) => j,
                        Err(e) => {
                            let _ = tx.send(Err(anyhow::anyhow!("解析 SSE 数据失败: {}", e)));
                            break;
                        }
                    };
                    let choices = json
                        .get("choices")
                        .and_then(|c| c.as_array())
                        .cloned()
                        .unwrap_or_default();

                    for choice in choices {
                        let delta = choice.get("delta").cloned().unwrap_or_default();

                        if let Some(calls) = delta.get("tool_calls").and_then(|c| c.as_array())
                            && !calls.is_empty()
                        {
                            if !content_buffer.is_empty() || !reasoning_buffer.is_empty() {
                                let _ = tx.send(Ok(StreamChunk {
                                    content: Some(std::mem::take(&mut content_buffer))
                                        .filter(|s| !s.is_empty()),
                                    reasoning_content: Some(std::mem::take(&mut reasoning_buffer))
                                        .filter(|s| !s.is_empty()),
                                }));
                            }
                            let _ = tx.send(Err(anyhow::anyhow!("streaming 不支持工具调用")));
                            return;
                        }

                        if let Some(reasoning) =
                            delta.get("reasoning_content").and_then(|r| r.as_str())
                        {
                            reasoning_buffer.push_str(reasoning);
                        }
                        if let Some(content) = delta.get("content").and_then(|c| c.as_str()) {
                            content_buffer.push_str(content);
                        }

                        if !content_buffer.is_empty() && !reasoning_buffer.is_empty() {
                            let _ = tx.send(Ok(StreamChunk {
                                content: None,
                                reasoning_content: Some(std::mem::take(&mut reasoning_buffer)),
                            }));
                        }
                    }

                    if !content_buffer.is_empty() || !reasoning_buffer.is_empty() {
                        let _ = tx.send(Ok(StreamChunk {
                            content: Some(std::mem::take(&mut content_buffer))
                                .filter(|s| !s.is_empty()),
                            reasoning_content: Some(std::mem::take(&mut reasoning_buffer))
                                .filter(|s| !s.is_empty()),
                        }));
                    }
                }
            }
        });

        Ok(Box::new(
            tokio_stream::wrappers::UnboundedReceiverStream::new(rx),
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_empty_api_key_returns_notice() {
        let client = OpenAiClient::new("http://localhost", "", "gpt-4o-mini", 30, 0.7_f32).unwrap();
        assert!(client.api_key.is_empty());
    }

    #[tokio::test]
    async fn test_convert_and_extract() {
        let _client =
            OpenAiClient::new("http://localhost", "sk-test", "gpt-4o-mini", 30, 0.7_f32).unwrap();

        let msg = Message::user("hello");
        let converted = OpenAiClient::convert_message(msg);
        match converted {
            ChatCompletionRequestMessage::User(_) => {}
            _ => panic!("期望 User 消息"),
        }
    }

    #[test]
    fn test_convert_system_message() {
        let msg = Message::system("sys");
        let converted = OpenAiClient::convert_message(msg);
        assert!(matches!(converted, ChatCompletionRequestMessage::System(_)));
    }

    #[test]
    fn test_convert_tool_message() {
        let msg = Message::tool("1", "result");
        let converted = OpenAiClient::convert_message(msg);
        assert!(matches!(converted, ChatCompletionRequestMessage::Tool(_)));
    }

    #[test]
    fn test_convert_assistant_message() {
        let msg = Message::assistant("reply");
        let converted = OpenAiClient::convert_message(msg);
        assert!(matches!(
            converted,
            ChatCompletionRequestMessage::Assistant(_)
        ));
    }

    #[test]
    fn test_convert_assistant_with_tool_calls() {
        let mut msg = Message::assistant("");
        msg.tool_calls = Some(vec![ToolCall {
            id: "1".to_string(),
            call_type: "function".to_string(),
            function: FunctionCall {
                name: "fake".to_string(),
                arguments: "{}".to_string(),
            },
        }]);
        let converted = OpenAiClient::convert_message(msg);
        assert!(matches!(
            converted,
            ChatCompletionRequestMessage::Assistant(_)
        ));
    }

    #[test]
    fn test_convert_tool() {
        let tool = ToolDefinition {
            tool_type: "function".to_string(),
            function: crate::agent::llm::FunctionDefinition {
                name: "read".to_string(),
                description: "read".to_string(),
                parameters: serde_json::json!({}),
            },
        };
        let converted = OpenAiClient::convert_tool(tool);
        assert!(matches!(converted.r#type, ChatCompletionToolType::Function));
    }

    #[test]
    fn test_extract_response_text() {
        let choice = async_openai::types::ChatChoice {
            index: 0,
            message: async_openai::types::ChatCompletionResponseMessage {
                content: Some("hello".to_string()),
                role: async_openai::types::Role::Assistant,
                tool_calls: None,
                function_call: None,
                refusal: None,
                audio: None,
            },
            logprobs: None,
            finish_reason: None,
        };
        match OpenAiClient::extract_response(choice) {
            LlmResponse::Text(t) => assert_eq!(t, "hello"),
            _ => panic!("期望 Text 响应"),
        }
    }

    #[test]
    fn test_extract_response_tool_calls() {
        let choice = async_openai::types::ChatChoice {
            index: 0,
            message: async_openai::types::ChatCompletionResponseMessage {
                content: None,
                role: async_openai::types::Role::Assistant,
                tool_calls: Some(vec![async_openai::types::ChatCompletionMessageToolCall {
                    id: "1".to_string(),
                    r#type: ChatCompletionToolType::Function,
                    function: async_openai::types::FunctionCall {
                        name: "fake".to_string(),
                        arguments: "{}".to_string(),
                    },
                }]),
                function_call: None,
                refusal: None,
                audio: None,
            },
            logprobs: None,
            finish_reason: None,
        };
        match OpenAiClient::extract_response(choice) {
            LlmResponse::ToolCalls(calls) => {
                assert_eq!(calls.len(), 1);
                assert_eq!(calls[0].function.name, "fake");
            }
            _ => panic!("期望 ToolCalls 响应"),
        }
    }

    #[tokio::test]
    async fn test_chat_with_empty_key() {
        let client = OpenAiClient::new("http://localhost", "", "gpt-4o-mini", 30, 0.7_f32).unwrap();
        let result = client.chat(vec![], vec![]).await.unwrap();
        match result {
            LlmResponse::Text(t) => assert!(t.contains("API key 未配置")),
            _ => panic!("期望提示文本"),
        }
    }

    #[tokio::test]
    async fn test_chat_with_wiremock_success() {
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let server = MockServer::start().await;
        let body = serde_json::json!({
            "id": "chatcmpl-test",
            "object": "chat.completion",
            "created": 1700000000,
            "model": "gpt-4o-mini",
            "choices": [
                {
                    "index": 0,
                    "message": {
                        "role": "assistant",
                        "content": "hello from mock"
                    },
                    "finish_reason": "stop"
                }
            ],
            "usage": {
                "prompt_tokens": 10,
                "completion_tokens": 3,
                "total_tokens": 13
            }
        });

        Mock::given(method("POST"))
            .and(path("/chat/completions"))
            .respond_with(
                ResponseTemplate::new(200)
                    .insert_header("content-type", "application/json")
                    .set_body_json(body),
            )
            .mount(&server)
            .await;

        let client =
            OpenAiClient::new(server.uri(), "sk-test", "gpt-4o-mini", 30, 0.7_f32).unwrap();

        let result = client
            .chat(vec![Message::user("hi")], vec![])
            .await
            .unwrap();

        match result {
            LlmResponse::Text(t) => assert_eq!(t, "hello from mock"),
            _ => panic!("期望 Text 响应"),
        }
    }

    #[tokio::test]
    async fn test_chat_stream_with_wiremock() {
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let server = MockServer::start().await;
        let chunk1 = serde_json::json!({
            "id": "chatcmpl-test",
            "object": "chat.completion.chunk",
            "created": 1700000000,
            "model": "gpt-4o-mini",
            "choices": [
                {
                    "index": 0,
                    "delta": { "role": "assistant", "content": "Hello" },
                    "finish_reason": null
                }
            ]
        });
        let chunk2 = serde_json::json!({
            "id": "chatcmpl-test",
            "object": "chat.completion.chunk",
            "created": 1700000000,
            "model": "gpt-4o-mini",
            "choices": [
                {
                    "index": 0,
                    "delta": { "content": " world" },
                    "finish_reason": null
                }
            ]
        });
        let chunk3 = serde_json::json!({
            "id": "chatcmpl-test",
            "object": "chat.completion.chunk",
            "created": 1700000000,
            "model": "gpt-4o-mini",
            "choices": [
                {
                    "index": 0,
                    "delta": {},
                    "finish_reason": "stop"
                }
            ]
        });
        let body = format!(
            "data: {}\n\ndata: {}\n\ndata: {}\n\ndata: [DONE]\n\n",
            chunk1, chunk2, chunk3
        );

        Mock::given(method("POST"))
            .and(path("/chat/completions"))
            .respond_with(
                ResponseTemplate::new(200)
                    .insert_header("content-type", "text/event-stream")
                    .set_body_bytes(body.into_bytes()),
            )
            .mount(&server)
            .await;

        let client =
            OpenAiClient::new(server.uri(), "sk-test", "gpt-4o-mini", 30, 0.7_f32).unwrap();
        let mut stream = client
            .chat_stream(vec![Message::user("hi")], vec![])
            .await
            .unwrap();

        let mut chunks = Vec::new();
        while let Some(chunk) = stream.next().await {
            chunks.push(chunk.unwrap());
        }
        let text: String = chunks.into_iter().filter_map(|c| c.content).collect();
        assert_eq!(text, "Hello world");
    }

    #[test]
    fn test_timeout_minimum() {
        let client = OpenAiClient::new("http://localhost", "sk", "m", 1, 0.7_f32).unwrap();
        assert_eq!(client.timeout, Duration::from_secs(5));
    }
}
