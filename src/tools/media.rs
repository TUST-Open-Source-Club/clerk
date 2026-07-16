use anyhow::{Context, Result};
use async_trait::async_trait;
use serde_json::Value;
use std::collections::HashMap;
use std::io::Cursor;
use std::path::{Path, PathBuf};

use crate::tools::schema::{Tool, ToolContext, ToolResult, ToolSchema, get_string};
use crate::util::expand_tilde;
use base64::{Engine as _, engine::general_purpose::STANDARD};
use uuid::Uuid;

/// 超过该大小的图片会先压缩再内联为 base64
const MAX_INLINE_SIZE: u64 = 2 * 1024 * 1024;
/// 压缩后图片的最大边长（像素）
const MAX_INLINE_DIMENSION: u32 = 1024;

/// `read_media_file` 工具：读取图片/视频，返回元信息与 base64 数据 URL。
pub struct ReadMediaFile;

#[async_trait]
impl Tool for ReadMediaFile {
    fn name(&self) -> &str {
        "read_media_file"
    }

    fn description(&self) -> &str {
        "读取图片或视频文件，返回格式、尺寸和 base64 数据 URL。"
    }

    fn schema(&self) -> ToolSchema {
        ToolSchema::new("read_media_file", "读取媒体文件").with_string(
            "path",
            "相对于工作目录的媒体文件路径",
            true,
        )
    }

    async fn execute(&self, args: HashMap<String, Value>, ctx: &ToolContext) -> Result<ToolResult> {
        let path_str = get_string(&args, "path")?;
        let path = resolve_path(&ctx.working_dir, &path_str)?;
        let description = read_media_file(&path).await?;
        Ok(ToolResult::Text(description))
    }
}

