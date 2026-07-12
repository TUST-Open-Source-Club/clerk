use anyhow::{Context, Result};
use async_trait::async_trait;
use serde_json::Value;
use std::collections::HashMap;
use std::path::PathBuf;

use crate::tools::schema::{Tool, ToolContext, ToolResult, ToolSchema, get_string};

pub struct MergePdfTool;

#[async_trait]
impl Tool for MergePdfTool {
    fn name(&self) -> &str {
        "pdf_merge"
    }

    fn description(&self) -> &str {
        "将多个 PDF 文件合并为一个。优先使用 pdftk/qpdf/pypdf，不存在时提示安装。"
    }

    fn schema(&self) -> ToolSchema {
        ToolSchema::new("pdf_merge", "合并 PDF")
            .with_array(
                "files",
                crate::tools::schema::ParameterSchema::string("PDF 文件路径"),
                "要合并的 PDF 文件路径列表",
                true,
            )
            .with_string("output", "输出文件路径", true)
    }

    async fn execute(&self, args: HashMap<String, Value>, ctx: &ToolContext) -> Result<ToolResult> {
        let files = args
            .get("files")
            .and_then(|v| v.as_array())
            .context("files 参数必须是数组")?;
        let output = get_string(&args, "output")?;

        let file_paths: Vec<PathBuf> = files
            .iter()
            .map(|f| {
                let s = f.as_str().context("files 必须是字符串数组")?;
                resolve_path(&ctx.working_dir, s)
            })
            .collect::<Result<_>>()?;
        let output_path = resolve_path(&ctx.working_dir, &output)?;

        if let Some(tool) = find_pdf_tool().await? {
            match tool.as_str() {
                "pdftk" => {
                    let mut cmd = tokio::process::Command::new("pdftk");
                    for p in &file_paths {
                        cmd.arg(p);
                    }
                    cmd.arg("cat").arg("output").arg(&output_path);
                    run_command(cmd).await?;
                }
                "qpdf" => {
                    let mut cmd = tokio::process::Command::new("qpdf");
                    cmd.arg("--empty").arg("--pages");
                    for p in &file_paths {
                        cmd.arg(p);
                    }
                    cmd.arg("--").arg(&output_path);
                    run_command(cmd).await?;
                }
                "python" => {
                    let mut cmd = tokio::process::Command::new("python3");
                    cmd.arg("-c")
                        .arg(generate_pypdf_merge_script(&file_paths, &output_path));
                    run_command(cmd).await?;
                }
                _ => unreachable!(),
            }
        } else {
            return Ok(ToolResult::Error(pdf_tool_missing_hint()));
        }

        Ok(ToolResult::Text(format!(
            "已合并 {} 个文件到: {}",
            file_paths.len(),
            output_path.display()
        )))
    }
}

pub struct SplitPdfTool;

#[async_trait]
impl Tool for SplitPdfTool {
    fn name(&self) -> &str {
        "pdf_split"
    }

    fn description(&self) -> &str {
        "按页码范围拆分 PDF 文件。优先使用 pdftk/qpdf/pypdf，不存在时提示安装。"
    }

    fn schema(&self) -> ToolSchema {
        ToolSchema::new("pdf_split", "拆分 PDF")
            .with_string("input", "输入 PDF 文件路径", true)
            .with_string("output", "输出文件路径", true)
            .with_integer("start", "起始页（从 1 开始）", true)
            .with_integer("end", "结束页（包含）", true)
    }

