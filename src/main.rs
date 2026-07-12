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
use std::sync::Arc;
use std::{env, io};
use tokio::sync::Mutex;
use tracing::{error, info};

use crate::agent::llm::OpenAiClient;
use crate::agent::runner::ReActRunner;
use crate::agent::session::SessionContext;
use crate::app::App;
use crate::config::Config;
use crate::store::Store;
use crate::tools::registry::ToolRegistry;
use crate::tools::schema::ToolContext;
use crate::tools::{fs, shell};

fn setup_logging() -> Result<()> {
    let filter = env::var("RUST_LOG").unwrap_or_else(|_| "clerk=info".to_string());
    tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_writer(std::io::stderr)
        .init();
    Ok(())
}

fn create_tool_registry(working_dir: &std::path::Path) -> ToolRegistry {
    let mut registry = ToolRegistry::new(ToolContext {
        working_dir: working_dir.to_path_buf(),
    });
    registry.register(Box::new(fs::ReadFileTool));
    registry.register(Box::new(fs::WriteFileTool));
    registry.register(Box::new(fs::ListDirTool));
    registry.register(Box::new(shell::ShellTool));
    registry
}

fn create_llm_client(config: &Config) -> Result<Arc<dyn crate::agent::llm::LlmClient>> {
    let client = OpenAiClient::from_config(&config.llm)?;
    Ok(Arc::new(client))
}

async fn run_app() -> Result<()> {
    let args = cli::parse();
    let config = Config::load(args.config.as_deref())?;
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

    let registry = Arc::new(Mutex::new(create_tool_registry(&working_dir)));
    let client = create_llm_client(&config)?;

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

    let app = App::new(store, client, registry).await?;
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

    #[test]
    fn test_generate_example_config() {
        let example = config::generate_example_config();
        assert!(example.contains("[llm]"));
        assert!(example.contains("api_key"));
    }

    #[test]
    fn test_create_tool_registry() {
        let registry = create_tool_registry(std::path::Path::new("/tmp"));
        let names = registry.names();
        assert!(names.contains(&"fs_read".to_string()));
        assert!(names.contains(&"shell".to_string()));
    }
}
