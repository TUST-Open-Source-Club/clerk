use crate::skills::parser::Skill;

/// 根据用户输入选择相关的 Skills 并生成注入文本
pub fn select_skills<'a>(skills: &'a [Skill], input: &str, top_k: usize) -> Vec<&'a Skill> {
    let mut scored: Vec<(&Skill, f32)> = skills
        .iter()
        .map(|s| (s, s.score_relevance(input)))
        .filter(|(_, score)| *score > 0.0)
        .collect();
    scored.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap());
    scored.into_iter().take(top_k).map(|(s, _)| s).collect()
}

/// 将选中的 Skills 格式化为系统提示词追加内容
pub fn build_skill_prompt(skills: &[Skill]) -> String {
    if skills.is_empty() {
        return String::new();
    }

    let mut prompt = String::from("\n\n以下是与当前任务相关的 Skill：\n");
    for skill in skills {
        prompt.push_str(&format!("\n## Skill: {}\n", skill.meta.name));
        prompt.push_str(&skill.format_for_prompt());
    }
    prompt
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::skills::parser::parse;

    fn make_skill(name: &str, tags: &[&str]) -> Skill {
        let tags_yaml = tags.join("\n  - ");
        let content = format!("---\nname: {}\ntags:\n  - {}\n---\n", name, tags_yaml);
        parse(&content).unwrap()
    }

    #[test]
    fn test_select_skills() {
        let excel = make_skill("excel", &["excel"]);
        let word = make_skill("word", &["word"]);
        let skills = vec![excel, word];

        let selected = select_skills(&skills, "分析 excel 数据", 2);
        assert_eq!(selected.len(), 1);
        assert_eq!(selected[0].meta.name, "excel");
    }

    #[test]
    fn test_build_skill_prompt() {
        let skill = make_skill("test", &["test"]);
        let prompt = build_skill_prompt(&[skill]);
        assert!(prompt.contains("Skill: test"));
    }
}