    async fn execute(&self, args: HashMap<String, Value>, ctx: &ToolContext) -> Result<ToolResult> {
        let input = get_string(&args, "input")?;
        let output = get_string(&args, "output")?;
        let start = crate::tools::schema::get_i64(&args, "start", 1);
        let end = crate::tools::schema::get_i64(&args, "end", 1);

        let input_path = resolve_path(&ctx.working_dir, &input)?;
        let output_path = resolve_path(&ctx.working_dir, &output)?;

        if let Some(tool) = find_pdf_tool().await? {
            match tool.as_str() {
                "pdftk" => {
                    let mut cmd = tokio::process::Command::new("pdftk");
                    cmd.arg(&input_path)
                        .arg("cat")
                        .arg(format!("{}-{}", start, end))
                        .arg("output")
                        .arg(&output_path);
                    run_command(cmd).await?;
                }
                "qpdf" => {
                    let mut cmd = tokio::process::Command::new("qpdf");
                    cmd.arg(&input_path)
                        .arg("--pages")
                        .arg(".")
                        .arg(format!("{}-{}", start, end))
                        .arg("--")
                        .arg(&output_path);
                    run_command(cmd).await?;
                }
                "python" => {
                    let script = format!(
                        "from pypdf import PdfReader, PdfWriter; r=PdfReader('{}'); w=PdfWriter(); [w.add_page(r.pages[i-1]) for i in range({}, {})]; w.write(open('{}','wb'))",
                        input_path.display(),
                        start,
                        end + 1,
                        output_path.display()
                    );
                    let mut cmd = tokio::process::Command::new("python3");
                    cmd.arg("-c").arg(script);
                    run_command(cmd).await?;
                }
                _ => unreachable!(),
            }
        } else {
            return Ok(ToolResult::Error(pdf_tool_missing_hint()));
        }

        Ok(ToolResult::Text(format!(
            "已提取第 {}-{} 页到: {}",
            start,
            end,
            output_path.display()
        )))
    }
}

async fn find_pdf_tool() -> Result<Option<String>> {
    for tool in [&"pdftk", &"qpdf"] {
        if command_exists(tool).await {
            return Ok(Some(tool.to_string()));
        }
    }

    // 检查 python3 + pypdf
    if command_exists("python3").await {
        let check = tokio::process::Command::new("python3")
            .args([&"-c", &"import pypdf; print('ok')"])
            .output()
            .await?;
        if check.status.success() {
            return Ok(Some("python".to_string()));
        }
    }

    Ok(None)
}

async fn command_exists(cmd: &str) -> bool {
    tokio::process::Command::new(cmd)
        .arg("--version")
        .output()
        .await
        .map(|o| o.status.success())
        .unwrap_or(false)
}

async fn run_command(mut cmd: tokio::process::Command) -> Result<()> {
    let output = cmd.output().await.context("执行 PDF 工具失败")?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(anyhow::anyhow!("PDF 工具失败: {}", stderr));
    }
    Ok(())
}

fn pdf_tool_missing_hint() -> String {
    "未检测到 PDF 处理工具。请安装以下任一工具：\n\
    - pdftk: sudo apt install pdftk / brew install pdftk-java\n\
    - qpdf: sudo apt install qpdf / brew install qpdf\n\
    - python3 + pypdf: pip install pypdf"
        .to_string()
}

fn resolve_path(working_dir: &std::path::Path, input: &str) -> Result<PathBuf> {
    let path = PathBuf::from(input);
    Ok(if path.is_absolute() {
        path
    } else {
        working_dir.join(path)
    })
}

fn generate_pypdf_merge_script(files: &[PathBuf], output: &std::path::Path) -> String {
    let imports = "from pypdf import PdfWriter; w=PdfWriter()";
    let mut body = String::new();
    for f in files {
        body.push_str(&format!("w.append('{}');", f.display()));
    }
    let save = format!("w.write(open('{}','wb'))", output.display());
    format!("{};{};{}", imports, body, save)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_resolve_output_path() {
        let ctx = ToolContext {
            working_dir: std::path::PathBuf::from("/tmp"),
        };
        let path = resolve_path(&ctx.working_dir, "a.pdf").unwrap();
        assert_eq!(path, std::path::PathBuf::from("/tmp/a.pdf"));
    }
}
