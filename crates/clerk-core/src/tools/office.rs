use anyhow::{Context, Result};
use async_trait::async_trait;
use calamine::{Reader, Xlsx};
use docx_rs::*;
use rust_xlsxwriter::{Chart, Format, Workbook};
use serde::Deserialize;
use serde_json::{Value, json};
use std::collections::HashMap;
use std::fs::File;
use std::io::{BufReader, Cursor, Read, Write};
use std::path::PathBuf;

use crate::tools::schema::{Tool, ToolContext, ToolResult, ToolSchema, get_string};

/// `office_read_excel` 工具：读取 Excel 指定 sheet，返回 JSON 行数据。
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

/// `office_write_excel` 工具：将二维 JSON 数组写入 Excel 文件，支持公式、样式、图表与图片。
pub struct WriteExcelTool;

/// 单元格扩展写法：字符串/数字/布尔直接写入；
/// 对象形式 {"value":..,"formula":"=SUM(A1:A2)","bold":true,"italic":true,"color":"FF0000","bg":"FFFF00","size":12}
/// 支持公式与样式。
#[derive(Debug, Deserialize)]
struct ExcelCell {
    value: Option<Value>,
    formula: Option<String>,
    bold: Option<bool>,
    italic: Option<bool>,
    color: Option<String>,
    bg: Option<String>,
    size: Option<f64>,
}

#[derive(Debug, Deserialize)]
struct ExcelChart {
    #[serde(rename = "type", default = "default_chart_type")]
    chart_type: String,
    title: Option<String>,
    /// 分类区域，如 "A2:A4" 或完整 "=Sheet1!$A$2:$A$4"
    categories: String,
    /// 数值区域，如 "B2:B4"
    values: String,
    name: Option<String>,
    #[serde(default)]
    row: u32,
    #[serde(default)]
    col: u16,
}

fn default_chart_type() -> String {
    "column".to_string()
}

#[derive(Debug, Deserialize)]
struct ExcelImage {
    path: String,
    #[serde(default)]
    row: u32,
    #[serde(default)]
    col: u16,
}

#[async_trait]
impl Tool for WriteExcelTool {
    fn name(&self) -> &str {
        "office_write_excel"
    }

    fn description(&self) -> &str {
        "将二维 JSON 数组写入 Excel 文件。单元格可以是字符串/数字/布尔，也可以是对象 \
        {\"value\":值,\"formula\":\"=SUM(A1:A2)\",\"bold\":true,\"italic\":true,\"color\":\"FF0000\",\"bg\":\"FFFF00\",\"size\":12} \
        以支持公式与样式。可通过 charts 添加图表、images 插入图片。"
    }

    fn schema(&self) -> ToolSchema {
        ToolSchema::new("office_write_excel", "写入 Excel")
            .with_string("path", "输出 Excel 文件路径", true)
            .with_string("sheet", "sheet 名称，默认 Sheet1", false)
            .with_array(
                "rows",
                crate::tools::schema::ParameterSchema::object("单元格：字符串/数字/布尔或样式对象"),
                "二维数组，每行是一组单元格",
                true,
            )
            .with_array(
                "charts",
                crate::tools::schema::ParameterSchema::object(
                    "{\"type\":\"column|bar|line|pie|area\",\"title\":..,\"categories\":\"A2:A4\",\"values\":\"B2:B4\",\"name\":..,\"row\":0,\"col\":0}",
                ),
                "图表数组（可选）",
                false,
            )
            .with_array(
                "images",
                crate::tools::schema::ParameterSchema::object(
                    "{\"path\":\"图片路径\",\"row\":0,\"col\":0}",
                ),
                "插入的图片数组（可选）",
                false,
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
                write_excel_cell(worksheet, row_idx as u32, col_idx as u16, cell)?;
            }
        }

        // 图表
        if let Some(charts) = args.get("charts").and_then(|v| v.as_array()) {
            for chart_value in charts {
                let spec: ExcelChart =
                    serde_json::from_value(chart_value.clone()).context("charts 元素解析失败")?;
                let mut chart = match spec.chart_type.as_str() {
                    "bar" => Chart::new_bar(),
                    "line" => Chart::new_line(),
                    "pie" => Chart::new_pie(),
                    "area" => Chart::new_area(),
                    _ => Chart::new_column(),
                };
                if let Some(title) = &spec.title {
                    chart.title().set_name(title);
                }
                let series = chart
                    .add_series()
                    .set_categories(normalize_range(&sheet_name, &spec.categories).as_str())
                    .set_values(normalize_range(&sheet_name, &spec.values).as_str());
                if let Some(name) = &spec.name {
                    series.set_name(name);
                }
                worksheet
                    .insert_chart(spec.row, spec.col, &chart)
                    .context("插入图表失败")?;
            }
        }

