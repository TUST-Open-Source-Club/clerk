use anyhow::{Context, Result};
use async_trait::async_trait;
use calamine::{Reader, Xlsx};
use docx_rs::*;
use rust_xlsxwriter::Workbook;
use serde_json::{Value, json};
use std::collections::HashMap;
use std::fs::File;
use std::io::BufReader;
use std::path::PathBuf;

use crate::tools::schema::{Tool, ToolContext, ToolResult, ToolSchema, get_string};

pub struct ReadExcelTool;

#[async_trait]
impl Tool for ReadExcelTool {
    fn name(&self) -> &str {
        "office_read_excel"
    }

    fn description(&self) -> &str {
        "读取 Excel 文件，返回所有 sheet 的数据（JSON 格式）。"
    }

    fn schema(&self) -> ToolSchema {
        ToolSchema::new("office_read_excel", "读取 Excel")
            .with_string("path", "Excel 文件路径", true)
            .with_string("sheet", "指定 sheet 名称，默认读取第一个", false)
    }

    async fn execute(&self, args: HashMap<String, Value>, ctx: &ToolContext) -> Result<ToolResult> {
        let path_str = get_string(&args, "path")?;
        let sheet_name = get_string(&args, "sheet").ok();
        let path = resolve_path(&ctx.working_dir, &path_str)?;

        let file =
            File::open(&path).with_context(|| format!("打开文件失败: {}", path.display()))?;
        let mut workbook: Xlsx<BufReader<File>> =
            Xlsx::new(BufReader::new(file)).context("解析 Excel 失败")?;

        let sheets = workbook.sheet_names();
        let target_sheet = sheet_name
            .or_else(|| sheets.first().cloned())
            .context("Excel 没有可用 sheet")?;

        let range = workbook
            .worksheet_range(&target_sheet)
            .with_context(|| format!("读取 sheet {} 失败", target_sheet))?;

        let mut rows = Vec::new();
        for row in range.rows() {
            let values: Vec<Value> = row
                .iter()
                .map(|cell| match cell {
                    calamine::Data::String(s) => Value::String(s.clone()),
                    calamine::Data::Float(f) => json!(f),
                    calamine::Data::Int(i) => json!(i),
                    calamine::Data::Bool(b) => json!(b),
                    calamine::Data::DateTime(d) => json!(d.as_f64()),
                    _ => Value::String(cell.to_string()),
                })
                .collect();
            rows.push(Value::Array(values));
        }

        Ok(ToolResult::Json(json!({
            "sheet": target_sheet,
            "rows": rows
        })))
    }
}

pub struct WriteExcelTool;

#[async_trait]
impl Tool for WriteExcelTool {
    fn name(&self) -> &str {
        "office_write_excel"
    }

    fn description(&self) -> &str {
        "将二维 JSON 数组写入 Excel 文件。"
    }

    fn schema(&self) -> ToolSchema {
        ToolSchema::new("office_write_excel", "写入 Excel")
            .with_string("path", "输出 Excel 文件路径", true)
            .with_string("sheet", "sheet 名称，默认 Sheet1", false)
            .with_array(
                "rows",
                crate::tools::schema::ParameterSchema::string("单元格值"),
                "二维数组，每行是一组字符串值",
                true,
            )
    }

    async fn execute(&self, args: HashMap<String, Value>, ctx: &ToolContext) -> Result<ToolResult> {
        let path_str = get_string(&args, "path")?;
        let sheet_name = get_string(&args, "sheet").unwrap_or_else(|_| "Sheet1".to_string());
        let rows = args
            .get("rows")
            .and_then(|v| v.as_array())
            .context("rows 参数必须是数组")?;
        let path = resolve_path(&ctx.working_dir, &path_str)?;

        let mut workbook = Workbook::new();
        let worksheet = workbook.add_worksheet();
        worksheet
            .set_name(&sheet_name)
            .context("设置 sheet 名称失败")?;

        for (row_idx, row) in rows.iter().enumerate() {
            let cells = row.as_array().context("每行必须是数组")?;
            for (col_idx, cell) in cells.iter().enumerate() {
                let fallback = cell.to_string();
                let value = cell.as_str().unwrap_or(&fallback);
                worksheet
                    .write_string(row_idx as u32, col_idx as u16, value)
                    .context("写入单元格失败")?;
            }
        }

        workbook
            .save(&path)
            .with_context(|| format!("保存 Excel 失败: {}", path.display()))?;
        Ok(ToolResult::Text(format!("已保存: {}", path.display())))
    }
}

pub struct ReadWordTool;

