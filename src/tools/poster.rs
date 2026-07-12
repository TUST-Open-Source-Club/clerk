use anyhow::{Context, Result};
use async_trait::async_trait;
use serde_json::Value;
use std::collections::HashMap;
use std::path::PathBuf;
use tokio_stream::StreamExt;

use crate::tools::schema::{Tool, ToolContext, ToolResult, ToolSchema, get_string};

pub struct PosterTool;

#[async_trait]
impl Tool for PosterTool {
    fn name(&self) -> &str {
        "poster"
    }

    fn description(&self) -> &str {
        "将 HTML 文件渲染为海报（PDF 或 PNG）。支持自定义纸张尺寸。"
    }

    fn schema(&self) -> ToolSchema {
        ToolSchema::new("poster", "HTML 转海报")
            .with_string("input", "输入 HTML 文件路径", true)
            .with_string("output", "输出文件路径（.pdf 或 .png）", true)
            .with_string("width", "纸张宽度，例如 210mm", false)
            .with_string("height", "纸张高度，例如 297mm", false)
    }

    async fn execute(&self, args: HashMap<String, Value>, ctx: &ToolContext) -> Result<ToolResult> {
        let input = get_string(&args, "input")?;
        let output = get_string(&args, "output")?;
        let width = get_string(&args, "width").unwrap_or_else(|_| "210mm".to_string());
        let height = get_string(&args, "height").unwrap_or_else(|_| "297mm".to_string());

        let input_path = resolve_path(&ctx.working_dir, &input)?;
        let output_path = resolve_path(&ctx.working_dir, &output)?;

        let config = chromiumoxide::browser::BrowserConfig::builder()
            .headless_mode(chromiumoxide::browser::HeadlessMode::True)
            .build()
            .map_err(|e| anyhow::anyhow!("构建浏览器配置失败: {}", e))?;

        let (mut browser, mut handler) = chromiumoxide::Browser::launch(config)
            .await
            .context("启动 Chromium 失败，请确认系统已安装 Chrome/Chromium")?;

        tokio::spawn(async move {
            while let Some(h) = handler.next().await {
                if h.is_err() {
                    break;
                }
            }
        });

        let url = format!(
            "file://{}",
            input_path.canonicalize().unwrap_or(input_path).display()
        );
        let page = browser
            .new_page(&url)
            .await
            .with_context(|| format!("打开页面失败: {}", url))?;

        let is_pdf = output_path
            .extension()
            .and_then(|e| e.to_str())
            .map(|e| e.eq_ignore_ascii_case("pdf"))
            .unwrap_or(false);

        let result = if is_pdf {
            let mut params =
                chromiumoxide::cdp::browser_protocol::page::PrintToPdfParams::default();
            let width_cm = mm_to_cm(&width)?;
            let height_cm = mm_to_cm(&height)?;
            params.paper_width = Some(width_cm);
            params.paper_height = Some(height_cm);
            let pdf = page.pdf(params).await.context("生成 PDF 失败")?;
            tokio::fs::write(&output_path, pdf).await?;
            format!("海报 PDF 已保存: {}", output_path.display())
        } else {
            let params = chromiumoxide::page::ScreenshotParams::builder()
                .format(chromiumoxide::cdp::browser_protocol::page::CaptureScreenshotFormat::Png)
                .build();
            let screenshot = page.screenshot(params).await.context("截图失败")?;
            tokio::fs::write(&output_path, screenshot).await?;
            format!("海报图片已保存: {}", output_path.display())
        };

        let _ = browser.close().await;
        Ok(ToolResult::Text(result))
    }
}

fn resolve_path(working_dir: &std::path::Path, input: &str) -> Result<PathBuf> {
    let path = PathBuf::from(input);
    Ok(if path.is_absolute() {
        path
    } else {
        working_dir.join(path)
    })
}

fn mm_to_cm(value: &str) -> Result<f64> {
    let value = value.trim().to_lowercase();
    if value.ends_with("mm") {
        value[..value.len() - 2]
            .trim()
            .parse::<f64>()
            .map(|v| v / 10.0)
            .context("解析 mm 尺寸失败")
    } else if value.ends_with("cm") {
        value[..value.len() - 2]
            .trim()
            .parse::<f64>()
            .context("解析 cm 尺寸失败")
    } else {
        value.parse().context("解析尺寸失败，请使用 mm 或 cm")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_mm_to_cm() {
        assert!((mm_to_cm("210mm").unwrap() - 21.0).abs() < f64::EPSILON);
        assert!((mm_to_cm("29.7cm").unwrap() - 29.7).abs() < f64::EPSILON);
    }
}
