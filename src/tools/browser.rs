use anyhow::{Context, Result};
use async_trait::async_trait;
use serde_json::Value;
use std::collections::HashMap;
use tokio_stream::StreamExt;

use crate::tools::schema::{Tool, ToolContext, ToolResult, ToolSchema, get_string};

pub struct BrowserTool {
    // 浏览器实例由每次调用时按需创建，避免长期占用资源
}

impl BrowserTool {
    pub fn new() -> Self {
        Self {}
    }

    async fn launch_browser() -> Result<chromiumoxide::Browser> {
        let config = chromiumoxide::browser::BrowserConfig::builder()
            .headless_mode(chromiumoxide::browser::HeadlessMode::True)
            .build()
            .map_err(|e| anyhow::anyhow!("构建浏览器配置失败: {}", e))?;

        let (browser, mut handler) = chromiumoxide::Browser::launch(config)
            .await
            .context("启动 Chromium 失败，请确认系统已安装 Chrome/Chromium")?;

        // 在后台运行浏览器事件循环
        tokio::spawn(async move {
            while let Some(h) = handler.next().await {
                if h.is_err() {
                    break;
                }
            }
        });

        Ok(browser)
    }
}

#[async_trait]
impl Tool for BrowserTool {
    fn name(&self) -> &str {
        "browser"
    }

    fn description(&self) -> &str {
        "使用无头 Chromium 浏览器打开网页、执行操作并获取内容/PDF/截图。"
    }

    fn schema(&self) -> ToolSchema {
        ToolSchema::new("browser", "无头浏览器操作")
            .with_string("url", "目标 URL", true)
            .with_string(
                "action",
                "操作类型: navigate|html|pdf|screenshot|click|type",
                true,
            )
            .with_string("selector", "CSS 选择器（click/type 时使用）", false)
            .with_string("value", "输入值（type 时使用）", false)
            .with_string("output", "输出文件路径（pdf/screenshot 时使用）", false)
    }

    async fn execute(&self, args: HashMap<String, Value>, ctx: &ToolContext) -> Result<ToolResult> {
        let url = get_string(&args, "url")?;
        let action = get_string(&args, "action")?;
        let selector = get_string(&args, "selector").ok();
        let value = get_string(&args, "value").ok();
        let output = get_string(&args, "output").ok();

        let mut browser = Self::launch_browser().await?;
        let page = browser
            .new_page(&url)
            .await
            .with_context(|| format!("打开页面失败: {}", url))?;

        let result = match action.as_str() {
            "navigate" => {
                page.wait_for_navigation()
                    .await
                    .context("等待页面加载失败")?;
                format!("已导航到: {}", url)
            }
            "html" => {
                page.wait_for_navigation()
                    .await
                    .context("等待页面加载失败")?;
                let html = page.content().await.context("获取 HTML 失败")?;
                truncate_html(&html, 8000)
            }
            "pdf" => {
                let output_path = resolve_output_path(ctx, output.as_deref(), "page.pdf")?;
                let params =
                    chromiumoxide::cdp::browser_protocol::page::PrintToPdfParams::default();
                let pdf = page.pdf(params).await.context("生成 PDF 失败")?;
                tokio::fs::write(&output_path, pdf).await?;
                format!("PDF 已保存: {}", output_path.display())
            }
            "screenshot" => {
                let output_path = resolve_output_path(ctx, output.as_deref(), "screenshot.png")?;
                let params = chromiumoxide::page::ScreenshotParams::builder()
                    .format(
                        chromiumoxide::cdp::browser_protocol::page::CaptureScreenshotFormat::Png,
                    )
                    .build();
                let screenshot = page.screenshot(params).await.context("截图失败")?;
                tokio::fs::write(&output_path, screenshot).await?;
                format!("截图已保存: {}", output_path.display())
            }
            "click" => {
                let sel = selector.context("click 操作需要 selector 参数")?;
                let element = page
                    .find_element(&sel)
                    .await
                    .with_context(|| format!("查找元素 {} 失败", sel))?;
                element
                    .click()
                    .await
                    .with_context(|| format!("点击 {} 失败", sel))?;
                format!("已点击: {}", sel)
            }
            "type" => {
                let sel = selector.context("type 操作需要 selector 参数")?;
                let val = value.context("type 操作需要 value 参数")?;
                let element = page
                    .find_element(&sel)
                    .await
                    .with_context(|| format!("查找元素 {} 失败", sel))?;
                element
                    .type_str(&val)
                    .await
                    .with_context(|| format!("在 {} 输入失败", sel))?;
                format!("已在 {} 输入: {}", sel, val)
            }
            _ => return Err(anyhow::anyhow!("未知操作: {}", action)),
        };

        let _ = browser.close().await;
        Ok(ToolResult::Text(result))
    }
}