        // 图片
        if let Some(images) = args.get("images").and_then(|v| v.as_array()) {
            for image_value in images {
                let spec: ExcelImage =
                    serde_json::from_value(image_value.clone()).context("images 元素解析失败")?;
                let img_path = resolve_path(&ctx.working_dir, &spec.path)?;
                let image = rust_xlsxwriter::Image::new(&img_path)
                    .with_context(|| format!("读取图片失败: {}", img_path.display()))?;
                worksheet
                    .insert_image(spec.row, spec.col, &image)
                    .context("插入图片失败")?;
            }
        }

        workbook
            .save(&path)
            .with_context(|| format!("保存 Excel 失败: {}", path.display()))?;
        Ok(ToolResult::Text(format!("已保存: {}", path.display())))
    }
}

/// 写入单个单元格：支持纯值或带公式/样式的对象。
fn write_excel_cell(
    worksheet: &mut rust_xlsxwriter::Worksheet,
    row: u32,
    col: u16,
    cell: &Value,
) -> Result<()> {
    if let Some(obj) = cell.as_object() {
        let spec: ExcelCell =
            serde_json::from_value(Value::Object(obj.clone())).context("单元格对象解析失败")?;
        let mut format = Format::new();
        if spec.bold.unwrap_or(false) {
            format = format.set_bold();
        }
        if spec.italic.unwrap_or(false) {
            format = format.set_italic();
        }
        if let Some(color) = &spec.color {
            format = format.set_font_color(parse_hex_color(color)?);
        }
        if let Some(bg) = &spec.bg {
            format = format.set_background_color(parse_hex_color(bg)?);
        }
        if let Some(size) = spec.size {
            format = format.set_font_size(size);
        }
        if let Some(formula) = &spec.formula {
            worksheet
                .write_formula_with_format(row, col, formula.as_str(), &format)
                .context("写入公式失败")?;
        } else if let Some(value) = &spec.value {
            write_excel_value(worksheet, row, col, value, Some(&format))?;
        } else {
            worksheet
                .write_string_with_format(row, col, "", &format)
                .context("写入空白单元格失败")?;
        }
        return Ok(());
    }
    write_excel_value(worksheet, row, col, cell, None)
}

/// 按 JSON 值类型写入单元格。
fn write_excel_value(
    worksheet: &mut rust_xlsxwriter::Worksheet,
    row: u32,
    col: u16,
    value: &Value,
    format: Option<&Format>,
) -> Result<()> {
    match (value, format) {
        (Value::Number(n), Some(f)) => {
            worksheet.write_number_with_format(row, col, n.as_f64().unwrap_or(0.0), f)
        }
        (Value::Number(n), None) => worksheet.write_number(row, col, n.as_f64().unwrap_or(0.0)),
        (Value::Bool(b), Some(f)) => worksheet.write_boolean_with_format(row, col, *b, f),
        (Value::Bool(b), None) => worksheet.write_boolean(row, col, *b),
        (other, Some(f)) => {
            let fallback = other.to_string();
            let s = other.as_str().unwrap_or(&fallback);
            worksheet.write_string_with_format(row, col, s, f)
        }
        (other, None) => {
            let fallback = other.to_string();
            let s = other.as_str().unwrap_or(&fallback);
            worksheet.write_string(row, col, s)
        }
    }
    .context("写入单元格失败")?;
    Ok(())
}

/// 解析 "FF0000" 或 "#FF0000" 形式的颜色。
fn parse_hex_color(color: &str) -> Result<rust_xlsxwriter::Color> {
    let hex = color.trim_start_matches('#');
    let rgb = u32::from_str_radix(hex, 16).with_context(|| format!("无效颜色: {color}"))?;
    Ok(rust_xlsxwriter::Color::RGB(rgb))
}

/// 将 "A2:A4" 形式的区域规范化为 "='Sheet'!$A$2:$A$4"；已带 "=" 的完整引用原样返回。
fn normalize_range(sheet: &str, range: &str) -> String {
    if range.starts_with('=') {
        return range.to_string();
    }
    let absolutize = |endpoint: &str| {
        let split = endpoint
            .find(|c: char| c.is_ascii_digit())
            .unwrap_or(endpoint.len());
        let (col, row) = endpoint.split_at(split);
        format!("${}${}", col, row)
    };
    match range.split_once(':') {
        Some((from, to)) => format!(
            "='{}'!{}:{}",
            sheet.replace('\'', "''"),
            absolutize(from),
            absolutize(to)
        ),
        None => format!("='{}'!{}", sheet.replace('\'', "''"), absolutize(range)),
    }
}

/// `office_read_word` 工具：提取 Word 文档的纯文本内容。
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

/// `office_write_word` 工具：简单模式按行写入段落；spec 模式支持标题、富文本段落、
/// 表格、图片、公式（OMML）、分页符、页面设置与背景等复杂版式。
pub struct WriteWordTool;

