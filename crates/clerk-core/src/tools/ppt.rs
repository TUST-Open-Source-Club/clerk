//! PowerPoint 工具：基于 `ppt-rs` 创建与读取 .pptx 演示文稿。
//! 支持标题页/标题+内容/双栏/空白等版式、文本框、项目符号、图片、
//! 主题配色字体，以及基于模板 .pptx 生成新演示文稿。

use anyhow::{Context, Result};
use async_trait::async_trait;
use ppt_rs::oxml::PresentationReader;
use ppt_rs::{
    Image, PresentationSettings, PresentationTheme, Shape, ShapeType, SlideContent, SlideLayout,
    create_pptx_with_settings, create_pptx_with_template,
};
use serde::Deserialize;
use serde_json::{Value, json};
use std::collections::HashMap;
use std::path::PathBuf;

use crate::tools::schema::{Tool, ToolContext, ToolResult, ToolSchema, get_string};

/// 1 英寸 = 914400 EMU。
const EMU_PER_INCH: f64 = 914400.0;

fn inches_to_emu(inches: f64) -> u32 {
    (inches * EMU_PER_INCH) as u32
}

/// 幻灯片规格（JSON）。
#[derive(Debug, Deserialize)]
struct PptSlide {
    /// 版式：title / title_content / two_content / section / title_only / blank
    layout: Option<String>,
    title: Option<String>,
    /// 副标题（仅 title 版式有效，以居中透明文本框渲染）
    subtitle: Option<String>,
    /// 项目符号：字符串或 {"text": "..", "level": 1}
    bullets: Option<Vec<Value>>,
    text_boxes: Option<Vec<PptTextBox>>,
    images: Option<Vec<PptImage>>,
    notes: Option<String>,
    title_color: Option<String>,
    content_color: Option<String>,
    title_size: Option<u32>,
    content_size: Option<u32>,
}

#[derive(Debug, Deserialize)]
struct PptTextBox {
    /// 位置与尺寸（英寸）
    x: f64,
    y: f64,
    width: f64,
    height: f64,
    text: String,
}

#[derive(Debug, Deserialize)]
struct PptImage {
    path: String,
    x: Option<f64>,
    y: Option<f64>,
    /// 显示宽度（英寸），高度按比例缩放
    width: Option<f64>,
}

/// `office_write_ppt` 工具：按 JSON 规格创建 PowerPoint 演示文稿。
pub struct WritePptTool;

#[async_trait]
impl Tool for WritePptTool {
    fn name(&self) -> &str {
        "office_write_ppt"
    }

    fn description(&self) -> &str {
        "创建 PowerPoint (.pptx) 演示文稿。支持版式（title/title_content/two_content/section/title_only/blank）、\
        项目符号、文本框、图片、演讲备注、主题配色与字体，可基于模板 .pptx 生成。"
    }

    fn schema(&self) -> ToolSchema {
        ToolSchema::new("office_write_ppt", "创建 PPT")
            .with_string("path", "输出 .pptx 文件路径", true)
            .with_string("title", "演示文稿标题（元数据）", false)
            .with_string(
                "theme",
                "主题：office|corporate|modern|vibrant|dark|nature|tech|carbon，默认 office",
                false,
            )
            .with_string("title_font", "标题字体（主题 major font）", false)
            .with_string("body_font", "正文字体（主题 minor font）", false)
            .with_string("template", "模板 .pptx 路径（套用其母版与版式）", false)
            .with_array(
                "slides",
                crate::tools::schema::ParameterSchema::object(
                    "{\"layout\":\"title_content\",\"title\":..,\"subtitle\":..,\"bullets\":[\"..\",{\"text\":..,\"level\":1}],\"text_boxes\":[{\"x\":1.0,\"y\":1.0,\"width\":3.0,\"height\":1.0,\"text\":..}],\"images\":[{\"path\":..,\"x\":1.0,\"y\":2.0,\"width\":3.0}],\"notes\":..,\"title_color\":\"FF0000\",\"title_size\":40}",
                ),
                "幻灯片数组",
                true,
            )
    }

