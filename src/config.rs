use anyhow::{Context, Result};
use directories::ProjectDirs;
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::{Path, PathBuf};
use tracing::{info, warn};

fn default_model() -> String {
    "gpt-4o-mini".to_string()
}

fn default_base_url() -> String {
    "https://api.openai.com/v1".to_string()
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LlmConfig {
    #[serde(default = "default_model")]
    pub model: String,
    #[serde(default = "default_base_url")]
    pub base_url: String,
    pub api_key: String,
    #[serde(default)]
    pub timeout_seconds: u64,
}

impl Default for LlmConfig {
    fn default() -> Self {
        Self {
            model: default_model(),
            base_url: default_base_url(),
            api_key: String::new(),
            timeout_seconds: 60,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct TuiConfig {
    #[serde(default)]
    pub theme: String,
    #[serde(default)]
    pub show_sidebar: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct StorageConfig {
    #[serde(default)]
    pub db_path: Option<PathBuf>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct Config {
    #[serde(default)]
    pub llm: LlmConfig,
    #[serde(default)]
    pub tui: TuiConfig,
    #[serde(default)]
    pub storage: StorageConfig,
    #[serde(default)]
    pub working_dir: Option<PathBuf>,
}

impl Config {
    pub fn load(path: Option<&Path>) -> Result<Self> {
        let config_path = match path {
            Some(p) => p.to_path_buf(),
            None => Self::default_config_path()?,
        };

        if !config_path.exists() {
            warn!("配置文件不存在，使用默认配置: {}", config_path.display());
            return Ok(Config::default());
        }

        info!("加载配置文件: {}", config_path.display());
        let content = fs::read_to_string(&config_path)
            .with_context(|| format!("无法读取配置文件: {}", config_path.display()))?;
        let config: Config = toml::from_str(&content)
            .with_context(|| format!("解析配置文件失败: {}", config_path.display()))?;
        Ok(config)
    }

    pub fn default_config_path() -> Result<PathBuf> {
        let dirs =
            ProjectDirs::from("com", "mikesolar", "clerk").context("无法确定项目配置目录")?;
        let config_dir = dirs.config_dir();
        fs::create_dir_all(config_dir)
            .with_context(|| format!("创建配置目录失败: {}", config_dir.display()))?;
        Ok(config_dir.join("config.toml"))
    }

    pub fn default_db_path() -> Result<PathBuf> {
        let dirs =
            ProjectDirs::from("com", "mikesolar", "clerk").context("无法确定项目数据目录")?;
        let data_dir = dirs.data_dir();
        fs::create_dir_all(data_dir)
            .with_context(|| format!("创建数据目录失败: {}", data_dir.display()))?;
        Ok(data_dir.join("clerk.db"))
    }

    pub fn validate(&self) -> Result<()> {
        if self.llm.api_key.is_empty() {
            warn!("LLM API key 未配置，运行时可能无法调用模型");
        }
        Ok(())
    }
}

pub fn generate_example_config() -> String {
    r#"# Clerk 配置文件

[llm]
model = "gpt-4o-mini"
base_url = "https://api.openai.com/v1"
api_key = "sk-xxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxx"
timeout_seconds = 60

[tui]
theme = "default"
show_sidebar = true

[storage]
# db_path = "/path/to/clerk.db"

# working_dir = "/path/to/workspace"
"#
    .to_string()
}
