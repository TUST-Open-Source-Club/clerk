mod agent;
mod app;
mod cli;
mod config;
mod mcp;
mod skills;
mod store;
mod tools;
mod ui;
mod util;

use anyhow::{Context, Result};
use crossterm::{
    execute,
    terminal::{EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode},
};
use ratatui::{Terminal, backend::CrosstermBackend};
use std::env;
use std::io::{self, BufRead, Write};
use std::path::Path;
use std::sync::Arc;
use tokio::sync::Mutex;
use tracing::{error, info};

use crate::agent::llm::OpenAiClient;
use crate::agent::runner::ReActRunner;
use crate::agent::session::SessionContext;
use crate::agent::subagent_manager::SubagentManager;
use crate::app::App;
use crate::config::Config;
use crate::store::Store;
use crate::tools::collaborate::{CollaborateParallelTool, CollaborateSequentialTool};
use crate::tools::media::ReadMediaFile;
use crate::tools::registry::ToolRegistry;
use crate::tools::render_image::RenderToImage;
use crate::tools::schema::ToolContext;
use crate::tools::subagent::{
    SubagentCreateTool, SubagentDeleteTool, SubagentListTool, SubagentRunTool,
};
use crate::tools::write_skill::WriteSkillTool;
use crate::tools::{browser, fs, office, pdf, poster, shell, web};

fn setup_logging() -> Result<()> {
    let filter = env::var("RUST_LOG").unwrap_or_else(|_| "clerk=info".to_string());
    let data_dir = Config::default_db_path()?
        .parent()
        .context("无法获取数据目录")?
        .to_path_buf();
    let appender = tracing_appender::rolling::RollingFileAppender::new(
        tracing_appender::rolling::Rotation::NEVER,
        data_dir,
        "clerk.log",
    );
    let (non_blocking, guard) = tracing_appender::non_blocking(appender);
    // 保持 guard 存活到进程结束，确保日志刷新
    let _guard = Box::leak(Box::new(guard));

    tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_writer(non_blocking)
        .init();
    Ok(())
}

fn create_tool_registry(
    working_dir: &std::path::Path,
    client: Arc<dyn crate::agent::llm::LlmClient>,
) -> ToolRegistry {
    let mut registry = ToolRegistry::new(ToolContext {
        working_dir: working_dir.to_path_buf(),
    });
    registry.register(Arc::new(fs::ReadFileTool));
    registry.register(Arc::new(fs::WriteFileTool));
    registry.register(Arc::new(fs::ListDirTool));
    registry.register(Arc::new(shell::ShellTool));
    registry.register(Arc::new(web::WebFetchTool));
    registry.register(Arc::new(web::WebPostTool));
    registry.register(Arc::new(browser::BrowserTool::new()));
    registry.register(Arc::new(office::ReadExcelTool));
    registry.register(Arc::new(office::WriteExcelTool));
    registry.register(Arc::new(office::ReadWordTool));
    registry.register(Arc::new(office::WriteWordTool));
    registry.register(Arc::new(office::RenderOfficeTool));
    registry.register(Arc::new(pdf::MergePdfTool));
    registry.register(Arc::new(pdf::SplitPdfTool));
    registry.register(Arc::new(poster::PosterTool));
    registry.register(Arc::new(ReadMediaFile));
    registry.register(Arc::new(RenderToImage));

    let manager = Arc::new(SubagentManager::new(client, registry.clone()));
    registry.register(Arc::new(SubagentCreateTool::new(manager.clone())));
    registry.register(Arc::new(SubagentRunTool::new(manager.clone())));
    registry.register(Arc::new(SubagentListTool::new(manager.clone())));
    registry.register(Arc::new(SubagentDeleteTool::new(manager.clone())));
    registry.register(Arc::new(CollaborateParallelTool::new(manager.clone())));
    registry.register(Arc::new(CollaborateSequentialTool::new(manager.clone())));
    registry.register(Arc::new(WriteSkillTool::new()));

    registry
}

fn create_llm_client(config: &Config) -> Result<Arc<dyn crate::agent::llm::LlmClient>> {
    let client = OpenAiClient::from_config(&config.llm)?;
    Ok(Arc::new(client))
}

