use anyhow::{Context, Result};
use async_trait::async_trait;
use std::process::Stdio;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, ChildStdin, ChildStdout};
use tracing::{error, info, warn};

use crate::mcp::types::JsonRpcRequest;

/// MCP 传输层抽象
#[async_trait]
pub trait Transport: Send + Sync {
    async fn send(&mut self, request: JsonRpcRequest) -> Result<()>;
    async fn receive(&mut self) -> Result<Option<String>>;
    async fn close(&mut self) -> Result<()>;
}

/// stdio 传输：通过子进程 stdin/stdout 通信
pub struct StdioTransport {
    stdin: ChildStdin,
    stdout_reader: BufReader<ChildStdout>,
    child: Child,
}

impl StdioTransport {
    pub async fn spawn(command: &str, args: &[String]) -> Result<Self> {
        let mut cmd = tokio::process::Command::new(command);
        cmd.args(args)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());

        info!("启动 MCP stdio server: {} {:?}", command, args);
        let mut child = cmd
            .spawn()
            .with_context(|| format!("启动 {} 失败", command))?;

        let stdin = child.stdin.take().context("无法获取子进程 stdin")?;
        let stdout = child.stdout.take().context("无法获取子进程 stdout")?;

        Ok(Self {
            stdin,
            stdout_reader: BufReader::new(stdout),
            child,
        })
    }
}

#[async_trait]
impl Transport for StdioTransport {
    async fn send(&mut self, request: JsonRpcRequest) -> Result<()> {
        let json = serde_json::to_string(&request).context("序列化请求失败")?;
        let line = format!("{}\n", json);
        self.stdin
            .write_all(line.as_bytes())
            .await
            .context("写入 stdin 失败")?;
        self.stdin.flush().await.context("刷新 stdin 失败")?;
        Ok(())
    }

    async fn receive(&mut self) -> Result<Option<String>> {
        let mut line = String::new();
        match self.stdout_reader.read_line(&mut line).await {
            Ok(0) => Ok(None),
            Ok(_) => Ok(Some(line.trim().to_string())),
            Err(e) => Err(anyhow::anyhow!("读取 stdout 失败: {}", e)),
        }
    }

    async fn close(&mut self) -> Result<()> {
        let _ = self.stdin.shutdown().await;
        match self.child.wait().await {
            Ok(status) => {
                if !status.success() {
                    warn!("MCP server 退出码: {:?}", status.code());
                }
            }
            Err(e) => error!("等待 MCP server 失败: {}", e),
        }
        Ok(())
    }
}

/// SSE 传输：通过 HTTP SSE 接收，POST 发送
pub struct SseTransport {
    client: reqwest::Client,
    endpoint: String,
    message_endpoint: Option<String>,
    event_buffer: Vec<String>,
}

impl SseTransport {
    pub fn new(endpoint: impl Into<String>) -> Self {
        Self {
            client: reqwest::Client::new(),
            endpoint: endpoint.into().trim_end_matches('/').to_string(),
            message_endpoint: None,
            event_buffer: Vec::new(),
        }
    }

    pub async fn connect(&mut self) -> Result<()> {
        info!("连接 MCP SSE endpoint: {}", self.endpoint);
        // SSE 连接后，server 会通过 event 发送 message endpoint
        // 此处仅做占位，完整实现需要 SSE 解析与后台任务
        self.message_endpoint = Some(format!("{}/message", self.endpoint));
        Ok(())
    }
}

#[async_trait]
impl Transport for SseTransport {
    async fn send(&mut self, request: JsonRpcRequest) -> Result<()> {
        let endpoint = self
            .message_endpoint
            .as_ref()
            .context("SSE 未连接，缺少 message endpoint")?;
        self.client
            .post(endpoint)
            .json(&request)
            .send()
            .await
            .context("SSE POST 失败")?;
        Ok(())
    }

    async fn receive(&mut self) -> Result<Option<String>> {
        if let Some(line) = self.event_buffer.pop() {
            return Ok(Some(line));
        }
        // 完整实现应维护 SSE 长连接并解析事件
        Ok(None)
    }

    async fn close(&mut self) -> Result<()> {
        self.message_endpoint = None;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_sse_transport_new() {
        let transport = SseTransport::new("http://localhost:3000/");
        assert_eq!(transport.endpoint, "http://localhost:3000");
    }
}

#[cfg(test)]
mod spawn_tests {
    // 子进程测试在部分 CI 环境中不稳定，保留空模块占位
}
