use anyhow::{Context, Result};
use async_trait::async_trait;
use serde_json::Value;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use tokio_stream::StreamExt;

use crate::tools::schema::{Tool, ToolContext, ToolResult, ToolSchema, get_i64, get_string};
use crate::util::expand_tilde;

/// `render_to_image` 工具：按扩展名将 HTML/PDF/Office/图片渲染或转换为 PNG 预览图。
pub struct RenderToImage;

#[async_trait]
impl Tool for RenderToImage {
    fn name(&self) -> &str {
        "render_to_image"
    }

    fn description(&self) -> &str {
        "将 HTML/PDF/Office/图片文件渲染或转换为 PNG 预览图。"
    }

    fn schema(&self) -> ToolSchema {
        ToolSchema::new("render_to_image", "渲染文件为图片")
            .with_string("input", "输入文件路径", true)
            .with_string(
                "output",
                "输出 PNG 文件路径，默认为 <input>.preview.png",
                false,
            )
            .with_integer("width", "截图宽度（HTML 截图时使用）", false)
            .with_integer("height", "截图高度（HTML 截图时使用）", false)
    }

    async fn execute(&self, args: HashMap<String, Value>, ctx: &ToolContext) -> Result<ToolResult> {
        let input = get_string(&args, "input")?;
        let output = get_string(&args, "output").ok();
        let width = get_i64(&args, "width", 0);
        let height = get_i64(&args, "height", 0);

        let input_path = resolve_path(&ctx.working_dir, &input)?;
        if !input_path.exists() {
            anyhow::bail!("输入文件不存在: {}", input_path.display());
        }

        let output_path = match output {
            Some(o) => resolve_path(&ctx.working_dir, &o)?,
            None => default_output_path(&ctx.working_dir, &input_path),
        };

        let ext = input_path
            .extension()
            .and_then(|e| e.to_str())
            .unwrap_or("")
            .to_lowercase();

        match ext.as_str() {
            "html" | "htm" => {
                render_html_to_png(&input_path, &output_path, width, height).await?;
            }
            "pdf" => {
                convert_pdf_to_png(&input_path, &output_path).await?;
            }
            "docx" | "xlsx" | "pptx" => {
                convert_office_to_png(&input_path, &output_path).await?;
            }
            "png" | "jpg" | "jpeg" | "gif" | "webp" => {
                tokio::fs::copy(&input_path, &output_path)
                    .await
                    .with_context(|| format!("复制图片失败: {}", input_path.display()))?;
            }
            _ => {
                return Ok(ToolResult::Error(format!("不支持的文件类型: {}", ext)));
            }
        }

        Ok(ToolResult::Text(format!(
            "已生成预览图片: {}（来源: {}）",
            output_path.display(),
            input_path.display()
        )))
    }
}

/// 展开 `~` 后，相对路径基于工作目录解析，绝对路径原样返回。
fn resolve_path(working_dir: &Path, input: &str) -> Result<PathBuf> {
    let path = expand_tilde(input);
    Ok(if path.is_absolute() {
        path
    } else {
        working_dir.join(path)
    })
}

/// 生成默认输出路径：`<工作目录>/<输入文件名>.preview.png`。
fn default_output_path(working_dir: &Path, input_path: &Path) -> PathBuf {
    let name = input_path
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("input");
    working_dir.join(format!("{}.preview.png", name))
}

/// 检查命令是否存在于 PATH 中。
async fn command_exists(cmd: &str) -> bool {
    tokio::process::Command::new(cmd)
        .arg("--version")
        .output()
        .await
        .map(|o| o.status.success())
        .unwrap_or(false)
}

/// 用无头 Chromium 打开 HTML 文件并截图保存为 PNG，可指定视口宽高。
async fn render_html_to_png(input: &Path, output: &Path, width: i64, height: i64) -> Result<()> {
    let mut config_builder = chromiumoxide::browser::BrowserConfig::builder()
        .headless_mode(chromiumoxide::browser::HeadlessMode::True);

    if width > 0 && height > 0 {
        let viewport = chromiumoxide::handler::viewport::Viewport {
            width: width as u32,
            height: height as u32,
            device_scale_factor: Some(1.0),
            emulating_mobile: false,
            is_landscape: false,
            has_touch: false,
        };
        config_builder = config_builder.viewport(viewport);
    }

    let config = config_builder
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

    let canonical = input.canonicalize().unwrap_or_else(|_| input.to_path_buf());
    let url = format!("file://{}", canonical.display());
    let page = browser
        .new_page(&url)
        .await
        .with_context(|| format!("打开页面失败: {}", url))?;

    page.wait_for_navigation()
        .await
        .context("等待页面加载失败")?;

    let params = chromiumoxide::page::ScreenshotParams::builder()
        .format(chromiumoxide::cdp::browser_protocol::page::CaptureScreenshotFormat::Png)
        .build();
    let screenshot = page.screenshot(params).await.context("截图失败")?;
    tokio::fs::write(output, screenshot)
        .await
        .with_context(|| format!("写入截图失败: {}", output.display()))?;

    let _ = browser.close().await;
    Ok(())
}

