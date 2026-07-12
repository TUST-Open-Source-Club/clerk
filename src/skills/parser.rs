use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct SkillMeta {
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub tags: Vec<String>,
    #[serde(default)]
    pub system_prompt: String,
    #[serde(default)]
    pub tools: Vec<String>,
}

#[derive(Debug, Clone)]
pub struct Skill {
    pub meta: SkillMeta,
    pub examples: Vec<SkillExample>,
    pub body: String,
    pub source_path: Option<std::path::PathBuf>,
}

#[derive(Debug, Clone)]
pub struct SkillExample {
    pub user: String,
    pub assistant: String,
}

pub fn parse(content: &str) -> Result<Skill> {
    let (meta, body) = split_frontmatter(content)?;
    let meta: SkillMeta = if meta.trim().is_empty() {
        SkillMeta::default()
    } else {
        serde_yaml::from_str(meta).context("解析 SKILL.md Frontmatter 失败")?
    };

    let examples = extract_examples(body);

    Ok(Skill {
        meta,
        examples,
        body: body.to_string(),
        source_path: None,
    })
}

fn split_frontmatter(content: &str) -> Result<(&str, &str)> {
    let trimmed = content.trim_start();
    if !trimmed.starts_with("---") {
        return Ok(("", trimmed));
    }

    let after_open = &trimmed[3..];
    match after_open.find("\n---") {
        Some(idx) => {
            let meta = &after_open[..idx];
            let body = &after_open[idx + 4..];
            Ok((meta, body.trim_start()))
        }
        None => Err(anyhow::anyhow!("SKILL.md Frontmatter 未正确闭合")),
    }
}

fn extract_examples(body: &str) -> Vec<SkillExample> {
    let mut examples = Vec::new();
    let mut current_user: Option<String> = None;
    let mut current_assistant: Option<String> = None;

    for line in body.lines() {
        if line.starts_with("### User:") || line.starts_with("### 用户:") {
            if let (Some(u), Some(a)) = (current_user.take(), current_assistant.take()) {
                examples.push(SkillExample {
                    user: u,
                    assistant: a,
                });
            }
            current_user = Some(String::new());
        } else if line.starts_with("### Assistant:") || line.starts_with("### 助手:") {
            current_assistant = Some(String::new());
        } else if let Some(user) = current_user.as_mut() {
            user.push_str(line);
            user.push('\n');
        } else if let Some(assistant) = current_assistant.as_mut() {
            assistant.push_str(line);
            assistant.push('\n');
        }
    }

    if let (Some(u), Some(a)) = (current_user.take(), current_assistant.take()) {
        examples.push(SkillExample {
            user: u.trim().to_string(),
            assistant: a.trim().to_string(),
        });
    }

    examples
}

impl Skill {
    pub fn format_for_prompt(&self) -> String {
        let mut prompt = String::new();
        if !self.meta.system_prompt.is_empty() {
            prompt.push_str(&self.meta.system_prompt);
            prompt.push('\n');
        }
        if !self.examples.is_empty() {
            prompt.push_str("\n示例:\n");
            for (i, ex) in self.examples.iter().enumerate() {
                prompt.push_str(&format!("### 示例 {}\n", i + 1));
                prompt.push_str(&format!("用户: {}\n", ex.user));
                prompt.push_str(&format!("助手: {}\n", ex.assistant));
            }
        }
        prompt
    }

    pub fn score_relevance(&self, input: &str) -> f32 {
        let input_lower = input.to_lowercase();
        let mut score = 0.0f32;

        for tag in &self.meta.tags {
            if !tag.is_empty() && input_lower.contains(&tag.to_lowercase()) {
                score += 1.0;
            }
        }

        if !self.meta.name.is_empty() && input_lower.contains(&self.meta.name.to_lowercase()) {
            score += 1.5;
        }

        if !self.meta.description.is_empty()
            && input_lower.contains(&self.meta.description.to_lowercase())
        {
            score += 0.5;
        }

        score
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_with_frontmatter() {
        let content = r#"---
name: excel
system_prompt: 你擅长 Excel 数据分析。
tags:
  - excel
  - data
---

# Excel Skill

### User:
分析这份数据
### Assistant:
好的，我先读取文件。
"#;

        let skill = parse(content).unwrap();
        assert_eq!(skill.meta.name, "excel");
        assert_eq!(skill.meta.tags, vec!["excel", "data"]);
        assert_eq!(skill.examples.len(), 1);
        assert!(skill.body.contains("Excel Skill"));
    }

    #[test]
    fn test_format_for_prompt() {
        let content = r#"---
name: test
system_prompt: sys
---
### User:
q
### Assistant:
a
"#;
        let skill = parse(content).unwrap();
        let prompt = skill.format_for_prompt();
        assert!(prompt.contains("sys"));
        assert!(prompt.contains("用户: q"));
    }

    #[test]
    fn test_relevance_score() {
        let content = "---\nname: excel\ntags:\n  - data\n---\n";
        let skill = parse(content).unwrap();
        assert!(skill.score_relevance("分析 excel 数据") > 0.0);
    }
}
