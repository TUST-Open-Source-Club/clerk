use anyhow::{Context, Result};
use std::sync::atomic::{AtomicU64, Ordering};
use tracing::{info, warn};

use crate::mcp::transport::{StdioTransport, Transport};
use crate::mcp::types::{
    CallToolParams, CallToolResult, ClientCapabilities, ImplementationInfo, InitializeParams,
    InitializeResult, JsonRpcRequest, JsonRpcResponse, McpTool,
};

pub struct McpClient {
    transport: Box<dyn Transport>,
    next_id: AtomicU64,
}

impl McpClient {
    pub fn new(transport: Box<dyn Transport>) -> Self {
        Self {
            transport,
            next_id: AtomicU64::new(1),
        }
    }

    pub async fn connect_stdio(command: &str, args: &[String]) -> Result<Self> {
        let transport = StdioTransport::spawn(command, args).await?;
        let mut client = Self::new(Box::new(transport));
        client.initialize().await?;
        Ok(client)
    }

    fn next_id(&self) -> u64 {
        self.next_id.fetch_add(1, Ordering::SeqCst)
    }

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

    #[test]
    fn test_next_id_increment() {
        let client = McpClient::new(Box::new(crate::mcp::transport::SseTransport::new(
            "http://localhost",
        )));
        assert_eq!(client.next_id(), 1);
        assert_eq!(client.next_id(), 2);
    }
}