/// 将 PDF 首页转为 PNG：优先 pdftoppm，其次 mutool。
async fn convert_pdf_to_png(input: &Path, output: &Path) -> Result<()> {
    if command_exists("pdftoppm").await {
        let prefix = output.with_extension("");
        let status = tokio::process::Command::new("pdftoppm")
            .arg("-png")
            .arg("-f")
            .arg("1")
            .arg("-l")
            .arg("1")
            .arg(input)
            .arg(&prefix)
            .output()
            .await
            .context("运行 pdftoppm 失败")?;
        if !status.status.success() {
            let stderr = String::from_utf8_lossy(&status.stderr);
            anyhow::bail!("pdftoppm 失败: {}", stderr);
        }
        let generated = prefix.with_extension("png");
        let expected = prefix
            .parent()
            .unwrap_or_else(|| Path::new("."))
            .join(format!(
                "{}-1.png",
                prefix.file_name().unwrap_or_default().to_string_lossy()
            ));
        let source = if expected.exists() {
            expected
        } else {
            generated
        };
        tokio::fs::rename(&source, output).await.with_context(|| {
            format!(
                "重命名输出文件失败: {} -> {}",
                source.display(),
                output.display()
            )
        })?;
        return Ok(());
    }

    if command_exists("mutool").await {
        let status = tokio::process::Command::new("mutool")
            .arg("draw")
            .arg("-o")
            .arg(output)
            .arg("-F")
            .arg("png")
            .arg(input)
            .arg("1")
            .output()
            .await
            .context("运行 mutool 失败")?;
        if !status.status.success() {
            let stderr = String::from_utf8_lossy(&status.stderr);
            anyhow::bail!("mutool 失败: {}", stderr);
        }
        return Ok(());
    }

    anyhow::bail!("{}", pdf_tool_missing_hint());
}

/// 未检测到 PDF 转图片工具时的安装提示。
fn pdf_tool_missing_hint() -> String {
    "未检测到 PDF 转图片工具。请安装以下任一工具：\n\
    - pdftoppm: sudo apt install poppler-utils / brew install poppler\n\
    - mutool: sudo apt install mupdf-tools / brew install mupdf-tools"
        .to_string()
}

/// 将 Office 文档（docx/xlsx/pptx）先经 LibreOffice 转 PDF，再转 PNG。
async fn convert_office_to_png(input: &Path, output: &Path) -> Result<()> {
    if !command_exists("libreoffice").await {
        anyhow::bail!("{}", office_tool_missing_hint());
    }

    let temp_dir = std::env::temp_dir().join(format!("clerk_office_{}", uuid::Uuid::new_v4()));
    tokio::fs::create_dir_all(&temp_dir)
        .await
        .context("创建临时目录失败")?;
    let status = tokio::process::Command::new("libreoffice")
        .arg("--headless")
        .arg("--convert-to")
        .arg("pdf")
        .arg("--outdir")
        .arg(&temp_dir)
        .arg(input)
        .output()
        .await
        .context("运行 LibreOffice 失败")?;
    if !status.status.success() {
        let stderr = String::from_utf8_lossy(&status.stderr);
        anyhow::bail!("LibreOffice 转换失败: {}", stderr);
    }

    let stem = input
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("input");
    let pdf_path = temp_dir.join(format!("{}.pdf", stem));
    if !pdf_path.exists() {
        anyhow::bail!("LibreOffice 未生成预期的 PDF 文件");
    }

    convert_pdf_to_png(&pdf_path, output).await
}