    async fn execute(&self, args: HashMap<String, Value>, ctx: &ToolContext) -> Result<ToolResult> {
        let path_str = get_string(&args, "path")?;
        let path = resolve_path(&ctx.working_dir, &path_str)?;
        let title = get_string(&args, "title").unwrap_or_default();
        let slides_value = args
            .get("slides")
            .and_then(|v| v.as_array())
            .context("slides 参数必须是数组")?;
        if slides_value.is_empty() {
            anyhow::bail!("slides 参数不能为空数组");
        }

        let mut slides = Vec::new();
        for slide_value in slides_value {
            let spec: PptSlide =
                serde_json::from_value(slide_value.clone()).context("slides 元素解析失败")?;
            slides.push(build_slide(&spec, &ctx.working_dir)?);
        }

        // 主题与字体
        let mut settings = PresentationSettings::default();
        let theme_name = get_string(&args, "theme").unwrap_or_else(|_| "office".to_string());
        let mut theme =
            resolve_theme(&theme_name).with_context(|| format!("未知主题: {theme_name}"))?;
        if let Ok(title_font) = get_string(&args, "title_font") {
            theme = theme.major_font(title_font);
        }
        if let Ok(body_font) = get_string(&args, "body_font") {
            theme = theme.minor_font(body_font);
        }
        settings.theme = Some(theme);

        let bytes = if let Ok(template) = get_string(&args, "template") {
            let template_path = resolve_path(&ctx.working_dir, &template)?;
            create_pptx_with_template(
                &title,
                &slides,
                &template_path.to_string_lossy(),
                Some(settings),
            )
            .map_err(|e| anyhow::anyhow!("基于模板生成 PPT 失败: {e}"))?
        } else {
            create_pptx_with_settings(&title, &slides, Some(settings))
                .map_err(|e| anyhow::anyhow!("生成 PPT 失败: {e}"))?
        };

        std::fs::write(&path, bytes)
            .with_context(|| format!("写入文件失败: {}", path.display()))?;
        Ok(ToolResult::Text(format!(
            "已保存: {}（{} 张幻灯片）",
            path.display(),
            slides.len()
        )))
    }
}

/// 解析主题预设。
fn resolve_theme(name: &str) -> Option<PresentationTheme> {
    Some(match name {
        "office" => PresentationTheme::office(),
        "corporate" => PresentationTheme::corporate(),
        "modern" => PresentationTheme::modern(),
        "vibrant" => PresentationTheme::vibrant(),
        "dark" => PresentationTheme::dark(),
        "nature" => PresentationTheme::nature(),
        "tech" => PresentationTheme::tech(),
        "carbon" => PresentationTheme::carbon(),
        _ => return None,
    })
}

/// 按规格构建单张幻灯片。
fn build_slide(spec: &PptSlide, working_dir: &std::path::Path) -> Result<SlideContent> {
    let layout = match spec.layout.as_deref().unwrap_or("title_content") {
        "title" => SlideLayout::CenteredTitle,
        "two_content" => SlideLayout::TwoColumn,
        "section" => SlideLayout::SectionHeader,
        "title_only" => SlideLayout::TitleOnly,
        "blank" => SlideLayout::Blank,
        _ => SlideLayout::TitleAndContent,
    };

    let mut slide = SlideContent::new(spec.title.as_deref().unwrap_or("")).layout(layout);
    if let Some(color) = &spec.title_color {
        slide = slide.title_color(color.trim_start_matches('#'));
    }
    if let Some(color) = &spec.content_color {
        slide = slide.content_color(color.trim_start_matches('#'));
    }
    if let Some(size) = spec.title_size {
        slide = slide.title_size(size);
    }
    if let Some(size) = spec.content_size {
        slide = slide.content_size(size);
    }

    if let Some(bullets) = &spec.bullets {
        for bullet in bullets {
            if let Some(text) = bullet.as_str() {
                slide = slide.add_bullet(text);
            } else if bullet.is_object() {
                let text = bullet
                    .get("text")
                    .and_then(|v| v.as_str())
                    .context("bullets 元素缺少 text 字段")?;
                let level = bullet.get("level").and_then(|v| v.as_u64()).unwrap_or(0);
                slide = if level > 0 {
                    slide.add_sub_bullet(text)
                } else {
                    slide.add_bullet(text)
                };
            } else {
                anyhow::bail!("bullets 元素必须是字符串或对象");
            }
        }
    }

    // 副标题：title 版式下置于标题下方的居中透明文本框
    if let Some(subtitle) = &spec.subtitle {
        slide.shapes.push(
            Shape::new(ShapeType::Rectangle, 457200, 4_114_800, 8_230_200, 914400)
                .with_text(subtitle),
        );
    }

    // 文本框
    if let Some(text_boxes) = &spec.text_boxes {
        for tb in text_boxes {
            slide.shapes.push(
                Shape::new(
                    ShapeType::Rectangle,
                    inches_to_emu(tb.x),
                    inches_to_emu(tb.y),
                    inches_to_emu(tb.width),
                    inches_to_emu(tb.height),
                )
                .with_text(&tb.text),
            );
        }
    }

    // 图片
    if let Some(images) = &spec.images {
        for img in images {
            let img_path = resolve_path(working_dir, &img.path)?;
            let mut image = Image::from_path(&img_path)
                .map_err(|e| anyhow::anyhow!("读取图片失败 {}: {e}", img_path.display()))?;
            if let Some(width) = img.width {
                image = image.scale_to_width(inches_to_emu(width));
            }
            image = image.position(
                inches_to_emu(img.x.unwrap_or(1.0)),
                inches_to_emu(img.y.unwrap_or(2.0)),
            );
            slide = slide.add_image(image);
        }
    }

    if let Some(notes) = &spec.notes {
        slide = slide.notes(notes);
    }

    Ok(slide)
}

