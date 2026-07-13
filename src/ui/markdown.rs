use pulldown_cmark::{Event, Options, Parser, Tag, TagEnd};
use ratatui::{
    style::{Color, Modifier, Style},
    text::{Line, Span, Text},
};

/// 将 Markdown 文本转换为带样式的 ratatui `Text`。
/// 支持 `<think>...</think>` 标签，标签内文本以灰色显示，用于展示模型思考过程。
/// 如果解析失败或内容为空，则退回到纯文本显示。
pub fn markdown_to_text(input: &str) -> Text<'_> {
    let mut all_lines: Vec<Line> = Vec::new();
    let mut rest = input;

    loop {
        match rest.split_once("<think>") {
            Some((before, after)) => {
                if !before.is_empty() {
                    all_lines.extend(render_markdown(before).lines);
                }
                let (thinking, remaining) = after.split_once("</think>").unwrap_or((after, ""));
                if !thinking.is_empty() {
                    for line in thinking.lines() {
                        all_lines.push(Line::from(Span::styled(
                            line.to_string(),
                            Style::default().fg(Color::Gray),
                        )));
                    }
                }
                rest = remaining;
            }
            None => {
                if !rest.is_empty() {
                    all_lines.extend(render_markdown(rest).lines);
                }
                break;
            }
        }
    }

    // 去掉末尾由块级元素结束产生的空行
    while let Some(last) = all_lines.last() {
        if last.spans.is_empty() || (last.spans.len() == 1 && last.spans[0].content.is_empty()) {
            all_lines.pop();
        } else {
            break;
        }
    }

    if all_lines.is_empty() {
        all_lines.push(Line::from(input.to_string()));
    }

    Text::from(all_lines)
}

fn render_markdown(input: &str) -> Text<'_> {
    let mut lines: Vec<Line> = Vec::new();
    let mut current_spans: Vec<Span> = Vec::new();
    let mut style = Style::default();

    let options = Options::ENABLE_STRIKETHROUGH
        | Options::ENABLE_TABLES
        | Options::ENABLE_FOOTNOTES
        | Options::ENABLE_TASKLISTS;

    for event in Parser::new_ext(input, options) {
        match event {
            Event::Start(tag) => {
                style = apply_tag_style(style, &tag);
                if is_block_tag(&tag) && !current_spans.is_empty() {
                    lines.push(Line::from(std::mem::take(&mut current_spans)));
                }
            }
            Event::End(tag_end) => {
                style = reset_tag_style(style, &tag_end);
                if is_block_end(&tag_end) && !current_spans.is_empty() {
                    lines.push(Line::from(std::mem::take(&mut current_spans)));
                    lines.push(Line::from(""));
                }
            }
            Event::Text(text) => {
                current_spans.push(Span::styled(text.to_string(), style));
            }
            Event::Code(code) => {
                current_spans.push(Span::styled(
                    code.to_string(),
                    Style::default()
                        .fg(Color::Yellow)
                        .bg(Color::DarkGray)
                        .add_modifier(Modifier::BOLD),
                ));
            }
            Event::SoftBreak | Event::HardBreak => {
                lines.push(Line::from(std::mem::take(&mut current_spans)));
            }
            Event::Rule => {
                lines.push(Line::from("─".repeat(40)));
            }
            _ => {}
        }
    }

    if !current_spans.is_empty() {
        lines.push(Line::from(current_spans));
    }

    Text::from(lines)
}

fn apply_tag_style(style: Style, tag: &Tag) -> Style {
    match tag {
        Tag::Strong | Tag::Heading { .. } => style.add_modifier(Modifier::BOLD),
        Tag::Emphasis => style.add_modifier(Modifier::ITALIC),
        Tag::Strikethrough => style.add_modifier(Modifier::CROSSED_OUT),
        Tag::CodeBlock { .. } => Style::default().fg(Color::Yellow).bg(Color::DarkGray),
        Tag::Link { .. } => style.fg(Color::Blue).add_modifier(Modifier::UNDERLINED),
        _ => style,
    }
}

fn reset_tag_style(style: Style, tag_end: &TagEnd) -> Style {
    match tag_end {
        TagEnd::Strong | TagEnd::Heading(_) => style.remove_modifier(Modifier::BOLD),
        TagEnd::Emphasis => style.remove_modifier(Modifier::ITALIC),
        TagEnd::Strikethrough => style.remove_modifier(Modifier::CROSSED_OUT),
        TagEnd::CodeBlock => Style::default(),
        TagEnd::Link => style,
        _ => style,
    }
}

fn is_block_tag(tag: &Tag) -> bool {
    matches!(
        tag,
        Tag::Paragraph
            | Tag::Heading { .. }
            | Tag::BlockQuote(_)
            | Tag::CodeBlock { .. }
            | Tag::List(_)
            | Tag::Item
            | Tag::FootnoteDefinition(_)
            | Tag::Table(_)
            | Tag::TableHead
            | Tag::TableRow
            | Tag::TableCell
    )
}

