use anyhow::{Context, Result};
use std::sync::atomic::{AtomicU64, Ordering};
use tracing::{info, warn};

use crate::mcp::transport::{StdioTransport, Transport};
use crate::mcp::types::{
    CallToolParams, CallToolResult, ClientCapabilities, ImplementationInfo, InitializeParams,
    InitializeResult, JsonRpcRequest, JsonRpcResponse, McpTool,
};

/// MCP 客户端：基于 JSON-RPC 与 MCP server 通信，负责 initialize 握手与 tools 调用。
pub struct McpClient {
    transport: Box<dyn Transport>,
    next_id: AtomicU64,
}

impl McpClient {
    /// 基于指定传输层创建客户端。
    pub fn new(transport: Box<dyn Transport>) -> Self {
        Self {
            transport,
            next_id: AtomicU64::new(1),
        }
    }

    /// 通过 stdio 启动 MCP server 子进程并完成 initialize 握手。
    pub async fn connect_stdio(command: &str, args: &[String]) -> Result<Self> {
        let transport = StdioTransport::spawn(command, args).await?;
        let mut client = Self::new(Box::new(transport));
        client.initialize().await?;
        Ok(client)
    }

    fn next_id(&self) -> u64 {
        self.next_id.fetch_add(1, Ordering::SeqCst)
    }

    /// MCP initialize 握手：声明协议版本与客户端信息。
    pub async fn initialize(&mut self) -> Result<InitializeResult> {
        let params = InitializeParams {
            protocol_version: "2024-11-05".to_string(),
            capabilities: ClientCapabilities::default(),
            client_info: ImplementationInfo {
                name: "clerk".to_string(),
                version: env!("CARGO_PKG_VERSION").to_string(),
            },
        };

        let request = JsonRpcRequest::new(
            self.next_id(),
            "initialize",
            Some(serde_json::to_value(params)?),
        );
        let response = self.request(request).await?;
        let init_result: InitializeResult =
            serde_json::from_value(response).context("解析 initialize 响应失败")?;

        info!(
            "MCP server 初始化成功: {} {}",
            init_result.server_info.name, init_result.server_info.version
        );
        Ok(init_result)
    }

    /// 获取 MCP server 提供的工具列表。
    pub async fn list_tools(&mut self) -> Result<Vec<McpTool>> {
        let request = JsonRpcRequest::new(self.next_id(), "tools/list", None);
        let response = self.request(request).await?;
        let tools = response
            .get("tools")
            .and_then(|v| v.as_array())
            .cloned()
            .unwrap_or_default();
        let tools: Vec<McpTool> = tools
            .into_iter()
            .map(serde_json::from_value)
            .collect::<Result<_, _>>()
            .context("解析 tools/list 响应失败")?;
        Ok(tools)
    }

    /// 调用 MCP server 上的工具。
    pub async fn call_tool(
        &mut self,
        name: &str,
        arguments: serde_json::Value,
    ) -> Result<CallToolResult> {
        let params = CallToolParams {
            name: name.to_string(),
            arguments,
        };
        let request = JsonRpcRequest::new(
            self.next_id(),
            "tools/call",
            Some(serde_json::to_value(params)?),
        );
        let response = self.request(request).await?;
        let result: CallToolResult =
            serde_json::from_value(response).context("解析 tools/call 响应失败")?;
        Ok(result)
    }

    /// 发送请求并轮询等待响应（最多约 1 秒）；无法解析的行跳过。
    async fn request(&mut self, request: JsonRpcRequest) -> Result<serde_json::Value> {
        self.transport.send(request).await?;

        // 简单轮询等待响应
        for _ in 0..100 {
            if let Some(line) = self.transport.receive().await? {
                if line.trim().is_empty() {
                    continue;
                }
                match serde_json::from_str::<JsonRpcResponse>(&line) {
                    Ok(response) => {
                        return match response.result {
                            crate::mcp::types::JsonRpcResult::Success { result } => Ok(result),
                            crate::mcp::types::JsonRpcResult::Error { error } => Err(
                                anyhow::anyhow!("MCP 错误 {}: {}", error.code, error.message),
                            ),
                        };
                    }
                    Err(e) => {
                        warn!("解析 MCP 响应行失败: {} - {}", e, line);
                        continue;
                    }
                }
            }
            tokio::time::sleep(tokio::time::Duration::from_millis(10)).await;
        }

        Err(anyhow::anyhow!("等待 MCP 响应超时"))
    }