/// `office_read_ppt` 工具：提取演示文稿的幻灯片数量、标题与文本内容。
pub struct ReadPptTool;

#[async_trait]
impl Tool for ReadPptTool {
    fn name(&self) -> &str {
        "office_read_ppt"
    }

    fn description(&self) -> &str {
        "读取 PowerPoint (.pptx) 演示文稿，返回幻灯片数量、每页标题与文本内容（JSON）。"
    }

    fn schema(&self) -> ToolSchema {
        ToolSchema::new("office_read_ppt", "读取 PPT").with_string("path", ".pptx 文件路径", true)
    }

    async fn execute(&self, args: HashMap<String, Value>, ctx: &ToolContext) -> Result<ToolResult> {
        let path_str = get_string(&args, "path")?;
        let path = resolve_path(&ctx.working_dir, &path_str)?;

        let reader = PresentationReader::open(&path.to_string_lossy())
            .map_err(|e| anyhow::anyhow!("解析 PPT 失败 {}: {e}", path.display()))?;

        let mut slides = Vec::new();
        for (index, slide) in reader
            .get_all_slides()
            .map_err(|e| anyhow::anyhow!("读取幻灯片失败: {e}"))?
            .iter()
            .enumerate()
        {
            let mut texts: Vec<String> = slide.body_text.clone();
            for shape in &slide.shapes {
                if !shape.is_title && !shape.is_body {
                    let text = shape.text();
                    if !text.is_empty() {
                        texts.push(text);
                    }
                }
            }
            slides.push(json!({
                "index": index + 1,
                "title": slide.title,
                "texts": texts,
            }));
        }

        Ok(ToolResult::Json(json!({
            "title": reader.info().title,
            "slide_count": reader.slide_count(),
            "slides": slides,
        })))
    }
}