/// 未检测到 LibreOffice 时的安装提示。
fn office_tool_missing_hint() -> String {
    "未检测到 LibreOffice。请安装以渲染 Office 文件：\n\
    - Ubuntu/Debian: sudo apt install libreoffice\n\
    - macOS: brew install --cask libreoffice\n\
    - Windows: winget install TheDocumentFoundation.LibreOffice"
        .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn ctx(dir: &TempDir) -> ToolContext {
        ToolContext {
            working_dir: dir.path().to_path_buf(),
            ..Default::default()
        }
    }

    #[test]
    fn test_name() {
        let tool = RenderToImage;
        assert_eq!(tool.name(), "render_to_image");
    }

    #[test]
    fn test_schema() {
        let tool = RenderToImage;
        let schema = tool.schema();
        assert_eq!(schema.name, "render_to_image");
        let props = schema
            .parameters
            .get("properties")
            .unwrap()
            .as_object()
            .unwrap();
        assert!(props.contains_key("input"));
        assert!(props.contains_key("output"));
        assert!(props.contains_key("width"));
        assert!(props.contains_key("height"));
    }

    #[tokio::test]
    async fn test_missing_input() {
        let dir = TempDir::new().unwrap();
        let tool = RenderToImage;
        let mut args = HashMap::new();
        args.insert(
            "input".to_string(),
            Value::String("missing.html".to_string()),
        );
        let result = tool.execute(args, &ctx(&dir)).await;
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(err.contains("输入文件不存在"));
    }

    #[tokio::test]
    async fn test_image_copy() {
        let dir = TempDir::new().unwrap();
        let input = dir.path().join("test.png");
        let img = image::RgbImage::new(10, 10);
        img.save(&input).unwrap();

        let tool = RenderToImage;
        let mut args = HashMap::new();
        args.insert("input".to_string(), Value::String("test.png".to_string()));
        let result = tool.execute(args, &ctx(&dir)).await.unwrap();
        let text = result.to_string_for_model();
        assert!(text.contains("test.png.preview.png"));
        assert!(dir.path().join("test.png.preview.png").exists());
    }

    #[tokio::test]
    async fn test_html_to_png() {
        let dir = TempDir::new().unwrap();
        let input = dir.path().join("test.html");
        tokio::fs::write(&input, "<html><body>hi</body></html>")
            .await
            .unwrap();

        let tool = RenderToImage;
        let mut args = HashMap::new();
        args.insert("input".to_string(), Value::String("test.html".to_string()));
        args.insert("width".to_string(), Value::Number(320.into()));
        args.insert("height".to_string(), Value::Number(240.into()));
        let result = tool.execute(args, &ctx(&dir)).await;
        match result {
            Ok(r) => {
                let text = r.to_string_for_model();
                assert!(text.contains("test.html.preview.png"));
                assert!(dir.path().join("test.html.preview.png").exists());
            }
            Err(_) => {
                // Chromium 可能未安装或环境不支持，接受错误结果
            }
        }
    }

    #[tokio::test]
    async fn test_unsupported_extension() {
        let dir = TempDir::new().unwrap();
        let input = dir.path().join("test.txt");
        tokio::fs::write(&input, "hello").await.unwrap();

        let tool = RenderToImage;
        let mut args = HashMap::new();
        args.insert("input".to_string(), Value::String("test.txt".to_string()));
        let result = tool.execute(args, &ctx(&dir)).await.unwrap();
        let text = result.to_string_for_model();
        assert!(text.contains("不支持的文件类型"));
    }

    #[test]
    fn test_resolve_path() {
        let wd = std::path::PathBuf::from("/tmp");
        assert_eq!(
            resolve_path(&wd, "a.png").unwrap(),
            std::path::PathBuf::from("/tmp/a.png")
        );
        assert_eq!(
            resolve_path(&wd, "/abs/a.png").unwrap(),
            std::path::PathBuf::from("/abs/a.png")
        );
    }

    #[test]
    fn test_default_output_path() {
        let wd = std::path::PathBuf::from("/tmp");
        let input = std::path::PathBuf::from("/home/a.html");
        assert_eq!(
            default_output_path(&wd, &input),
            std::path::PathBuf::from("/tmp/a.html.preview.png")
        );
    }

    #[tokio::test]
    async fn test_html_to_png_without_viewport() {
        let dir = TempDir::new().unwrap();
        let input = dir.path().join("test.html");
        tokio::fs::write(&input, "<html><body>hi</body></html>")
            .await
            .unwrap();

        let tool = RenderToImage;
        let mut args = HashMap::new();
        args.insert("input".to_string(), Value::String("test.html".to_string()));
        let result = tool.execute(args, &ctx(&dir)).await;
        match result {
            Ok(r) => {
                let text = r.to_string_for_model();
                assert!(text.contains("test.html.preview.png"));
                assert!(dir.path().join("test.html.preview.png").exists());
            }
            Err(_) => {
                // Chromium 可能未安装或环境不支持，接受错误结果
            }
        }
    }

    #[tokio::test]
    async fn test_gif_copy() {
        let dir = TempDir::new().unwrap();
        let input = dir.path().join("test.gif");
        // 最小有效 1x1 GIF
        let gif = b"GIF89a\x01\x00\x01\x00\x00\x00\x00!\xf9\x04\x00\x00\x00\x00\x00,\x00\x00\x00\x00\x01\x00\x01\x00\x00\x02\x02D\x01\x00;";
        tokio::fs::write(&input, gif).await.unwrap();

        let tool = RenderToImage;
        let mut args = HashMap::new();
        args.insert("input".to_string(), Value::String("test.gif".to_string()));
        let result = tool.execute(args, &ctx(&dir)).await.unwrap();
        let text = result.to_string_for_model();
        assert!(text.contains("test.gif.preview.png"));
        assert!(dir.path().join("test.gif.preview.png").exists());
    }

    #[tokio::test]
    async fn test_pdf_to_png() {
        let dir = TempDir::new().unwrap();
        let input = dir.path().join("test.pdf");
        create_minimal_pdf(&input);

        let tool = RenderToImage;
        let mut args = HashMap::new();
        args.insert("input".to_string(), Value::String("test.pdf".to_string()));
        let result = tool.execute(args, &ctx(&dir)).await;
        match result {
            Ok(r) => {
                let text = r.to_string_for_model();
                assert!(text.contains("test.pdf.preview.png"));
                assert!(dir.path().join("test.pdf.preview.png").exists());
            }
            Err(e) => {
                let msg = e.to_string();
                assert!(
                    msg.contains("pdftoppm")
                        || msg.contains("mutool")
                        || msg.contains("PDF 转图片")
                );
            }
        }
    }

    #[tokio::test]
    async fn test_office_to_png() {
        let dir = TempDir::new().unwrap();
        let input = dir.path().join("test.docx");
        create_minimal_docx(&input);

        let tool = RenderToImage;
        let mut args = HashMap::new();
        args.insert("input".to_string(), Value::String("test.docx".to_string()));
        let result = tool.execute(args, &ctx(&dir)).await;
        match result {
            Ok(r) => {
                let text = r.to_string_for_model();
                assert!(text.contains("test.docx.preview.png"));
                assert!(dir.path().join("test.docx.preview.png").exists());
            }
            Err(e) => {
                let msg = e.to_string();
                assert!(msg.contains("LibreOffice") || msg.contains("LibreOffice 转换失败"));
            }
        }
    }

    #[tokio::test]
    async fn test_command_exists_false_for_missing() {
        assert!(!command_exists("clerk_nonexistent_binary_9999").await);
    }

    #[test]
    fn test_pdf_tool_missing_hint() {
        let hint = pdf_tool_missing_hint();
        assert!(hint.contains("pdftoppm"));
        assert!(hint.contains("mutool"));
    }

    #[test]
    fn test_office_tool_missing_hint() {
        let hint = office_tool_missing_hint();
        assert!(hint.contains("LibreOffice"));
    }

    fn create_minimal_pdf(path: &std::path::Path) {
        use lopdf::{Document, Object, Stream, dictionary};
        let mut doc = Document::with_version("1.4");
        let pages_id = doc.new_object_id();
        let page_id = doc.new_object_id();
        let resources_id = doc.new_object_id();
        let content_id = doc.new_object_id();

        doc.objects.insert(
            resources_id,
            Object::Dictionary(dictionary! {
                "Font" => dictionary! {
                    "F1" => dictionary! {
                        "Type" => "Font",
                        "Subtype" => "Type1",
                        "BaseFont" => "Helvetica",
                    }
                }
            }),
        );

        let content = Stream::new(
            dictionary! {
                "Length" => Object::Integer(0),
            },
            vec![],
        );
        doc.objects.insert(content_id, Object::Stream(content));

        doc.objects.insert(
            pages_id,
            Object::Dictionary(dictionary! {
                "Type" => "Pages",
                "Kids" => vec![Object::Reference(page_id)],
                "Count" => 1,
            }),
        );
        doc.objects.insert(
            page_id,
            Object::Dictionary(dictionary! {
                "Type" => "Page",
                "Parent" => Object::Reference(pages_id),
                "MediaBox" => vec![0.into(), 0.into(), 612.into(), 792.into()],
                "Resources" => Object::Reference(resources_id),
                "Contents" => Object::Reference(content_id),
            }),
        );
        let catalog_id = doc.new_object_id();
        doc.objects.insert(
            catalog_id,
            Object::Dictionary(dictionary! {
                "Type" => "Catalog",
                "Pages" => Object::Reference(pages_id),
            }),
        );
        doc.trailer.set("Root", Object::Reference(catalog_id));
        doc.save(path).unwrap();
    }

    fn create_minimal_docx(path: &std::path::Path) {
        use docx_rs::Docx;
        let file = std::fs::File::create(path).unwrap();
        Docx::new().build().pack(file).unwrap();
    }
}
