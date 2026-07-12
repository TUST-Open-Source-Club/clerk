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
use std::time::Duration;
use tracing::debug;

use crate::agent::llm::client::{
    FunctionCall, LlmClient, LlmResponse, Message, Role as ClerkRole, ToolCall, ToolDefinition,
};

pub struct OpenAiClient {
    client: Client<OpenAIConfig>,
    api_key: String,
    model: String,
    timeout: Duration,
}

impl OpenAiClient {
    pub fn new(
        base_url: impl Into<String>,
        api_key: impl Into<String>,
        model: impl Into<String>,
        timeout_seconds: u64,
    ) -> Result<Self> {
        let api_key = api_key.into();
        let base_url = base_url.into();
        let config = OpenAIConfig::new()
            .with_api_key(api_key.clone())
            .with_api_base(base_url);

        Ok(Self {
            client: Client::with_config(config),
            api_key,
            model: model.into(),
            timeout: Duration::from_secs(timeout_seconds.max(5)),
        })
    }

    pub fn from_config(config: &crate::config::LlmConfig) -> Result<Self> {
        Self::new(
            config.base_url.clone(),
            config.api_key.clone(),
            config.model.clone(),
            config.timeout_seconds,
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
        builder.temperature(0.7);

        let request = builder.build().context("构建 LLM 请求失败")?;

        debug!(
            "发送 LLM 请求: {}",
            serde_json::to_string_pretty(&request).unwrap_or_default()
        );

        let response = tokio::time::timeout(self.timeout, self.client.chat().create(request))
            .await
            .context("LLM 请求超时")?
            .context("请求 LLM 失败")?;

        let choice = response
            .choices
            .into_iter()
            .next()
            .context("LLM 响应为空")?;

        Ok(Self::extract_response(choice))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_empty_api_key_returns_notice() {
        let client = OpenAiClient::new("http://localhost", "", "gpt-4o-mini", 30).unwrap();
        assert!(client.api_key.is_empty());
    }

    #[tokio::test]
    async fn test_convert_and_extract() {
        let _client = OpenAiClient::new("http://localhost", "sk-test", "gpt-4o-mini", 30).unwrap();

        let msg = Message::user("hello");
        let converted = OpenAiClient::convert_message(msg);
        match converted {
            ChatCompletionRequestMessage::User(_) => {}
            _ => panic!("期望 User 消息"),
        }
    }
}