fn read_line<R: BufRead>(reader: &mut R) -> Result<String> {
    let mut line = String::new();
    reader.read_line(&mut line).context("读取输入失败")?;
    Ok(line
        .trim_end_matches('\n')
        .trim_end_matches('\r')
        .to_string())
}

fn run_config_wizard<R: BufRead, W: Write>(
    path: &Path,
    reader: &mut R,
    writer: &mut W,
) -> Result<Config> {
    writer.write_all("欢迎使用 Clerk！首次使用需要配置 LLM。\n".as_bytes())?;

    writer.write_all("请输入 API Base URL [https://api.openai.com/v1]: ".as_bytes())?;
    writer.flush()?;
    let base_url = read_line(reader)?;
    let base_url = if base_url.is_empty() {
        "https://api.openai.com/v1".to_string()
    } else {
        base_url
    };

    writer.write_all("请输入 Model [gpt-4o-mini]: ".as_bytes())?;
    writer.flush()?;
    let model = read_line(reader)?;
    let model = if model.is_empty() {
        "gpt-4o-mini".to_string()
    } else {
        model
    };

    writer.write_all("请输入 API Key: ".as_bytes())?;
    writer.flush()?;
    let api_key = read_line(reader)?;

    writer.write_all("请输入超时时间（秒）[600]: ".as_bytes())?;
    writer.flush()?;
    let timeout_line = read_line(reader)?;
    let timeout_seconds = if timeout_line.is_empty() {
        600
    } else {
        timeout_line.parse().context("超时时间必须是有效的数字")?
    };

    writer.write_all("请输入 temperature [0.7]: ".as_bytes())?;
    writer.flush()?;
    let temperature_line = read_line(reader)?;
    let temperature: f32 = if temperature_line.is_empty() {
        0.7_f32
    } else {
        temperature_line
            .parse()
            .context("temperature 必须是有效的数字")?
    };

    writer.write_all("请输入工作目录 [当前目录]: ".as_bytes())?;
    writer.flush()?;
    let working_dir_line = read_line(reader)?;
    let working_dir = if working_dir_line.is_empty() {
        env::current_dir().context("无法获取当前目录")?
    } else {
        std::path::PathBuf::from(working_dir_line)
    };

    writer.write_all("模型是否支持图片输入 (y/N): ".as_bytes())?;
    writer.flush()?;
    let supports_images = parse_yes_no(&read_line(reader)?);

    writer.write_all("模型是否支持视频输入 (y/N): ".as_bytes())?;
    writer.flush()?;
    let supports_video = parse_yes_no(&read_line(reader)?);

    let mut config = Config::default();
    config.llm.base_url = base_url;
    config.llm.model = model;
    config.llm.api_key = api_key;
    config.llm.timeout_seconds = timeout_seconds;
    config.llm.temperature = temperature;
    config.working_dir = Some(working_dir);
    config.multimodal.supports_images = supports_images;
    config.multimodal.supports_video = supports_video;

    config.save(Some(path))?;
    info!("配置已保存到: {}", path.display());
    Ok(config)
}

fn parse_yes_no(line: &str) -> bool {
    matches!(line.trim().to_lowercase().as_str(), "y" | "yes" | "是")
}