#[async_trait]
impl Tool for ReadWordTool {
    fn name(&self) -> &str {
        "office_read_word"
    }

    fn description(&self) -> &str {
        "读取 Word 文档的纯文本内容。"
    }

    fn schema(&self) -> ToolSchema {
        ToolSchema::new("office_read_word", "读取 Word").with_string("path", "Word 文件路径", true)
    }

    async fn execute(&self, args: HashMap<String, Value>, ctx: &ToolContext) -> Result<ToolResult> {
        let path_str = get_string(&args, "path")?;
        let path = resolve_path(&ctx.working_dir, &path_str)?;

        let docx = read_docx(
            &std::fs::read(&path).with_context(|| format!("读取文件失败: {}", path.display()))?,
        )
        .context("解析 Word 文档失败")?;

        let mut text = String::new();
        for paragraph in docx.document.children {
            if let docx_rs::DocumentChild::Paragraph(p) = paragraph {
                for child in &p.children {
                    if let docx_rs::ParagraphChild::Run(r) = child {
                        for run_child in &r.children {
                            if let docx_rs::RunChild::Text(t) = run_child {
                                text.push_str(&t.text);
                            }
                        }
                    }
                }
                text.push('\n');
            }
        }

        Ok(ToolResult::Text(text))
    }
}

pub struct WriteWordTool;

#[async_trait]
impl Tool for WriteWordTool {
    fn name(&self) -> &str {
        "office_write_word"
    }

    fn description(&self) -> &str {
        "将文本内容写入 Word 文档。"
    }

    fn schema(&self) -> ToolSchema {
        ToolSchema::new("office_write_word", "写入 Word")
            .with_string("path", "输出 Word 文件路径", true)
            .with_string("content", "文档内容", true)
    }

    async fn execute(&self, args: HashMap<String, Value>, ctx: &ToolContext) -> Result<ToolResult> {
        let path_str = get_string(&args, "path")?;
        let content = get_string(&args, "content")?;
        let path = resolve_path(&ctx.working_dir, &path_str)?;

        let mut docx = Docx::new();
        for line in content.lines() {
            docx = docx.add_paragraph(Paragraph::new().add_run(Run::new().add_text(line)));
        }

        let file =
            File::create(&path).with_context(|| format!("创建文件失败: {}", path.display()))?;
        docx.build().pack(file).context("生成 Word 文档失败")?;

        Ok(ToolResult::Text(format!("已保存: {}", path.display())))
    }
}

fn resolve_path(working_dir: &std::path::Path, input: &str) -> Result<PathBuf> {
    let path = PathBuf::from(input);
    Ok(if path.is_absolute() {
        path
    } else {
        working_dir.join(path)
    })
}

pub struct RenderOfficeTool;

#[async_trait]
impl Tool for RenderOfficeTool {
    fn name(&self) -> &str {
        "office_render"
    }

    fn description(&self) -> &str {
        "使用 Pandoc 将 Markdown/HTML 渲染为 Word/PDF/PPT。支持公式、图片和 reference-docx 模板。"
    }

    fn schema(&self) -> ToolSchema {
        ToolSchema::new("office_render", "Pandoc 文档渲染")
            .with_string("input", "输入文件路径（.md 或 .html）", true)
            .with_string("output", "输出文件路径（.docx/.pdf/.pptx）", true)
            .with_string("template", "Pandoc reference-docx 模板路径", false)
            .with_string("from", "输入格式: markdown|html，默认自动识别", false)
    }

    async fn execute(&self, args: HashMap<String, Value>, ctx: &ToolContext) -> Result<ToolResult> {
        let input = get_string(&args, "input")?;
        let output = get_string(&args, "output")?;
        let template = get_string(&args, "template").ok();
        let from = get_string(&args, "from").unwrap_or_else(|_| "markdown".to_string());

        // 探测 pandoc 是否存在
        let check = tokio::process::Command::new("pandoc")
            .arg("--version")
            .output()
            .await;
        if check.is_err() || !check.unwrap().status.success() {
            return Ok(ToolResult::Error(
                "未检测到 Pandoc。请安装 Pandoc 以使用 office_render 工具：\n\
                - Ubuntu/Debian: sudo apt install pandoc\n\
                - macOS: brew install pandoc\n\
                - Windows: winget install JohnMacFarlane.Pandoc\n\
                或访问 https://pandoc.org/installing.html"
                    .to_string(),
            ));
        }

        let input_path = resolve_path(&ctx.working_dir, &input)?;
        let output_path = resolve_path(&ctx.working_dir, &output)?;

        let mut cmd = tokio::process::Command::new("pandoc");
        cmd.arg(&input_path)
            .arg("-f")
            .arg(&from)
            .arg("-o")
            .arg(&output_path);

        if let Some(tpl) = template {
            let tpl_path = resolve_path(&ctx.working_dir, &tpl)?;
            cmd.arg("--reference-doc").arg(&tpl_path);
        }

        let result = cmd.output().await.context("执行 Pandoc 失败")?;
        if !result.status.success() {
            let stderr = String::from_utf8_lossy(&result.stderr);
            return Ok(ToolResult::Error(format!("Pandoc 失败: {}", stderr)));
        }

        Ok(ToolResult::Text(format!(
            "已渲染: {}",
            output_path.display()
        )))
    }
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