/// 结构化 Word 文档规格。
#[derive(Debug, Deserialize)]
struct WordSpec {
    title: Option<String>,
    author: Option<String>,
    background: Option<WordBackground>,
    pagesetup: Option<WordPageSetup>,
    #[serde(default)]
    elements: Vec<WordElement>,
}

#[derive(Debug, Deserialize)]
#[serde(untagged)]
enum WordBackground {
    /// 纯色背景，如 {"color": "F5F5DC"}
    Color { color: String },
    /// 图片背景，如 {"image": "bg.png"}
    Image { image: String },
}

#[derive(Debug, Deserialize)]
struct WordPageSetup {
    /// 纸张：A4（默认）/ LETTER / A5 / LEGAL
    size: Option<String>,
    /// 方向：portrait（默认）/ landscape
    orientation: Option<String>,
    /// 页边距（毫米）
    margins: Option<WordMargins>,
}

#[derive(Debug, Deserialize)]
struct WordMargins {
    top: Option<f64>,
    right: Option<f64>,
    bottom: Option<f64>,
    left: Option<f64>,
}

#[derive(Debug, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum WordElement {
    /// 标题：{"type":"heading","level":1,"text":"..."}
    Heading { level: Option<u8>, text: String },
    /// 段落：{"type":"paragraph","text":"...","bold":true,"italic":true,"underline":true,
    /// "color":"FF0000","size":12,"align":"center"}
    Paragraph {
        text: String,
        bold: Option<bool>,
        italic: Option<bool>,
        underline: Option<bool>,
        color: Option<String>,
        /// 字号（磅）
        size: Option<f64>,
        /// 对齐：left / center / right
        align: Option<String>,
    },
    /// 表格：{"type":"table","headers":["A","B"],"rows":[["1","2"]]}
    Table {
        headers: Option<Vec<String>>,
        #[serde(default)]
        rows: Vec<Vec<String>>,
    },
    /// 图片：{"type":"image","path":"a.png","width":4.0}（width 单位：英寸，可选）
    Image { path: String, width: Option<f64> },
    /// 公式（OMML 线性文本）：{"type":"formula","text":"E=mc^2"}
    Formula { text: String },
    /// 分页符：{"type":"page_break"}
    PageBreak,
}

#[async_trait]
impl Tool for WriteWordTool {
    fn name(&self) -> &str {
        "office_write_word"
    }

    fn description(&self) -> &str {
        "将文本内容写入 Word 文档。提供 spec 参数（JSON 对象）时可创建复杂版式：\
        title/author 元数据、background（{\"color\":\"RRGGBB\"} 或 {\"image\":\"路径\"}）、\
        pagesetup（{\"size\":\"A4\",\"orientation\":\"landscape\",\"margins\":{\"top\":25.4,...}} 毫米）、\
        elements 数组（heading/paragraph/table/image/formula/page_break）。"
    }

    fn schema(&self) -> ToolSchema {
        ToolSchema::new("office_write_word", "写入 Word")
            .with_string("path", "输出 Word 文件路径", true)
            .with_string("content", "文档内容（简单模式，与 spec 二选一）", false)
            .with_object(
                "spec",
                "结构化文档规格（与 content 二选一）：{\"title\":..,\"author\":..,\"background\":..,\"pagesetup\":..,\"elements\":[...]}。\
                elements 元素：{\"type\":\"heading\",\"level\":1,\"text\":..}；\
                {\"type\":\"paragraph\",\"text\":..,\"bold\":..,\"italic\":..,\"underline\":..,\"color\":\"FF0000\",\"size\":12,\"align\":\"center\"}；\
                {\"type\":\"table\",\"headers\":[..],\"rows\":[[..]]}；\
                {\"type\":\"image\",\"path\":..,\"width\":英寸}；\
                {\"type\":\"formula\",\"text\":..}；{\"type\":\"page_break\"}",
                false,
            )
    }

    async fn execute(&self, args: HashMap<String, Value>, ctx: &ToolContext) -> Result<ToolResult> {
        let path_str = get_string(&args, "path")?;
        let path = resolve_path(&ctx.working_dir, &path_str)?;

        if let Some(spec_value) = args.get("spec") {
            let spec: WordSpec =
                serde_json::from_value(spec_value.clone()).context("spec 参数解析失败")?;
            build_word_from_spec(&spec, &path, &ctx.working_dir)?;
        } else {
            let content =
                get_string(&args, "content").context("缺少参数：content 与 spec 必须提供其一")?;
            let mut docx = Docx::new();
            for line in content.lines() {
                docx = docx.add_paragraph(Paragraph::new().add_run(Run::new().add_text(line)));
            }
            let file =
                File::create(&path).with_context(|| format!("创建文件失败: {}", path.display()))?;
            docx.build().pack(file).context("生成 Word 文档失败")?;
        }

        Ok(ToolResult::Text(format!("已保存: {}", path.display())))
    }
}