/// 相对路径基于工作目录解析，绝对路径原样返回。
fn resolve_path(working_dir: &std::path::Path, input: &str) -> Result<PathBuf> {
    let path = PathBuf::from(input);
    Ok(if path.is_absolute() {
        path
    } else {
        working_dir.join(path)
    })
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

    /// 在临时目录生成一张测试用 PNG。
    fn make_png(dir: &TempDir, name: &str) {
        let img = image::RgbImage::from_pixel(8, 8, image::Rgb([255, 0, 0]));
        img.save(dir.path().join(name)).unwrap();
    }

    #[tokio::test]
    async fn test_write_and_read_ppt() {
        let dir = TempDir::new().unwrap();
        make_png(&dir, "pic.png");
        let tool = WritePptTool;
        let mut args = HashMap::new();
        args.insert("path".to_string(), Value::String("test.pptx".to_string()));
        args.insert("title".to_string(), Value::String("季度汇报".to_string()));
        args.insert("theme".to_string(), Value::String("corporate".to_string()));
        args.insert(
            "slides".to_string(),
            json!([
                {"layout": "title", "title": "季度汇报", "subtitle": "2026 Q2"},
                {"layout": "title_content", "title": "要点", "bullets": ["营收增长", {"text": "成本下降", "level": 1}]},
                {"layout": "two_content", "title": "对比", "bullets": ["左栏", "右栏"]},
                {"layout": "blank", "text_boxes": [{"x": 1.0, "y": 1.0, "width": 4.0, "height": 1.0, "text": "自由文本"}]},
                {"layout": "title_only", "title": "配图", "images": [{"path": "pic.png", "x": 2.0, "y": 2.0, "width": 3.0}]}
            ]),
        );
        let result = tool.execute(args, &ctx(&dir)).await.unwrap();
        assert!(result.to_string_for_model().contains("5 张幻灯片"));

        let read_tool = ReadPptTool;
        let mut args = HashMap::new();
        args.insert("path".to_string(), Value::String("test.pptx".to_string()));
        let result = read_tool.execute(args, &ctx(&dir)).await.unwrap();
        let ToolResult::Json(value) = result else {
            panic!("应返回 JSON");
        };
        assert_eq!(value["slide_count"].as_u64().unwrap(), 5);
        let slides = value["slides"].as_array().unwrap();
        assert_eq!(slides[0]["title"].as_str().unwrap(), "季度汇报");
        assert!(
            slides[1]["texts"]
                .as_array()
                .unwrap()
                .iter()
                .any(|t| t.as_str().unwrap().contains("营收增长"))
        );
        // blank 版式上的文本框
        assert!(
            slides[3]["texts"]
                .as_array()
                .unwrap()
                .iter()
                .any(|t| t.as_str().unwrap().contains("自由文本"))
        );
    }

    #[tokio::test]
    async fn test_write_ppt_with_template() {
        let dir = TempDir::new().unwrap();

        // 先生成一个模板文件
        let tool = WritePptTool;
        let mut args = HashMap::new();
        args.insert("path".to_string(), Value::String("tpl.pptx".to_string()));
        args.insert(
            "slides".to_string(),
            json!([{"layout": "title_content", "title": "模板页", "bullets": ["占位"]}]),
        );
        tool.execute(args, &ctx(&dir)).await.unwrap();

        // 以该文件为模板生成新演示文稿
        let mut args = HashMap::new();
        args.insert(
            "path".to_string(),
            Value::String("from_tpl.pptx".to_string()),
        );
        args.insert(
            "template".to_string(),
            Value::String("tpl.pptx".to_string()),
        );
        args.insert(
            "slides".to_string(),
            json!([
                {"layout": "title_content", "title": "新页面一", "bullets": ["内容一"]},
                {"layout": "title_content", "title": "新页面二", "bullets": ["内容二"]}
            ]),
        );
        let result = tool.execute(args, &ctx(&dir)).await.unwrap();
        assert!(result.to_string_for_model().contains("2 张幻灯片"));

        let read_tool = ReadPptTool;
        let mut args = HashMap::new();
        args.insert(
            "path".to_string(),
            Value::String("from_tpl.pptx".to_string()),
        );
        let result = read_tool.execute(args, &ctx(&dir)).await.unwrap();
        let ToolResult::Json(value) = result else {
            panic!("应返回 JSON");
        };
        assert_eq!(value["slide_count"].as_u64().unwrap(), 2);
        let slides = value["slides"].as_array().unwrap();
        assert_eq!(slides[0]["title"].as_str().unwrap(), "新页面一");
    }

    #[tokio::test]
    async fn test_write_ppt_unknown_theme() {
        let dir = TempDir::new().unwrap();
        let tool = WritePptTool;
        let mut args = HashMap::new();
        args.insert("path".to_string(), Value::String("bad.pptx".to_string()));
        args.insert(
            "theme".to_string(),
            Value::String("nonexistent".to_string()),
        );
        args.insert("slides".to_string(), json!([{"layout": "blank"}]));
        assert!(tool.execute(args, &ctx(&dir)).await.is_err());
    }

    #[tokio::test]
    async fn test_write_ppt_empty_slides() {
        let dir = TempDir::new().unwrap();
        let tool = WritePptTool;
        let mut args = HashMap::new();
        args.insert("path".to_string(), Value::String("empty.pptx".to_string()));
        args.insert("slides".to_string(), json!([]));
        assert!(tool.execute(args, &ctx(&dir)).await.is_err());
    }

    #[tokio::test]
    async fn test_read_ppt_missing_file() {
        let dir = TempDir::new().unwrap();
        let tool = ReadPptTool;
        let mut args = HashMap::new();
        args.insert(
            "path".to_string(),
            Value::String("missing.pptx".to_string()),
        );
        assert!(tool.execute(args, &ctx(&dir)).await.is_err());
    }
}