async fn run_app() -> Result<()> {
    let args = cli::parse();

    let config_path = match &args.config {
        Some(p) => p.clone(),
        None => Config::default_config_path()?,
    };

    let config = if args.setup || !config_path.exists() {
        let stdin = io::stdin();
        run_config_wizard(&config_path, &mut stdin.lock(), &mut io::stdout())?
    } else {
        Config::load(args.config.as_deref())?
    };
    config.validate()?;

    if args.check_config {
        info!("配置检查通过");
        return Ok(());
    }

    let working_dir = args
        .working_dir
        .or(config.working_dir.clone())
        .unwrap_or_else(|| env::current_dir().unwrap());
    env::set_current_dir(&working_dir)
        .with_context(|| format!("无法切换到工作目录: {}", working_dir.display()))?;
    info!("工作目录: {}", working_dir.display());

    let db_path = match &config.storage.db_path {
        Some(p) => p.clone(),
        None => Config::default_db_path()?,
    };
    let store = Store::open(&db_path).await?;

    let client = create_llm_client(&config)?;
    let registry = Arc::new(Mutex::new(create_tool_registry(
        &working_dir,
        client.clone(),
    )));

    if let Some(command) = args.command {
        // 非交互模式：使用 ReActRunner 处理命令
        info!("执行命令: {}", command);
        let session_id = uuid::Uuid::new_v4().to_string();
        store.create_session(&session_id, Some("命令会话")).await?;
        store.add_message(&session_id, "user", &command).await?;

        let runner = ReActRunner::new(client, registry);
        let mut ctx = SessionContext::new(build_system_prompt());
        let reply = runner.run(&mut ctx, &command, None).await?;

        store.add_message(&session_id, "assistant", &reply).await?;
        println!("{}", reply);
        return Ok(());
    }

    enable_raw_mode()?;
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen)?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;

    let app = App::new(store, client, registry, config.multimodal.clone()).await?;
    let result = app.run(&mut terminal).await;

    restore_terminal(&mut terminal)?;
    result
}

fn build_system_prompt() -> String {
    r#"你是一个终端办公 AI Agent，名为 Clerk。
你可以使用以下工具帮助用户：
- fs_read: 读取文件内容
- fs_write: 写入文件内容
- fs_list: 列出目录内容
- shell: 执行 shell 命令
- web_fetch: 获取网页内容
- web_post: 发送 POST 请求
- browser: 使用无头 Chromium 浏览器操作网页、生成 PDF/截图
- office_read_excel / office_write_excel: Excel 读写
- office_read_word / office_write_word: Word 读写
- office_render: 使用 Pandoc 渲染复杂 Word/PDF/PPT（支持模板、公式、图片）
- pdf_merge / pdf_split: PDF 合并与拆分
- poster: HTML 转海报 PDF/PNG
- read_media_file: 读取图片/视频文件并返回 base64 数据 URL
- render_to_image: 将 HTML/PDF/Office/图片渲染为 PNG 预览图
- subagent_create / subagent_run / subagent_list / subagent_delete: 创建并运行子 Agent
- collaborate_parallel / collaborate_sequential: 多子 Agent 并行/顺序协作
- write_skill: 将领域知识保存为 SKILL.md，供后续复用
请根据用户需求判断是否需要调用工具，并简洁地回复。"#
        .to_string()
}

fn restore_terminal(terminal: &mut Terminal<CrosstermBackend<io::Stdout>>) -> Result<()> {
    disable_raw_mode()?;
    execute!(terminal.backend_mut(), LeaveAlternateScreen)?;
    terminal.show_cursor()?;
    Ok(())
}