/// 根据结构化规格构建 Word 文档。
fn build_word_from_spec(
    spec: &WordSpec,
    path: &std::path::Path,
    working_dir: &std::path::Path,
) -> Result<()> {
    // 页面设置
    let (page_w, page_h) = page_size_twips(spec.pagesetup.as_ref());
    let mut docx = Docx::new().page_size(page_w, page_h).page_orient(
        match spec
            .pagesetup
            .as_ref()
            .and_then(|p| p.orientation.as_deref())
        {
            Some("landscape") => PageOrientationType::Landscape,
            _ => PageOrientationType::Portrait,
        },
    );
    if let Some(margins) = spec.pagesetup.as_ref().and_then(|p| p.margins.as_ref()) {
        // docx-rs 默认页边距（twips）
        let default = PageMargin {
            top: 1985,
            right: 1701,
            bottom: 1701,
            left: 1701,
            header: 851,
            footer: 992,
            gutter: 0,
        };
        docx = docx.page_margin(PageMargin {
            top: margins.top.map(mm_to_twips).unwrap_or(default.top),
            right: margins.right.map(mm_to_twips).unwrap_or(default.right),
            bottom: margins.bottom.map(mm_to_twips).unwrap_or(default.bottom),
            left: margins.left.map(mm_to_twips).unwrap_or(default.left),
            ..default
        });
    }

    // 标题样式（Heading 1-3）
    for (level, size_pt) in [(1u8, 16.0f64), (2, 13.0), (3, 12.0)] {
        docx = docx.add_style(
            Style::new(format!("Heading{level}"), StyleType::Paragraph)
                .name(format!("heading {level}"))
                .based_on("Normal")
                .next("Normal")
                .bold()
                .color("2F5496")
                .size((size_pt * 2.0) as usize)
                .q_format(true),
        );
    }

    // 公式占位符：构建后处理阶段替换为 OMML
    let mut formulas: Vec<String> = Vec::new();

    for element in &spec.elements {
        match element {
            WordElement::Heading { level, text } => {
                let level = level.unwrap_or(1).clamp(1, 3);
                docx = docx.add_paragraph(
                    Paragraph::new()
                        .style(&format!("Heading{level}"))
                        .add_run(Run::new().add_text(text)),
                );
            }
            WordElement::Paragraph {
                text,
                bold,
                italic,
                underline,
                color,
                size,
                align,
            } => {
                let mut run = Run::new().add_text(text);
                if bold.unwrap_or(false) {
                    run = run.bold();
                }
                if italic.unwrap_or(false) {
                    run = run.italic();
                }
                if underline.unwrap_or(false) {
                    run = run.underline("single");
                }
                if let Some(color) = color {
                    run = run.color(color.trim_start_matches('#'));
                }
                if let Some(size) = size {
                    run = run.size((size * 2.0) as usize);
                }
                let mut paragraph = Paragraph::new().add_run(run);
                if let Some(align) = align {
                    let alignment = match align.as_str() {
                        "center" => AlignmentType::Center,
                        "right" => AlignmentType::Right,
                        _ => AlignmentType::Left,
                    };
                    paragraph = paragraph.align(alignment);
                }
                docx = docx.add_paragraph(paragraph);
            }
            WordElement::Table { headers, rows } => {
                let mut table_rows: Vec<TableRow> = Vec::new();
                if let Some(headers) = headers {
                    let cells = headers
                        .iter()
                        .map(|h| {
                            TableCell::new()
                                .shading(Shading::new().fill("D9D9D9"))
                                .add_paragraph(
                                    Paragraph::new().add_run(Run::new().add_text(h).bold()),
                                )
                        })
                        .collect();
                    table_rows.push(TableRow::new(cells));
                }
                for row in rows {
                    let cells = row
                        .iter()
                        .map(|c| {
                            TableCell::new()
                                .add_paragraph(Paragraph::new().add_run(Run::new().add_text(c)))
                        })
                        .collect();
                    table_rows.push(TableRow::new(cells));
                }
                docx = docx.add_table(Table::new(table_rows));
            }
            WordElement::Image { path, width } => {
                let img_path = resolve_path(working_dir, path)?;
                let buf = std::fs::read(&img_path)
                    .with_context(|| format!("读取图片失败: {}", img_path.display()))?;
                let (px_w, px_h) = image::image_dimensions(&img_path)
                    .with_context(|| format!("解析图片尺寸失败: {}", img_path.display()))?;
                // 96 DPI：1px = 9525 EMU；1 英寸 = 914400 EMU
                let (mut w_emu, mut h_emu) = (px_w * 9525, px_h * 9525);
                if let Some(width_in) = width {
                    w_emu = (width_in * 914400.0) as u32;
                    h_emu = (w_emu as u64 * px_h as u64 / px_w.max(1) as u64) as u32;
                }
                let pic = Pic::new(&buf).size(w_emu, h_emu);
                docx = docx.add_paragraph(Paragraph::new().add_run(Run::new().add_image(pic)));
            }
            WordElement::Formula { text } => {
                let marker = format!("CLERK_FORMULA_PLACEHOLDER_{}", formulas.len());
                formulas.push(text.clone());
                docx = docx.add_paragraph(
                    Paragraph::new()
                        .align(AlignmentType::Center)
                        .add_run(Run::new().add_text(marker)),
                );
            }
            WordElement::PageBreak => {
                docx = docx
                    .add_paragraph(Paragraph::new().add_run(Run::new().add_break(BreakType::Page)));
            }
        }
    }

    let mut buf = Cursor::new(Vec::new());
    docx.build().pack(&mut buf).context("生成 Word 文档失败")?;

    // 需要后处理（背景/元数据/公式）时改写 zip 内的 OpenXML
    let bytes = if spec.background.is_some()
        || spec.title.is_some()
        || spec.author.is_some()
        || !formulas.is_empty()
    {
        postprocess_docx(
            &buf.into_inner(),
            spec,
            &formulas,
            page_w,
            page_h,
            working_dir,
        )?
    } else {
        buf.into_inner()
    };

    std::fs::write(path, bytes).with_context(|| format!("写入文件失败: {}", path.display()))?;
    Ok(())
}