fn resolve_output_path(
    ctx: &ToolContext,
    output: Option<&str>,
    default: &str,
) -> Result<std::path::PathBuf> {
    let path = match output {
        Some(p) => std::path::PathBuf::from(p),
        None => ctx.working_dir.join(default),
    };
    Ok(if path.is_absolute() {
        path
    } else {
        ctx.working_dir.join(path)
    })
}

fn truncate_html(html: &str, max_chars: usize) -> String {
    let text = crate::tools::web::html_to_markdown(html);
    if text.chars().count() > max_chars {
        let truncated: String = text.chars().take(max_chars).collect();
        format!("{}\n\n（内容已截断）", truncated)
    } else {
        text
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_resolve_output_path() {
        let ctx = ToolContext {
            working_dir: std::path::PathBuf::from("/tmp"),
        };
        let path = resolve_output_path(&ctx, None, "out.pdf").unwrap();
        assert_eq!(path, std::path::PathBuf::from("/tmp/out.pdf"));

        let path = resolve_output_path(&ctx, Some("a.png"), "out.pdf").unwrap();
        assert_eq!(path, std::path::PathBuf::from("/tmp/a.png"));

        let path = resolve_output_path(&ctx, Some("/abs/a.png"), "out.pdf").unwrap();
        assert_eq!(path, std::path::PathBuf::from("/abs/a.png"));
    }

    #[test]
    fn test_truncate_html() {
        let html = "<p>hello world</p>";
        let result = truncate_html(html, 100);
        assert!(result.contains("hello world"));
        assert!(!result.contains("截断"));

        let long = "a".repeat(200);
        let result = truncate_html(&format!("<p>{}</p>", long), 50);
        assert!(result.contains("截断"));
    }

    #[test]
    fn test_name() {
        let tool = BrowserTool::new();
        assert_eq!(tool.name(), "browser");
    }

    #[test]
    fn test_description() {
        let tool = BrowserTool::new();
        assert!(!tool.description().is_empty());
    }

    #[test]
    fn test_schema() {
        let tool = BrowserTool::new();
        let schema = tool.schema();
        assert_eq!(schema.name, "browser");
        let props = schema
            .parameters
            .get("properties")
            .unwrap()
            .as_object()
            .unwrap();
        assert!(props.contains_key("url"));
        assert!(props.contains_key("action"));
    }

    #[test]
    fn test_into_tool_definition() {
        let tool = BrowserTool::new();
        let def = tool.schema().into_tool_definition();
        assert_eq!(def.function.name, "browser");
        assert_eq!(def.tool_type, "function");
    }

    #[tokio::test]
    async fn test_browser_execute_missing_url() {
        let tool = BrowserTool::new();
        let result = tool.execute(HashMap::new(), &ToolContext::default()).await;
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(err.contains("url"));
    }

    #[tokio::test]
    async fn test_browser_execute_missing_action() {
        let tool = BrowserTool::new();
        let mut args = HashMap::new();
        args.insert(
            "url".to_string(),
            Value::String("https://example.com".to_string()),
        );
        let result = tool.execute(args, &ToolContext::default()).await;
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(err.contains("action"));
    }
}
