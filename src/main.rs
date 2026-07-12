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
    terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen},
};
use ratatui::{
    backend::CrosstermBackend,
    Terminal,
};
use std::{env, io};
use tracing::{error, info};

use crate::app::App;
use crate::config::Config;
use crate::store::Store;

fn setup_logging() -> Result<()> {
    let filter = env::var("RUST_LOG").unwrap_or_else(|_| "clerk=info".to_string());
    tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_writer(std::io::stderr)
        .init();
    Ok(())
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

    if let Some(command) = args.command {
        // 非交互模式：简单处理命令后退出
        info!("执行命令: {}", command);
        let session_id = uuid::Uuid::new_v4().to_string();
        store.create_session(&session_id, Some("命令会话")).await?;
        store.add_message(&session_id, "user", &command).await?;
        store
            .add_message(&session_id, "assistant", "命令模式在阶段 0 仅保存输入，阶段 1 接入 LLM 后处理。")
            .await?;
        return Ok(());
    }

    enable_raw_mode()?;
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen)?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;

    let app = App::new(store).await?;
    let result = app.run(&mut terminal).await;

    restore_terminal(&mut terminal)?;
    result
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
}