    pub async fn close(&mut self) -> Result<()> {
        self.transport.close().await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use async_trait::async_trait;
    use std::sync::Mutex;

    struct FakeTransport {
        responses: Mutex<Vec<String>>,
        sent: Mutex<Vec<String>>,
        closed: Mutex<bool>,
    }

    impl FakeTransport {
        fn new(responses: Vec<String>) -> Self {
            Self {
                responses: Mutex::new(responses),
                sent: Mutex::new(Vec::new()),
                closed: Mutex::new(false),
            }
        }
    }

    #[async_trait]
    impl Transport for FakeTransport {
        async fn send(&mut self, request: JsonRpcRequest) -> Result<()> {
            self.sent
                .lock()
                .unwrap()
                .push(serde_json::to_string(&request).unwrap());
            Ok(())
        }

        async fn receive(&mut self) -> Result<Option<String>> {
            let mut responses = self.responses.lock().unwrap();
            if responses.is_empty() {
                return Ok(None);
            }
            Ok(Some(responses.remove(0)))
        }

        async fn close(&mut self) -> Result<()> {
            *self.closed.lock().unwrap() = true;
            Ok(())
        }
    }

    #[test]
    fn test_next_id_increment() {
        let client = McpClient::new(Box::new(crate::mcp::transport::SseTransport::new(
            "http://localhost",
        )));
        assert_eq!(client.next_id(), 1);
        assert_eq!(client.next_id(), 2);
    }

    #[tokio::test]
    async fn test_initialize() {
        let response = serde_json::json!({
            "jsonrpc": "2.0",
            "id": 1,
            "result": {
                "protocol_version": "2024-11-05",
                "capabilities": {},
                "server_info": { "name": "test", "version": "1.0" }
            }
        });
        let transport = FakeTransport::new(vec![response.to_string()]);
        let mut client = McpClient::new(Box::new(transport));
        let result = client.initialize().await.unwrap();
        assert_eq!(result.server_info.name, "test");
    }

    #[tokio::test]
    async fn test_list_tools() {
        let response = serde_json::json!({
            "jsonrpc": "2.0",
            "id": 2,
            "result": { "tools": [{ "name": "echo", "description": "d", "inputSchema": {} }] }
        });
        let transport = FakeTransport::new(vec![response.to_string()]);
        let mut client = McpClient::new(Box::new(transport));
        let tools = client.list_tools().await.unwrap();
        assert_eq!(tools.len(), 1);
        assert_eq!(tools[0].name, "echo");
    }

    #[tokio::test]
    async fn test_call_tool() {
        let response = serde_json::json!({
            "jsonrpc": "2.0",
            "id": 3,
            "result": { "content": [{ "type": "text", "text": "done" }] }
        });
        let transport = FakeTransport::new(vec![response.to_string()]);
        let mut client = McpClient::new(Box::new(transport));
        let result = client
            .call_tool("echo", serde_json::json!({}))
            .await
            .unwrap();
        assert_eq!(result.content.len(), 1);
    }

    #[tokio::test]
    async fn test_close() {
        let transport = FakeTransport::new(vec![]);
        let mut client = McpClient::new(Box::new(transport));
        client.close().await.unwrap();
    }

    #[tokio::test]
    async fn test_request_error_response() {
        let response = serde_json::json!({
            "jsonrpc": "2.0",
            "id": 1,
            "error": { "code": -1, "message": "bad" }
        });
        let transport = FakeTransport::new(vec![response.to_string()]);
        let mut client = McpClient::new(Box::new(transport));
        let result = client.initialize().await;
        assert!(result.is_err());
    }
}