    #[tokio::test]
    async fn test_write_and_read_excel() {
        let dir = TempDir::new().unwrap();
        let write_tool = WriteExcelTool;
        let mut args = HashMap::new();
        args.insert("path".to_string(), Value::String("test.xlsx".to_string()));
        args.insert(
            "rows".to_string(),
            json!([["Name", "Age"], ["Alice", "30"], ["Bob", "25"]]),
        );
        write_tool.execute(args, &ctx(&dir)).await.unwrap();

        let read_tool = ReadExcelTool;
        let mut args = HashMap::new();
        args.insert("path".to_string(), Value::String("test.xlsx".to_string()));
        let result = read_tool.execute(args, &ctx(&dir)).await.unwrap();
        let text = result.to_string_for_model();
        assert!(text.contains("Alice"));
        assert!(text.contains("Bob"));
    }

    #[tokio::test]
    async fn test_read_excel_with_sheet() {
        let dir = TempDir::new().unwrap();
        let write_tool = WriteExcelTool;
        let mut args = HashMap::new();
        args.insert("path".to_string(), Value::String("test.xlsx".to_string()));
        args.insert("sheet".to_string(), Value::String("Data".to_string()));
        args.insert("rows".to_string(), json!([["a"]]));
        write_tool.execute(args, &ctx(&dir)).await.unwrap();

        let read_tool = ReadExcelTool;
        let mut args = HashMap::new();
        args.insert("path".to_string(), Value::String("test.xlsx".to_string()));
        args.insert("sheet".to_string(), Value::String("Data".to_string()));
        let result = read_tool.execute(args, &ctx(&dir)).await.unwrap();
        assert!(result.to_string_for_model().contains("a"));
    }

    #[tokio::test]
    async fn test_read_excel_missing_file() {
        let dir = TempDir::new().unwrap();
        let tool = ReadExcelTool;
        let mut args = HashMap::new();
        args.insert(
            "path".to_string(),
            Value::String("missing.xlsx".to_string()),
        );
        assert!(tool.execute(args, &ctx(&dir)).await.is_err());
    }

    #[tokio::test]
    async fn test_write_excel_invalid_rows() {
        let dir = TempDir::new().unwrap();
        let tool = WriteExcelTool;
        let mut args = HashMap::new();
        args.insert("path".to_string(), Value::String("bad.xlsx".to_string()));
        args.insert("rows".to_string(), Value::String("not array".to_string()));
        assert!(tool.execute(args, &ctx(&dir)).await.is_err());
    }

    #[tokio::test]
    async fn test_read_word_missing_file() {
        let dir = TempDir::new().unwrap();
        let tool = ReadWordTool;
        let mut args = HashMap::new();
        args.insert(
            "path".to_string(),
            Value::String("missing.docx".to_string()),
        );
        assert!(tool.execute(args, &ctx(&dir)).await.is_err());
    }

    #[tokio::test]
    async fn test_render_office_without_pandoc() {
        let dir = TempDir::new().unwrap();
        let tool = RenderOfficeTool;
        let mut args = HashMap::new();
        args.insert("input".to_string(), Value::String("a.md".to_string()));
        args.insert("output".to_string(), Value::String("a.docx".to_string()));
        let result = tool.execute(args, &ctx(&dir)).await.unwrap();
        let text = result.to_string_for_model();
        assert!(text.contains("Pandoc") || text.contains("已渲染"));
    }

    #[test]
    fn test_resolve_path() {
        let wd = std::path::PathBuf::from("/tmp");
        assert_eq!(
            resolve_path(&wd, "a.xlsx").unwrap(),
            std::path::PathBuf::from("/tmp/a.xlsx")
        );
        assert_eq!(
            resolve_path(&wd, "/abs/a.xlsx").unwrap(),
            std::path::PathBuf::from("/abs/a.xlsx")
        );
    }
}