/// 纸张尺寸（twips，1/20 磅）。
fn page_size_twips(setup: Option<&WordPageSetup>) -> (u32, u32) {
    let (w, h) = match setup.and_then(|p| p.size.as_deref()).unwrap_or("A4") {
        "LETTER" | "Letter" => (12240, 15840),
        "A5" => (8390, 11906),
        "LEGAL" | "Legal" => (12240, 20160),
        _ => (11906, 16838), // A4
    };
    match setup.and_then(|p| p.orientation.as_deref()) {
        Some("landscape") => (h, w),
        _ => (w, h),
    }
}

/// 毫米转 twips（1mm ≈ 56.6929 twips）。
fn mm_to_twips(mm: f64) -> i32 {
    (mm * 56.6929).round() as i32
}

/// twips 转毫米（用于背景图片尺寸）。
fn twips_to_mm(twips: u32) -> f64 {
    twips as f64 / 56.6929
}

/// 后处理 docx zip：注入背景、文档元数据与 OMML 公式。
fn postprocess_docx(
    bytes: &[u8],
    spec: &WordSpec,
    formulas: &[String],
    page_w: u32,
    page_h: u32,
    working_dir: &std::path::Path,
) -> Result<Vec<u8>> {
    let mut archive = zip::ZipArchive::new(Cursor::new(bytes)).context("读取 docx 包失败")?;
    let mut entries: Vec<(String, Vec<u8>)> = Vec::new();
    for i in 0..archive.len() {
        let mut file = archive.by_index(i).context("读取 docx 条目失败")?;
        let mut data = Vec::new();
        file.read_to_end(&mut data)
            .context("读取 docx 条目内容失败")?;
        entries.push((file.name().to_string(), data));
    }

    // 背景图片：加入包并建立关系
    let mut extra_entries: Vec<(String, Vec<u8>)> = Vec::new();
    let mut bg_image_rid: Option<String> = None;
    if let Some(WordBackground::Image { image }) = &spec.background {
        let img_path = resolve_path(working_dir, image)?;
        let data = std::fs::read(&img_path)
            .with_context(|| format!("读取背景图片失败: {}", img_path.display()))?;
        let ext = img_path
            .extension()
            .and_then(|e| e.to_str())
            .unwrap_or("png")
            .to_ascii_lowercase();
        let ext = if ext == "jpeg" {
            "jpg".to_string()
        } else {
            ext
        };
        extra_entries.push((format!("word/media/clerk-bg.{ext}"), data));
        bg_image_rid = Some("rIdClerkBg".to_string());

        for (name, data) in entries.iter_mut() {
            match name.as_str() {
                "word/_rels/document.xml.rels" => {
                    let mut xml = String::from_utf8_lossy(data).into_owned();
                    let rel = format!(
                        "<Relationship Id=\"rIdClerkBg\" Type=\"http://schemas.openxmlformats.org/officeDocument/2006/relationships/image\" Target=\"media/clerk-bg.{ext}\"/>"
                    );
                    xml = xml.replace("</Relationships>", &format!("{rel}</Relationships>"));
                    *data = xml.into_bytes();
                }
                "[Content_Types].xml" => {
                    let mut xml = String::from_utf8_lossy(data).into_owned();
                    let marker = format!("Extension=\"{ext}\"");
                    if !xml.contains(&marker) {
                        let mime = match ext.as_str() {
                            "jpg" => "image/jpeg",
                            "gif" => "image/gif",
                            "bmp" => "image/bmp",
                            _ => "image/png",
                        };
                        let default =
                            format!("<Default Extension=\"{ext}\" ContentType=\"{mime}\"/>");
                        xml = xml.replace("</Types>", &format!("{default}</Types>"));
                    }
                    *data = xml.into_bytes();
                }
                _ => {}
            }
        }
    }

    for (name, data) in entries.iter_mut() {
        match name.as_str() {
            "word/document.xml" => {
                let mut xml = String::from_utf8_lossy(data).into_owned();
                // 公式需要 OMML 命名空间
                if !formulas.is_empty() && !xml.contains("xmlns:m=") {
                    xml = xml.replacen(
                        "<w:document ",
                        "<w:document xmlns:m=\"http://schemas.openxmlformats.org/officeDocument/2006/math\" ",
                        1,
                    );
                }
                // 背景：作为 w:document 的第一个子元素
                if let Some(background) = &spec.background {
                    let bg_xml = match background {
                        WordBackground::Color { color } => format!(
                            "<w:background w:color=\"{}\"/>",
                            color.trim_start_matches('#')
                        ),
                        WordBackground::Image { .. } => {
                            let rid = bg_image_rid.clone().unwrap_or_default();
                            format!(
                                "<w:background><v:background id=\"clerkBg\"><v:shape id=\"clerkBgShape\" type=\"#_x0000_t75\" style=\"position:absolute;margin-left:0;margin-top:0;width:{:.1}mm;height:{:.1}mm;z-index:-251654144;mso-position-horizontal:center;mso-position-horizontal-relative:margin;mso-position-vertical:center;mso-position-vertical-relative:margin\"><v:imagedata r:id=\"{}\" o:title=\"\"/></v:shape></v:background></w:background>",
                                twips_to_mm(page_w),
                                twips_to_mm(page_h),
                                rid
                            )
                        }
                    };
                    if let Some(pos) = xml.find("<w:body>") {
                        xml.insert_str(pos, &bg_xml);
                    }
                }
                // 公式：占位符段落替换为 OMML
                for (i, formula) in formulas.iter().enumerate() {
                    let marker = format!("CLERK_FORMULA_PLACEHOLDER_{i}");
                    xml = replace_marker_paragraph_with_omml(&xml, &marker, formula)
                        .with_context(|| format!("替换公式占位符失败: {marker}"))?;
                }
                *data = xml.into_bytes();
            }
            "word/settings.xml" => {
                if spec.background.is_some() {
                    let mut xml = String::from_utf8_lossy(data).into_owned();
                    if !xml.contains("displayBackgroundShape") {
                        if let Some(pos) = xml.find("<w:zoom") {
                            xml.insert_str(pos, "<w:displayBackgroundShape/>");
                        } else if let Some(pos) = xml.find('>') {
                            // 回退：插到根元素起始标签之后
                            xml.insert_str(pos + 1, "<w:displayBackgroundShape/>");
                        }
                    }
                    *data = xml.into_bytes();
                }
            }
            "docProps/core.xml" if spec.title.is_some() || spec.author.is_some() => {
                let mut xml = String::from_utf8_lossy(data).into_owned();
                if let Some(author) = &spec.author {
                    xml = xml.replace(
                        "<dc:creator>unknown</dc:creator>",
                        &format!("<dc:creator>{}</dc:creator>", escape_xml(author)),
                    );
                    xml = xml.replace(
                        "<cp:lastModifiedBy>unknown</cp:lastModifiedBy>",
                        &format!(
                            "<cp:lastModifiedBy>{}</cp:lastModifiedBy>",
                            escape_xml(author)
                        ),
                    );
                }
                if let Some(title) = &spec.title {
                    xml = xml.replace(
                        "</cp:coreProperties>",
                        &format!(
                            "<dc:title>{}</dc:title></cp:coreProperties>",
                            escape_xml(title)
                        ),
                    );
                }
                *data = xml.into_bytes();
            }
            _ => {}
        }
    }

    entries.extend(extra_entries);

    let mut out = Cursor::new(Vec::new());
    {
        let mut zip = zip::ZipWriter::new(&mut out);
        let options =
            zip::write::FileOptions::default().compression_method(zip::CompressionMethod::Deflated);
        for (name, data) in &entries {
            zip.start_file(name, options)
                .context("写入 docx 条目失败")?;
            zip.write_all(data).context("写入 docx 条目内容失败")?;
        }
        zip.finish().context("完成 docx 包失败")?;
    }
    Ok(out.into_inner())
}

