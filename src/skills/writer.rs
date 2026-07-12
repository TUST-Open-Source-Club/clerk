use anyhow::{Context, Result};
use std::fs;
use std::path::{Path, PathBuf};

/// 将新的 Skill 写入项目 skills 目录。
pub struct SkillWriter;

impl SkillWriter {
    /// 写入 `<base_dir>/skills/<name>/SKILL.md`。
    /// `name` 会被清理为仅包含字母、数字、下划线、中划线的字符串。
    pub fn write(base_dir: &Path, name: &str, content: &str) -> Result<PathBuf> {
        let dir = base_dir.join("skills").join(Self::sanitize_name(name));
        fs::create_dir_all(&dir)
            .with_context(|| format!("创建 Skill 目录失败: {}", dir.display()))?;

        let path = dir.join("SKILL.md");
        fs::write(&path, content)
            .with_context(|| format!("写入 SKILL.md 失败: {}", path.display()))?;
        Ok(path)
    }

    fn sanitize_name(name: &str) -> String {
        let trimmed = name.trim();
        if trimmed.is_empty() {
            return "untitled".to_string();
        }
        let sanitized: String = trimmed
            .chars()
            .map(|c| {
                if c.is_alphanumeric() || c == '-' || c == '_' {
                    c
                } else {
                    '_'
                }
            })
            .collect();
        if sanitized.chars().next().unwrap().is_numeric() {
            format!("skill_{}", sanitized)
        } else {
            sanitized
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn test_write_skill() {
        let dir = TempDir::new().unwrap();
        let path = SkillWriter::write(dir.path(), "excel-analysis", "# Excel\n").unwrap();
        assert!(path.exists());
        assert!(
            path.to_string_lossy()
                .contains("skills/excel-analysis/SKILL.md")
        );
    }

    #[test]
    fn test_sanitize_special_chars() {
        assert_eq!(SkillWriter::sanitize_name("hello world!"), "hello_world_");
        assert_eq!(SkillWriter::sanitize_name("  "), "untitled");
        assert_eq!(SkillWriter::sanitize_name("1num"), "skill_1num");
    }
}
