use clap::Parser;
use std::path::PathBuf;

#[derive(Parser, Debug)]
#[command(name = "clerk")]
#[command(about = "Clerk - 终端办公 AI Agent", long_about = None)]
pub struct Args {
    /// 配置文件路径
    #[arg(short, long, value_name = "FILE")]
    pub config: Option<PathBuf>,

    /// 工作目录
    #[arg(short, long, value_name = "DIR")]
    pub working_dir: Option<PathBuf>,

    /// 启动时直接执行一条命令后退出（非交互模式）
    #[arg(short = 'x', long, value_name = "COMMAND")]
    pub command: Option<String>,

    /// 仅检查配置并退出
    #[arg(long)]
    pub check_config: bool,
}

pub fn parse() -> Args {
    Args::parse()
}