/// 将包含占位符的段落整体替换为 OMML 公式段落。
fn replace_marker_paragraph_with_omml(xml: &str, marker: &str, formula: &str) -> Result<String> {
    let marker_pos = xml
        .find(marker)
        .ok_or_else(|| anyhow::anyhow!("未找到公式占位符"))?;
    // 向前找所在段落起点（<w:p 或 <w:p>，排除 <w:pPr）
    let before = &xml[..marker_pos];
    let start = before
        .rfind("<w:p ")
        .into_iter()
        .chain(before.rfind("<w:p>"))
        .max()
        .ok_or_else(|| anyhow::anyhow!("未找到占位符段落起点"))?;
    let end_rel = xml[marker_pos..]
        .find("</w:p>")
        .ok_or_else(|| anyhow::anyhow!("未找到占位符段落终点"))?;
    let end = marker_pos + end_rel + "</w:p>".len();

    let omml = format!(
        "<m:oMathPara><m:oMath><m:r><m:rPr><m:sty m:val=\"p\"/></m:rPr><m:t>{}</m:t></m:r></m:oMath></m:oMathPara>",
        escape_xml(formula)
    );
    Ok(format!("{}{}{}", &xml[..start], omml, &xml[end..]))
}

/// XML 文本转义。
fn escape_xml(text: &str) -> String {
    text.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

/// 相对路径基于工作目录解析，绝对路径原样返回。
fn resolve_path(working_dir: &std::path::Path, input: &str) -> Result<PathBuf> {
    let path = PathBuf::from(input);
    Ok(if path.is_absolute() {
        path
    } else {
        working_dir.join(path)
    })
}

/// `office_render` 工具：调用 Pandoc 将 Markdown/HTML 渲染为 Word/PDF/PPT。
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

    /// 读取 zip（docx/xlsx）中指定条目的内容。
    fn read_zip_entry(path: &std::path::Path, name: &str) -> Option<String> {
        let file = std::fs::File::open(path).unwrap();
        let mut archive = zip::ZipArchive::new(file).unwrap();
        archive.by_name(name).ok().map(|mut f| {
            let mut s = String::new();
            std::io::Read::read_to_string(&mut f, &mut s).unwrap();
            s
        })
    }

    fn zip_entry_names(path: &std::path::Path) -> Vec<String> {
        let file = std::fs::File::open(path).unwrap();
        let mut archive = zip::ZipArchive::new(file).unwrap();
        (0..archive.len())
            .map(|i| archive.by_index(i).unwrap().name().to_string())
            .collect()
    }

    /// 在临时目录生成一张测试用 PNG。
    fn make_png(dir: &TempDir, name: &str) {
        let img = image::RgbImage::from_pixel(16, 16, image::Rgb([0, 128, 255]));
        img.save(dir.path().join(name)).unwrap();
    }

    #[tokio::test]
    async fn test_write_word_legacy_mode() {
        let dir = TempDir::new().unwrap();
        let tool = WriteWordTool;
        let mut args = HashMap::new();
        args.insert("path".to_string(), Value::String("legacy.docx".to_string()));
        args.insert(
            "content".to_string(),
            Value::String("第一行\n第二行".to_string()),
        );
        tool.execute(args, &ctx(&dir)).await.unwrap();

        let read_tool = ReadWordTool;
        let mut args = HashMap::new();
        args.insert("path".to_string(), Value::String("legacy.docx".to_string()));
        let result = read_tool.execute(args, &ctx(&dir)).await.unwrap();
        assert!(result.to_string_for_model().contains("第二行"));
    }

    #[tokio::test]
    async fn test_write_word_spec_complex() {
        let dir = TempDir::new().unwrap();
        make_png(&dir, "pic.png");
        let tool = WriteWordTool;
        let mut args = HashMap::new();
        args.insert(
            "path".to_string(),
            Value::String("complex.docx".to_string()),
        );
        args.insert(
            "spec".to_string(),
            json!({
                "title": "复杂文档",
                "author": "Clerk",
                "background": {"color": "F5F5DC"},
                "pagesetup": {"size": "A4", "orientation": "landscape", "margins": {"top": 25.4, "left": 19.0}},
                "elements": [
                    {"type": "heading", "level": 1, "text": "概述"},
                    {"type": "paragraph", "text": "红色加粗段落", "bold": true, "color": "#FF0000", "size": 14, "align": "center"},
                    {"type": "table", "headers": ["名称", "数量"], "rows": [["苹果", "3"], ["香蕉", "5"]]},
                    {"type": "image", "path": "pic.png", "width": 2.0},
                    {"type": "formula", "text": "E=mc^2"},
                    {"type": "page_break"},
                    {"type": "paragraph", "text": "第二页"}
                ]
            }),
        );
        tool.execute(args, &ctx(&dir)).await.unwrap();

        let path = dir.path().join("complex.docx");
        let document = read_zip_entry(&path, "word/document.xml").unwrap();
        // 背景颜色
        assert!(document.contains("<w:background w:color=\"F5F5DC\"/>"));
        // OMML 公式
        assert!(document.contains("m:oMathPara"));
        assert!(document.contains("E=mc^2"));
        assert!(document.contains("xmlns:m="));
        // 标题样式引用
        assert!(document.contains("w:val=\"Heading1\""));
        // 横向页面
        assert!(document.contains("w:orient=\"landscape\""));
        // 页边距：25.4mm = 1440 twips
        assert!(document.contains("w:top=\"1440\""));
        // 表格
        assert!(document.contains("<w:tbl>"));
        assert!(document.contains("苹果"));
        // 分页符
        assert!(document.contains("w:br w:type=\"page\""));

        // 元数据
        let core = read_zip_entry(&path, "docProps/core.xml").unwrap();
        assert!(core.contains("<dc:title>复杂文档</dc:title>"));
        assert!(core.contains("<dc:creator>Clerk</dc:creator>"));

        // 背景需开启 displayBackgroundShape
        let settings = read_zip_entry(&path, "word/settings.xml").unwrap();
        assert!(settings.contains("<w:displayBackgroundShape/>"));

        // docx-rs 可读回文本
        let read_tool = ReadWordTool;
        let mut args = HashMap::new();
        args.insert(
            "path".to_string(),
            Value::String("complex.docx".to_string()),
        );
        let result = read_tool.execute(args, &ctx(&dir)).await.unwrap();
        let text = result.to_string_for_model();
        assert!(text.contains("概述"));
        assert!(text.contains("第二页"));
    }

    #[tokio::test]
    async fn test_write_word_background_image() {
        let dir = TempDir::new().unwrap();
        make_png(&dir, "bg.png");
        let tool = WriteWordTool;
        let mut args = HashMap::new();
        args.insert("path".to_string(), Value::String("bg.docx".to_string()));
        args.insert(
            "spec".to_string(),
            json!({
                "background": {"image": "bg.png"},
                "elements": [{"type": "paragraph", "text": "带背景图片"}]
            }),
        );
        tool.execute(args, &ctx(&dir)).await.unwrap();

        let path = dir.path().join("bg.docx");
        // 背景图片已打入包内
        assert!(
            zip_entry_names(&path)
                .iter()
                .any(|n| n == "word/media/clerk-bg.png")
        );
        let document = read_zip_entry(&path, "word/document.xml").unwrap();
        assert!(document.contains("<w:background>"));
        assert!(document.contains("r:id=\"rIdClerkBg\""));
        let rels = read_zip_entry(&path, "word/_rels/document.xml.rels").unwrap();
        assert!(rels.contains("Id=\"rIdClerkBg\""));
        let settings = read_zip_entry(&path, "word/settings.xml").unwrap();
        assert!(settings.contains("<w:displayBackgroundShape/>"));
    }

    #[tokio::test]
    async fn test_write_word_missing_content_and_spec() {
        let dir = TempDir::new().unwrap();
        let tool = WriteWordTool;
        let mut args = HashMap::new();
        args.insert("path".to_string(), Value::String("none.docx".to_string()));
        assert!(tool.execute(args, &ctx(&dir)).await.is_err());
    }

    #[tokio::test]
    async fn test_write_excel_with_formula_chart_image() {
        let dir = TempDir::new().unwrap();
        make_png(&dir, "img.png");
        let tool = WriteExcelTool;
        let mut args = HashMap::new();
        args.insert("path".to_string(), Value::String("rich.xlsx".to_string()));
        args.insert(
            "rows".to_string(),
            json!([
                [{"value": "名称", "bold": true, "bg": "D9D9D9"}, {"value": "数量", "bold": true, "bg": "D9D9D9"}],
                ["苹果", 3],
                ["香蕉", 5],
                [{"value": "合计", "bold": true}, {"formula": "=SUM(B2:B3)", "color": "FF0000"}]
            ]),
        );
        args.insert(
            "charts".to_string(),
            json!([{"type": "column", "title": "数量", "categories": "A2:A3", "values": "B2:B3", "row": 5, "col": 0}]),
        );
        args.insert(
            "images".to_string(),
            json!([{"path": "img.png", "row": 0, "col": 3}]),
        );
        tool.execute(args, &ctx(&dir)).await.unwrap();

        let path = dir.path().join("rich.xlsx");
        // 图表与图片已写入包内
        let names = zip_entry_names(&path);
        assert!(names.iter().any(|n| n.starts_with("xl/charts/chart")));
        assert!(names.iter().any(|n| n.starts_with("xl/media/")));

        // 数据可读回
        let read_tool = ReadExcelTool;
        let mut args = HashMap::new();
        args.insert("path".to_string(), Value::String("rich.xlsx".to_string()));
        let result = read_tool.execute(args, &ctx(&dir)).await.unwrap();
        assert!(result.to_string_for_model().contains("苹果"));
    }
}
