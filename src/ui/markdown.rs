use pulldown_cmark::{Event, Parser, Tag, TagEnd};
use ratatui::{
    style::{Color, Modifier, Style},
    text::{Line, Span, Text},
};

/// 将 Markdown 文本转换为带样式的 ratatui `Text`。
/// 如果解析失败或内容为空，则退回到纯文本显示。
pub fn markdown_to_text(input: &str) -> Text<'_> {
    let mut lines: Vec<Line> = Vec::new();
    let mut current_spans: Vec<Span> = Vec::new();
    let mut style = Style::default();

    for event in Parser::new(input) {
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

    // 去掉末尾由块级元素结束产生的空行，保留段落/列表之间的真实空行。
    while let Some(last) = lines.last() {
        if last.spans.is_empty() || (last.spans.len() == 1 && last.spans[0].content.is_empty()) {
            lines.pop();
        } else {
            break;
        }
    }

    if lines.is_empty() {
        lines.push(Line::from(input.to_string()));
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
}
