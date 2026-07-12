use anyhow::Result;
use std::fs;
use std::path::Path;
use tracing::{info, warn};

use crate::skills::parser::{Skill, parse};

pub struct SkillLoader;

impl SkillLoader {
    /// 加载所有作用域的 Skills
    pub fn load_all(
        builtin_dir: &Path,
        user_dir: Option<&Path>,
        project_dir: Option<&Path>,
    ) -> Result<Vec<Skill>> {
        let mut skills = Vec::new();

        // 优先级由低到高：内置 -> 用户级 -> 项目级
        Self::load_from_dir(builtin_dir, &mut skills)?;

        if let Some(dir) = user_dir {
            Self::load_from_dir(dir, &mut skills)?;
        }

        if let Some(dir) = project_dir {
            Self::load_from_dir(dir, &mut skills)?;
            // 同时支持项目根目录的 AGENTS.md
            if let Some(agents) = Self::load_agents_md(dir) {
                skills.push(agents);
            }
        }

        Ok(skills)
    }

    pub fn load_from_dir(dir: &Path, skills: &mut Vec<Skill>) -> Result<()> {
        if !dir.exists() {
            return Ok(());
        }

        for entry in fs::read_dir(dir)? {
            let entry = entry?;
            let path = entry.path();
            if path.is_file() && path.file_name().map(|n| n == "SKILL.md").unwrap_or(false) {
                match Self::load_file(&path) {
                    Ok(skill) => skills.push(skill),
                    Err(e) => warn!("加载 SKILL.md 失败 {}: {}", path.display(), e),
                }
            } else if path.is_dir() {
                // 支持 skills/xxx/SKILL.md 结构
                let skill_md = path.join("SKILL.md");
                if skill_md.exists() {
                    match Self::load_file(&skill_md) {
                        Ok(skill) => skills.push(skill),
                        Err(e) => warn!("加载 SKILL.md 失败 {}: {}", skill_md.display(), e),
                    }
                }
            }
        }

        Ok(())
    }

    pub fn load_file(path: &Path) -> Result<Skill> {
        info!("加载 Skill: {}", path.display());
        let content = fs::read_to_string(path)?;
        let mut skill = parse(&content)?;
        skill.source_path = Some(path.to_path_buf());
        Ok(skill)
    }

    fn load_agents_md(project_dir: &Path) -> Option<Skill> {
        let path = project_dir.parent()?.join("AGENTS.md");
        if !path.exists() {
            return None;
        }
        match Self::load_file(&path) {
            Ok(mut skill) => {
                if skill.meta.name.is_empty() {
                    skill.meta.name = "project".to_string();
                }
                Some(skill)
            }
            Err(_) => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn test_load_from_dir() {
        let dir = TempDir::new().unwrap();
        fs::write(dir.path().join("SKILL.md"), "---\nname: test\n---\nbody\n").unwrap();

        let mut skills = Vec::new();
        SkillLoader::load_from_dir(dir.path(), &mut skills).unwrap();
        assert_eq!(skills.len(), 1);
        assert_eq!(skills[0].meta.name, "test");
    }

    #[test]
    fn test_load_from_missing_dir() {
        let mut skills = Vec::new();
        SkillLoader::load_from_dir(Path::new("/nonexistent"), &mut skills).unwrap();
        assert!(skills.is_empty());
    }

    #[test]
    fn test_load_from_nested_skill_dir() {
        let dir = TempDir::new().unwrap();
        let skill_dir = dir.path().join("excel");
        fs::create_dir(&skill_dir).unwrap();
        fs::write(skill_dir.join("SKILL.md"), "---\nname: excel\n---\n").unwrap();

        let mut skills = Vec::new();
        SkillLoader::load_from_dir(dir.path(), &mut skills).unwrap();
        assert_eq!(skills.len(), 1);
        assert_eq!(skills[0].meta.name, "excel");
    }

    #[test]
    fn test_load_file_failure() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("bad.md");
        fs::write(&path, "---\nunclosed frontmatter").unwrap();
        assert!(SkillLoader::load_file(&path).is_err());
    }

    #[test]
    fn test_load_all_without_user_dir() {
        let builtin = TempDir::new().unwrap();
        let project = TempDir::new().unwrap();
        fs::write(builtin.path().join("SKILL.md"), "---\nname: builtin\n---\n").unwrap();
        fs::write(project.path().join("SKILL.md"), "---\nname: project\n---\n").unwrap();

        let skills = SkillLoader::load_all(builtin.path(), None, Some(project.path())).unwrap();
        assert_eq!(skills.len(), 3);
    }

    #[test]
    fn test_load_agents_md() {
        let project = TempDir::new().unwrap();
        let project_root = project.path().parent().unwrap();
        fs::write(project_root.join("AGENTS.md"), "---\nname: agents\n---\n").unwrap();

        let agents = SkillLoader::load_agents_md(project.path());
        assert!(agents.is_some());
        assert_eq!(agents.unwrap().meta.name, "agents");
    }
}