/// 读取媒体文件并生成包含元信息和 base64 数据 URL 的描述文本。
pub async fn read_media_file(path: &Path) -> Result<String> {
    if !path.exists() {
        anyhow::bail!("文件不存在: {}", path.display());
    }

    let metadata = tokio::fs::metadata(path)
        .await
        .with_context(|| format!("读取文件元信息失败: {}", path.display()))?;
    if metadata.is_dir() {
        anyhow::bail!("路径是目录: {}", path.display());
    }

    let kind: Option<String> = infer::get_from_path(path)
        .ok()
        .flatten()
        .map(|k| k.mime_type().to_string())
        .or_else(|| guess_kind_from_extension(path));

    match kind.as_deref() {
        Some(mime) if mime.starts_with("image/") => describe_image(path).await,
        Some(mime) if mime.starts_with("video/") => describe_video(path).await,
        _ => Ok(ToolResult::Error("不支持的媒体类型".to_string()).to_string_for_model()),
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

/// 按扩展名猜测 MIME 类型，作为 infer 检测失败时的回退。
fn guess_kind_from_extension(path: &Path) -> Option<String> {
    let ext = path.extension().and_then(|e| e.to_str())?.to_lowercase();
    match ext.as_str() {
        "bmp" => Some("image/bmp".to_string()),
        "png" => Some("image/png".to_string()),
        "jpg" | "jpeg" => Some("image/jpeg".to_string()),
        "gif" => Some("image/gif".to_string()),
        "webp" => Some("image/webp".to_string()),
        "mp4" => Some("video/mp4".to_string()),
        "webm" => Some("video/webm".to_string()),
        "mov" => Some("video/quicktime".to_string()),
        "avi" => Some("video/x-msvideo".to_string()),
        "mkv" => Some("video/x-matroska".to_string()),
        _ => None,
    }
}

/// 生成图片描述：格式、尺寸与 base64 数据 URL；超过大小上限时先压缩再编码为 PNG。
async fn describe_image(path: &Path) -> Result<String> {
    let bytes = tokio::fs::read(path)
        .await
        .with_context(|| format!("读取图片失败: {}", path.display()))?;
    let img = image::load_from_memory(&bytes)
        .with_context(|| format!("解析图片失败: {}", path.display()))?;
    let (width, height) = (img.width(), img.height());

    let (mime, encoded) = if bytes.len() as u64 > MAX_INLINE_SIZE {
        let resized = resize_image(img);
        let mut buf = Vec::new();
        resized
            .write_to(&mut Cursor::new(&mut buf), image::ImageFormat::Png)
            .context("编码图片失败")?;
        ("image/png".to_string(), STANDARD.encode(&buf))
    } else {
        let mime = infer::get_from_path(path)
            .ok()
            .flatten()
            .map(|k| k.mime_type().to_string())
            .or_else(|| guess_kind_from_extension(path))
            .unwrap_or_else(|| "image/png".to_string());
        (mime, STANDARD.encode(&bytes))
    };

    Ok(format!(
        "图片格式: {}，尺寸: {}x{}，数据 URL: data:{};base64,{}",
        mime, width, height, mime, encoded
    ))
}

/// 等比缩小图片，使最长边不超过 MAX_INLINE_DIMENSION。
fn resize_image(img: image::DynamicImage) -> image::DynamicImage {
    let (w, h) = (img.width(), img.height());
    let ratio = f64::from(MAX_INLINE_DIMENSION) / f64::from(w.max(h));
    if ratio >= 1.0 {
        return img;
    }
    let new_w = (f64::from(w) * ratio) as u32;
    let new_h = (f64::from(h) * ratio) as u32;
    img.resize(new_w, new_h, image::imageops::FilterType::Lanczos3)
}

/// 生成视频描述：用 ffmpeg 提取元信息，并抽取第 1 秒首帧作为 base64 预览图。
async fn describe_video(path: &Path) -> Result<String> {
    if !command_exists("ffmpeg").await {
        return Ok(
            ToolResult::Error("未安装 ffmpeg，无法提取视频信息".to_string()).to_string_for_model(),
        );
    }

    let probe = tokio::process::Command::new("ffmpeg")
        .arg("-i")
        .arg(path)
        .output()
        .await
        .context("运行 ffmpeg 失败")?;
    let metadata = String::from_utf8_lossy(&probe.stderr).to_string();

    let frame_path = std::env::temp_dir().join(format!("clerk_media_frame_{}.png", Uuid::new_v4()));
    let output = tokio::process::Command::new("ffmpeg")
        .arg("-i")
        .arg(path)
        .arg("-ss")
        .arg("00:00:01")
        .arg("-vframes")
        .arg("1")
        .arg(&frame_path)
        .output()
        .await
        .context("提取视频帧失败")?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Ok(ToolResult::Error(format!("提取视频帧失败: {}", stderr)).to_string_for_model());
    }

    let frame_bytes = tokio::fs::read(&frame_path)
        .await
        .context("读取视频帧失败")?;
    let frame_b64 = STANDARD.encode(&frame_bytes);

    Ok(format!(
        "视频元信息:\n{}\n首帧预览: data:image/png;base64,{}",
        metadata, frame_b64
    ))
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

    fn create_png(path: &Path) {
        let img = image::RgbImage::new(10, 10);
        img.save(path).unwrap();
    }

    #[tokio::test]
    async fn test_read_image_png() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("test.png");
        create_png(&path);

        let tool = ReadMediaFile;
        let mut args = HashMap::new();
        args.insert("path".to_string(), Value::String("test.png".to_string()));
        let result = tool.execute(args, &ctx(&dir)).await.unwrap();
        let text = result.to_string_for_model();
        assert!(text.contains("image/png"));
        assert!(text.contains("10x10"));
        assert!(text.contains("data:image/png;base64,"));
    }

    #[tokio::test]
    async fn test_read_missing_file() {
        let dir = TempDir::new().unwrap();
        let tool = ReadMediaFile;
        let mut args = HashMap::new();
        args.insert("path".to_string(), Value::String("missing.png".to_string()));
        let result = tool.execute(args, &ctx(&dir)).await;
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(err.contains("文件不存在"));
    }

    #[tokio::test]
    async fn test_read_unsupported_file() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("test.txt");
        tokio::fs::write(&path, "hello").await.unwrap();

        let tool = ReadMediaFile;
        let mut args = HashMap::new();
        args.insert("path".to_string(), Value::String("test.txt".to_string()));
        let result = tool.execute(args, &ctx(&dir)).await.unwrap();
        let text = result.to_string_for_model();
        assert!(text.contains("不支持的媒体类型"));
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

    #[tokio::test]
    async fn test_read_directory_returns_error() {
        let dir = TempDir::new().unwrap();
        let sub = dir.path().join("subdir");
        tokio::fs::create_dir(&sub).await.unwrap();

        let result = read_media_file(&sub).await;
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("路径是目录"));
    }

    #[tokio::test]
    async fn test_read_video_without_ffmpeg() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("clip.mov");
        tokio::fs::write(&path, b"not a real video").await.unwrap();

        let result = read_media_file(&path).await.unwrap();
        // 无论 ffmpeg 是否安装，都会进入视频分支；若未安装则返回提示。
        assert!(
            result.contains("ffmpeg") || result.contains("首帧预览"),
            "unexpected result: {}",
            result
        );
    }

    #[tokio::test]
    async fn test_read_large_image_resized() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("large.bmp");
        let img = image::RgbImage::from_fn(2000, 2000, |x, y| {
            image::Rgb([
                (x % 256) as u8,
                (y % 256) as u8,
                ((x / 256) + (y / 256)) as u8,
            ])
        });
        img.save(&path).unwrap();
        assert!(std::fs::metadata(&path).unwrap().len() > MAX_INLINE_SIZE);

        let result = read_media_file(&path).await.unwrap();
        assert!(result.contains("2000x2000"));
        assert!(result.contains("data:image/png;base64,"));
    }

    #[test]
    fn test_resize_image_noop_for_small_image() {
        let img = image::DynamicImage::new_rgb8(100, 100);
        let resized = resize_image(img);
        assert_eq!(resized.width(), 100);
        assert_eq!(resized.height(), 100);
    }

    #[test]
    fn test_resize_image_downscales_large_image() {
        let img = image::DynamicImage::new_rgb8(3000, 2000);
        let resized = resize_image(img);
        assert!(resized.width() <= MAX_INLINE_DIMENSION);
        assert!(resized.height() <= MAX_INLINE_DIMENSION);
        assert!(resized.width() > resized.height() || resized.height() == MAX_INLINE_DIMENSION);
    }
}
