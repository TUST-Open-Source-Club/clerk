use serde::{Deserialize, Serialize};
use std::collections::HashMap;

/// 流式输出块，可能同时包含模型思考内容和正式回复内容。
#[derive(Debug, Clone, Default)]
pub struct StreamChunk {
    pub content: Option<String>,
    pub reasoning_content: Option<String>,
}

/// 流式输出类型别名
pub type ChatStream =
    Box<dyn tokio_stream::Stream<Item = anyhow::Result<StreamChunk>> + Send + Unpin>;

/// 消息角色：系统、用户、助手或工具结果。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "lowercase")]
pub enum Role {
    System,
    User,
    Assistant,
    Tool,
}

impl std::fmt::Display for Role {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Role::System => write!(f, "system"),
            Role::User => write!(f, "user"),
            Role::Assistant => write!(f, "assistant"),
            Role::Tool => write!(f, "tool"),
        }
    }
}

/// 对话消息：角色 + 文本内容，可选携带工具调用或工具调用 ID。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Message {
    pub role: Role,
    pub content: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_calls: Option<Vec<ToolCall>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_call_id: Option<String>,
}

impl Message {
    pub fn system(content: impl Into<String>) -> Self {
        Self {
            role: Role::System,
            content: content.into(),
            tool_calls: None,
            tool_call_id: None,
        }
    }

    pub fn user(content: impl Into<String>) -> Self {
        Self {
            role: Role::User,
            content: content.into(),
            tool_calls: None,
            tool_call_id: None,
        }
    }

    pub fn assistant(content: impl Into<String>) -> Self {
        Self {
            role: Role::Assistant,
            content: content.into(),
            tool_calls: None,
            tool_call_id: None,
        }
    }

    pub fn tool(tool_call_id: impl Into<String>, content: impl Into<String>) -> Self {
        Self {
            role: Role::Tool,
            content: content.into(),
            tool_calls: None,
            tool_call_id: Some(tool_call_id.into()),
        }
    }
}

/// 一次工具调用记录（OpenAI 格式）。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolCall {
    pub id: String,
    #[serde(rename = "type")]
    pub call_type: String,
    pub function: FunctionCall,
}

/// 函数调用：名称 + JSON 字符串参数。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FunctionCall {
    pub name: String,
    pub arguments: String,
}

/// 提供给 LLM 的工具定义（type = function）。
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct ToolDefinition {
    #[serde(rename = "type")]
    pub tool_type: String,
    pub function: FunctionDefinition,
}

/// 工具函数的元数据：名称、描述与参数 JSON Schema。
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct FunctionDefinition {
    pub name: String,
    pub description: String,
    pub parameters: serde_json::Value,
}

/// Chat Completion 请求体。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChatCompletionRequest {
    pub model: String,
    pub messages: Vec<Message>,
    #[serde(skip_serializing_if = "Vec::is_empty", default)]
    pub tools: Vec<ToolDefinition>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_choice: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub temperature: Option<f32>,
}

/// Chat Completion 响应体。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChatCompletionResponse {
    pub id: String,
    pub object: String,
    pub created: i64,
    pub model: String,
    pub choices: Vec<Choice>,
    pub usage: Option<Usage>,
}

/// 响应中的一个候选消息。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Choice {
    pub index: i32,
    pub message: Message,
    pub finish_reason: Option<String>,
}

/// token 用量统计。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Usage {
    pub prompt_tokens: i32,
    pub completion_tokens: i32,
    pub total_tokens: i32,
}

/// 统一的 LLM 响应内容
#[derive(Debug, Clone)]
pub enum LlmResponse {
    Text(String),
    ToolCalls(Vec<ToolCall>),
}

/// LLM 客户端抽象
#[async_trait::async_trait]
pub trait LlmClient: Send + Sync {
    async fn chat(
        &self,
        messages: Vec<Message>,
        tools: Vec<ToolDefinition>,
    ) -> anyhow::Result<LlmResponse>;

    /// 流式聊天。默认实现退化为 `chat()`，一次性返回完整文本，
    /// 因此 mock/测试客户端无需额外实现。
    async fn chat_stream(
        &self,
        messages: Vec<Message>,
        tools: Vec<ToolDefinition>,
    ) -> anyhow::Result<ChatStream> {
        match self.chat(messages, tools).await? {
            LlmResponse::Text(text) => Ok(Box::new(tokio_stream::iter(vec![Ok(StreamChunk {
                content: Some(text),
                reasoning_content: None,
            })]))),
            LlmResponse::ToolCalls(_) => Err(anyhow::anyhow!("streaming 不支持工具调用")),
        }
    }
}

/// 从 FunctionCall 参数解析 JSON
impl FunctionCall {
    pub fn parse_arguments(&self) -> anyhow::Result<HashMap<String, serde_json::Value>> {
        let args: HashMap<String, serde_json::Value> = serde_json::from_str(&self.arguments)?;
        Ok(args)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_message_builder() {
        let m = Message::user("hello");
        assert_eq!(m.role.to_string(), "user");
        assert_eq!(m.content, "hello");
    }

    #[test]
    fn test_parse_arguments() {
        let fc = FunctionCall {
            name: "read_file".to_string(),
            arguments: r#"{"path": "/tmp/test.txt"}"#.to_string(),
        };
        let args = fc.parse_arguments().unwrap();
        assert_eq!(args.get("path").unwrap().as_str().unwrap(), "/tmp/test.txt");
    }
}
