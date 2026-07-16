//! 媒体附件的公共逻辑：类型识别与附件描述拼接，TUI 与 GUI 共用。

use std::path::{Path, PathBuf};

use crate::tools::media::read_media_file;

/// 判断文件是图片还是视频（infer 检测 MIME，失败时按扩展名回退）。
pub fn media_kind(path: &Path) -> Option<&'static str> {
    let mime = infer::get_from_path(path)
        .ok()
        .flatten()
        .map(|k| k.mime_type().to_string())
        .or_else(|| {
            let ext = path.extension().and_then(|e| e.to_str())?.to_lowercase();
            match ext.as_str() {
                "png" | "jpg" | "jpeg" | "gif" | "webp" => Some("image/unknown".to_string()),
                "mp4" | "webm" | "mov" | "avi" | "mkv" => Some("video/unknown".to_string()),
                _ => None,
            }
        })?;
    if mime.starts_with("image/") {
        Some("image")
    } else if mime.starts_with("video/") {
        Some("video")
    } else {
        None
    }
}

/// 将附件读取结果拼接到用户消息文本之后：
/// 每个附件读取为包含 base64 数据 URL 的描述文本，读取失败时附带错误说明。
pub async fn with_attachments(text: String, attachments: &[PathBuf]) -> String {
    if attachments.is_empty() {
        return text;
    }

    let mut descriptions = Vec::new();
    for path in attachments {
        match read_media_file(path).await {
            Ok(desc) => descriptions.push(format!("附件 {}:\n{}", path.display(), desc)),
            Err(e) => descriptions.push(format!("附件 {} 读取失败: {}", path.display(), e)),
        }
    }
    format!("{}\n\n{}", text, descriptions.join("\n\n"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_media_kind_by_extension() {
        assert_eq!(media_kind(Path::new("/tmp/a.png")), Some("image"));
        assert_eq!(media_kind(Path::new("/tmp/a.mp4")), Some("video"));
        assert_eq!(media_kind(Path::new("/tmp/a.txt")), None);
    }

    #[tokio::test]
    async fn test_with_attachments_empty_returns_text() {
        let text = with_attachments("hello".to_string(), &[]).await;
        assert_eq!(text, "hello");
    }

    #[tokio::test]
    async fn test_with_attachments_missing_file_reports_error() {
        let text = with_attachments(
            "hello".to_string(),
            &[PathBuf::from("/nonexistent/pic.png")],
        )
        .await;
        assert!(text.starts_with("hello"));
        assert!(text.contains("读取失败"));
    }
}
