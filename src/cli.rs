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

    /// 强制运行首次配置向导
    #[arg(long)]
    pub setup: bool,
}

pub fn parse() -> Args {
    Args::parse()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_default() {
        let args = Args::try_parse_from(["clerk"]).unwrap();
        assert!(args.config.is_none());
        assert!(args.working_dir.is_none());
        assert!(args.command.is_none());
        assert!(!args.check_config);
        assert!(!args.setup);
    }

    #[test]
    fn test_parse_all_options() {
        let args = Args::try_parse_from([
            "clerk",
            "--config",
            "/tmp/c.toml",
            "--working-dir",
            "/tmp/wd",
            "--command",
            "hello",
            "--check-config",
            "--setup",
        ])
        .unwrap();
        assert_eq!(args.config, Some(PathBuf::from("/tmp/c.toml")));
        assert_eq!(args.working_dir, Some(PathBuf::from("/tmp/wd")));
        assert_eq!(args.command, Some("hello".to_string()));
        assert!(args.check_config);
        assert!(args.setup);
    }

    #[test]
    fn test_parse_short_flags() {
        let args =
            Args::try_parse_from(["clerk", "-c", "/tmp/c.toml", "-w", "/tmp/wd", "-x", "run"])
                .unwrap();
        assert_eq!(args.config, Some(PathBuf::from("/tmp/c.toml")));
        assert_eq!(args.working_dir, Some(PathBuf::from("/tmp/wd")));
        assert_eq!(args.command, Some("run".to_string()));
    }
}