#[tokio::main]
async fn main() {
    if let Err(e) = setup_logging() {
        eprintln!("日志初始化失败: {}", e);
    }

    if let Err(e) = run_app().await {
        error!("应用运行失败: {:#}", e);
        eprintln!("错误: {:#}", e);
        std::process::exit(1);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;
    use tempfile::TempDir;

    #[test]
    fn test_setup_logging_once() {
        use std::sync::Once;
        static INIT: Once = Once::new();
        let mut result = Ok(());
        INIT.call_once(|| {
            result = setup_logging();
        });
        assert!(result.is_ok());
    }

    #[test]
    fn test_generate_example_config() {
        let example = config::generate_example_config();
        assert!(example.contains("[llm]"));
        assert!(example.contains("api_key"));
    }

    #[test]
    fn test_create_tool_registry() {
        let client: Arc<dyn crate::agent::llm::LlmClient> = Arc::new(FakeLlm);
        let registry = create_tool_registry(std::path::Path::new("/tmp"), client);
        let names = registry.names();
        assert!(names.contains(&"fs_read".to_string()));
        assert!(names.contains(&"shell".to_string()));
        assert!(names.contains(&"subagent_create".to_string()));
        assert!(names.contains(&"collaborate_parallel".to_string()));
        assert!(names.contains(&"write_skill".to_string()));
        assert!(names.contains(&"read_media_file".to_string()));
        assert!(names.contains(&"render_to_image".to_string()));
    }

    #[test]
    fn test_all_tools_have_valid_schema() {
        let client: Arc<dyn crate::agent::llm::LlmClient> = Arc::new(FakeLlm);
        let registry = create_tool_registry(std::path::Path::new("/tmp"), client);
        for name in registry.names() {
            let tool = registry.get(&name).unwrap();
            assert_eq!(tool.name(), name);
            assert!(!tool.description().is_empty());
            let schema = tool.schema();
            assert_eq!(schema.name, name);
            let _ = schema.into_tool_definition();
        }
    }

    #[test]
    fn test_build_system_prompt_contains_tools() {
        let prompt = build_system_prompt();
        assert!(prompt.contains("subagent_create"));
        assert!(prompt.contains("collaborate_parallel"));
        assert!(prompt.contains("write_skill"));
        assert!(prompt.contains("read_media_file"));
        assert!(prompt.contains("render_to_image"));
    }

    #[test]
    fn test_create_llm_client() {
        let mut config = Config::default();
        config.llm.api_key = "sk-test".to_string();
        let client = create_llm_client(&config).unwrap();
        // 仅验证创建成功即可
        drop(client);
    }

    #[test]
    fn test_run_config_wizard_with_defaults() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("wizard.toml");
        let answers = "\n\n\n\n\n\n\n\n";
        let mut output: Vec<u8> = Vec::new();
        let config = run_config_wizard(&path, &mut Cursor::new(answers), &mut output).unwrap();

        assert_eq!(config.llm.base_url, "https://api.openai.com/v1");
        assert_eq!(config.llm.model, "gpt-4o-mini");
        assert!(config.llm.api_key.is_empty());
        assert_eq!(config.llm.timeout_seconds, 600);
        assert!((config.llm.temperature - 0.7_f32).abs() < f32::EPSILON);
        assert_eq!(config.working_dir, Some(env::current_dir().unwrap()));
        assert!(!config.multimodal.supports_images);
        assert!(!config.multimodal.supports_video);
        assert!(path.exists());
    }

    #[test]
    fn test_run_config_wizard_with_custom_values() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("wizard.toml");
        let answers = "https://api.example.com/v1\ngpt-4o\nsk-123\n30\n1.0\n/tmp/wd\ny\ny\n";
        let mut output: Vec<u8> = Vec::new();
        let config = run_config_wizard(&path, &mut Cursor::new(answers), &mut output).unwrap();

        assert_eq!(config.llm.base_url, "https://api.example.com/v1");
        assert_eq!(config.llm.model, "gpt-4o");
        assert_eq!(config.llm.api_key, "sk-123");
        assert_eq!(config.llm.timeout_seconds, 30);
        assert!((config.llm.temperature - 1.0_f32).abs() < f32::EPSILON);
        assert_eq!(
            config.working_dir,
            Some(std::path::PathBuf::from("/tmp/wd"))
        );
        assert!(config.multimodal.supports_images);
        assert!(config.multimodal.supports_video);
    }

    #[test]
    fn test_read_line_trims_newline() {
        let mut input = Cursor::new("hello\r\n");
        assert_eq!(read_line(&mut input).unwrap(), "hello");

        let mut input2 = Cursor::new("world\n");
        assert_eq!(read_line(&mut input2).unwrap(), "world");
    }

    #[test]
    fn test_parse_yes_no() {
        assert!(parse_yes_no("y"));
        assert!(parse_yes_no("Y"));
        assert!(parse_yes_no("yes"));
        assert!(parse_yes_no("是"));
        assert!(parse_yes_no("  Y  "));
        assert!(!parse_yes_no("n"));
        assert!(!parse_yes_no(""));
        assert!(!parse_yes_no("no"));
    }

    struct FakeLlm;

    #[async_trait::async_trait]
    impl crate::agent::llm::LlmClient for FakeLlm {
        async fn chat(
            &self,
            _messages: Vec<crate::agent::llm::Message>,
            _tools: Vec<crate::agent::llm::ToolDefinition>,
        ) -> anyhow::Result<crate::agent::llm::LlmResponse> {
            Ok(crate::agent::llm::LlmResponse::Text("ok".to_string()))
        }
    }
}
