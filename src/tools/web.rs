use anyhow::{Context, Result};
use async_trait::async_trait;
use serde_json::Value;
use std::collections::HashMap;

use crate::tools::schema::{Tool, ToolContext, ToolResult, ToolSchema, get_bool, get_string};

/// `web_fetch` 工具：HTTP GET 抓取网页，可选转 Markdown 并按字符数截断。
pub struct WebFetchTool;

#[async_trait]
impl Tool for WebFetchTool {
    fn name(&self) -> &str {
        "web_fetch"
    }

    fn description(&self) -> &str {
        "使用 HTTP GET 获取网页内容，可选转换为 Markdown 纯文本。"
    }

    fn schema(&self) -> ToolSchema {
        ToolSchema::new("web_fetch", "获取网页内容")
            .with_string("url", "目标 URL", true)
            .with_boolean("to_markdown", "是否将 HTML 转换为 Markdown", false)
            .with_integer("max_length", "最大返回字符数，0 表示不限制", false)
    }

    async fn execute(
        &self,
        args: HashMap<String, Value>,
        _ctx: &ToolContext,
    ) -> Result<ToolResult> {
        let url = get_string(&args, "url")?;
        let to_markdown = get_bool(&args, "to_markdown", false);
        let max_length = crate::tools::schema::get_i64(&args, "max_length", 0);

        let client = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(30))
            .build()
            .context("创建 HTTP 客户端失败")?;

        let response = client
            .get(&url)
            .send()
            .await
            .with_context(|| format!("请求 {} 失败", url))?;

        let status = response.status();
        let body = response.text().await.context("读取响应体失败")?;

        let mut content = if to_markdown {
            html_to_markdown(&body)
        } else {
            body
        };

        if max_length > 0 {
            let max = max_length as usize;
            if content.chars().count() > max {
                content = content.chars().take(max).collect::<String>();
                content.push_str("\n\n（内容已截断）");
            }
        }

        Ok(ToolResult::Text(format!("状态: {}\n\n{}", status, content)))
    }
}

/// `web_post` 工具：以 JSON 请求体发送 HTTP POST。
pub struct WebPostTool;

#[async_trait]
impl Tool for WebPostTool {
    fn name(&self) -> &str {
        "web_post"
    }

    fn description(&self) -> &str {
        "使用 HTTP POST 向指定 URL 发送 JSON 数据。"
    }

    fn schema(&self) -> ToolSchema {
        ToolSchema::new("web_post", "POST 请求")
            .with_string("url", "目标 URL", true)
            .with_string("body", "JSON 请求体", true)
    }

    async fn execute(
        &self,
        args: HashMap<String, Value>,
        _ctx: &ToolContext,
    ) -> Result<ToolResult> {
        let url = get_string(&args, "url")?;
        let body_str = get_string(&args, "body")?;
        let body: Value = serde_json::from_str(&body_str).context("请求体不是合法 JSON")?;

        let client = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(30))
            .build()
            .context("创建 HTTP 客户端失败")?;

        let response = client
            .post(&url)
            .json(&body)
            .send()
            .await
            .with_context(|| format!("POST {} 失败", url))?;

        let status = response.status();
        let text = response.text().await.context("读取响应体失败")?;

        Ok(ToolResult::Text(format!("状态: {}\n\n{}", status, text)))
    }
}

/// 简易 HTML 转 Markdown
pub fn html_to_markdown(html: &str) -> String {
    let mut output = String::new();
    let mut in_tag = false;
    let mut tag_name = String::new();
    let mut last_was_newline = false;

    for ch in html.chars() {
        if ch == '<' {
            in_tag = true;
            tag_name.clear();
        } else if ch == '>' {
            in_tag = false;
            let name = tag_name.to_lowercase();
            if (name == "/p" || name == "br" || name.starts_with("h")) && !last_was_newline {
                output.push('\n');
                last_was_newline = true;
            }
        } else if in_tag {
            if ch.is_alphabetic() || ch == '/' {
                tag_name.push(ch);
            } else {
                // 遇到属性，停止记录 tag_name
                tag_name.push(' ');
            }
        } else {
            output.push(ch);
            last_was_newline = ch == '\n';
        }
    }

    output
        .lines()
        .map(|line| line.trim())
        .filter(|line| !line.is_empty())
        .collect::<Vec<_>>()
        .join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;
    use wiremock::{
        Mock, MockServer, ResponseTemplate,
        matchers::{method, path},
    };

    #[test]
    fn test_html_to_markdown() {
        let html = "<h1>Title</h1><p>Hello <br>World</p>";
        let md = html_to_markdown(html);
        assert!(md.contains("Title"));
        assert!(md.contains("Hello"));
        assert!(md.contains("World"));
    }

    #[tokio::test]
    async fn test_web_fetch() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/page"))
            .respond_with(ResponseTemplate::new(200).set_body_string("<h1>Test</h1>"))
            .mount(&server)
            .await;

        let tool = WebFetchTool;
        let mut args = HashMap::new();
        args.insert(
            "url".to_string(),
            Value::String(format!("{}/page", server.uri())),
        );
        args.insert("to_markdown".to_string(), Value::Bool(true));

        let result = tool.execute(args, &ToolContext::default()).await.unwrap();
        let text = result.to_string_for_model();
        assert!(text.contains("Test"));
        assert!(text.contains("200"));
    }
}