fn is_block_end(tag_end: &TagEnd) -> bool {
    matches!(
        tag_end,
        TagEnd::Paragraph
            | TagEnd::Heading(_)
            | TagEnd::BlockQuote(_)
            | TagEnd::CodeBlock
            | TagEnd::List(_)
            | TagEnd::Item
            | TagEnd::FootnoteDefinition
            | TagEnd::Table
            | TagEnd::TableHead
            | TagEnd::TableRow
            | TagEnd::TableCell
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_markdown_bold() {
        let text = markdown_to_text("**bold**");
        let line = text.lines.first().unwrap();
        let span = line.spans.first().unwrap();
        assert_eq!(span.content, "bold");
        assert!(span.style.add_modifier.contains(Modifier::BOLD));
    }

    #[test]
    fn test_markdown_fallback_for_plain_text() {
        let text = markdown_to_text("hello world");
        assert_eq!(text.lines.len(), 1);
        assert_eq!(text.lines[0].spans[0].content, "hello world");
    }

    #[test]
    fn test_markdown_code() {
        let text = markdown_to_text("`code`");
        let line = text.lines.first().unwrap();
        assert!(line.spans.iter().any(|s| s.content == "code"));
    }

    #[test]
    fn test_markdown_italic() {
        let text = markdown_to_text("*italic*");
        let span = text.lines[0].spans[0].clone();
        assert_eq!(span.content, "italic");
        assert!(span.style.add_modifier.contains(Modifier::ITALIC));
    }

    #[test]
    fn test_markdown_strikethrough() {
        let text = markdown_to_text("~~removed~~");
        let span = text.lines[0].spans[0].clone();
        assert_eq!(span.content, "removed");
        assert!(span.style.add_modifier.contains(Modifier::CROSSED_OUT));
    }

    #[test]
    fn test_markdown_heading() {
        let text = markdown_to_text("# title");
        let span = text.lines[0].spans[0].clone();
        assert_eq!(span.content, "title");
        assert!(span.style.add_modifier.contains(Modifier::BOLD));
    }

    #[test]
    fn test_markdown_link() {
        let text = markdown_to_text("[link](https://example.com)");
        let span = text.lines[0].spans[0].clone();
        assert_eq!(span.content, "link");
        assert!(span.style.add_modifier.contains(Modifier::UNDERLINED));
        assert_eq!(span.style.fg, Some(Color::Blue));
    }

    #[test]
    fn test_markdown_codeblock() {
        let text = markdown_to_text("```rust\nlet x = 1;\n```");
        let content: String = text
            .lines
            .iter()
            .flat_map(|l| l.spans.iter().map(|s| s.content.as_ref()))
            .collect();
        assert!(content.contains("let x = 1;"));
    }

    #[test]
    fn test_markdown_blockquote() {
        let text = markdown_to_text("> quote");
        assert!(
            text.lines
                .iter()
                .any(|l| l.spans.iter().any(|s| s.content.contains("quote")))
        );
    }

    #[test]
    fn test_markdown_list() {
        let text = markdown_to_text("- one\n- two");
        let content: String = text
            .lines
            .iter()
            .flat_map(|l| l.spans.iter().map(|s| s.content.as_ref()))
            .collect();
        assert!(content.contains("one"));
        assert!(content.contains("two"));
    }

    #[test]
    fn test_markdown_horizontal_rule() {
        let text = markdown_to_text("---");
        assert!(text.lines.iter().any(|l| {
            l.spans
                .first()
                .map(|s| s.content.starts_with('─'))
                .unwrap_or(false)
        }));
    }

    #[test]
    fn test_markdown_line_break() {
        let text = markdown_to_text("line1  \nline2");
        assert!(text.lines.len() >= 2);
        let first: String = text.lines[0]
            .spans
            .iter()
            .map(|s| s.content.as_ref())
            .collect();
        assert!(first.contains("line1"));
    }

    #[test]
    fn test_markdown_empty_input_uses_fallback() {
        let text = markdown_to_text("");
        assert_eq!(text.lines.len(), 1);
        assert!(text.lines[0].spans.is_empty());
    }

    #[test]
    fn test_markdown_paragraphs_keep_empty_line() {
        let text = markdown_to_text("p1\n\np2");
        assert!(text.lines.iter().any(|l| l.spans.is_empty()));
    }

    #[test]
    fn test_markdown_think_block_renders_gray() {
        let text = markdown_to_text("before<think>thinking...</think>after");
        let content: String = text
            .lines
            .iter()
            .flat_map(|l| l.spans.iter().map(|s| s.content.as_ref()))
            .collect();
        assert!(content.contains("thinking..."));
        assert!(content.contains("before"));
        assert!(content.contains("after"));
    }
}
