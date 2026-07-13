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

fn default_temperature() -> f32 {
    0.7_f32
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
    #[serde(default = "default_temperature")]
    pub temperature: f32,
}

impl Default for LlmConfig {
    fn default() -> Self {
        Self {
            model: default_model(),
            base_url: default_base_url(),
            api_key: String::new(),
            timeout_seconds: 60,
            temperature: default_temperature(),
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
pub struct MultimodalConfig {
    #[serde(default)]
    pub supports_images: bool,
    #[serde(default)]
    pub supports_video: bool,
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
    pub multimodal: MultimodalConfig,
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

    pub fn save(&self, path: Option<&Path>) -> Result<()> {
        let config_path = match path {
            Some(p) => p.to_path_buf(),
            None => Self::default_config_path()?,
        };

        if let Some(parent) = config_path.parent() {
            fs::create_dir_all(parent)
                .with_context(|| format!("创建配置目录失败: {}", parent.display()))?;
        }

        let content = toml::to_string_pretty(self).context("序列化配置失败")?;
        fs::write(&config_path, content)
            .with_context(|| format!("写入配置文件失败: {}", config_path.display()))?;
        Ok(())
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
temperature = 0.7

# 某些模型（如 Moonshot kimi-k2.6）只支持 temperature = 1，可在此处覆盖。

[tui]
theme = "default"
show_sidebar = true

[storage]
# db_path = "/path/to/clerk.db"

[multimodal]
# supports_images = true
# supports_video = true

# working_dir = "/path/to/workspace"
"#
    .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn test_default_config() {
        let config = Config::default();
        assert_eq!(config.llm.model, "gpt-4o-mini");
        assert_eq!(config.llm.base_url, "https://api.openai.com/v1");
        assert!(config.llm.api_key.is_empty());
        assert_eq!(config.llm.timeout_seconds, 60);
        assert!((config.llm.temperature - 0.7_f32).abs() < f32::EPSILON);
        assert!(!config.tui.show_sidebar);
        assert!(config.storage.db_path.is_none());
    }

    #[test]
    fn test_load_from_file() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("config.toml");
        fs::write(
            &path,
            r#"
working_dir = "/tmp/wd"

[llm]
model = "gpt-4o"
api_key = "sk-test"
timeout_seconds = 120
temperature = 1.0

[storage]
db_path = "/tmp/test.db"
"#,
        )
        .unwrap();

        let config = Config::load(Some(&path)).unwrap();
        assert_eq!(config.llm.model, "gpt-4o");
        assert_eq!(config.llm.api_key, "sk-test");
        assert_eq!(config.llm.timeout_seconds, 120);
        assert!((config.llm.temperature - 1.0_f32).abs() < f32::EPSILON);
        assert_eq!(config.storage.db_path, Some(PathBuf::from("/tmp/test.db")));
        assert_eq!(config.working_dir, Some(PathBuf::from("/tmp/wd")));
    }

    #[test]
    fn test_load_missing_file_uses_default() {
        let config = Config::load(Some(Path::new("/nonexistent/path.toml"))).unwrap();
        assert_eq!(config.llm.model, "gpt-4o-mini");
    }

    #[test]
    fn test_load_invalid_toml_fails() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("bad.toml");
        fs::write(&path, "this is not toml").unwrap();
        assert!(Config::load(Some(&path)).is_err());
    }

    #[test]
    fn test_validate_with_empty_api_key() {
        let config = Config::default();
        assert!(config.validate().is_ok());
    }

    #[test]
    fn test_generate_example_config() {
        let example = generate_example_config();
        assert!(example.contains("[llm]"));
        assert!(example.contains("api_key"));
        assert!(example.contains("[tui]"));
        assert!(example.contains("[multimodal]"));
    }

    #[test]
    fn test_default_config_path() {
        let path = Config::default_config_path().unwrap();
        let s = path.to_string_lossy();
        assert!(s.contains("clerk"));
        assert!(s.contains("config.toml"));
    }

    #[test]
    fn test_default_db_path() {
        let path = Config::default_db_path().unwrap();
        let s = path.to_string_lossy();
        assert!(s.contains("clerk"));
        assert!(s.contains("clerk.db"));
    }

    #[test]
    fn test_validate_with_api_key() {
        let mut config = Config::default();
        config.llm.api_key = "sk-test".to_string();
        assert!(config.validate().is_ok());
    }

    #[test]
    fn test_save_and_load() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("saved.toml");
        let mut config = Config::default();
        config.llm.model = "gpt-4o".to_string();
        config.llm.api_key = "sk-save".to_string();
        config.llm.timeout_seconds = 90;
        config.llm.temperature = 1.0_f32;
        config.working_dir = Some(PathBuf::from("/tmp/wd"));
        config.multimodal.supports_images = true;
        config.multimodal.supports_video = true;

        config.save(Some(&path)).unwrap();
        let loaded = Config::load(Some(&path)).unwrap();
        assert_eq!(loaded.llm.model, "gpt-4o");
        assert_eq!(loaded.llm.api_key, "sk-save");
        assert_eq!(loaded.llm.timeout_seconds, 90);
        assert!((loaded.llm.temperature - 1.0_f32).abs() < f32::EPSILON);
        assert_eq!(loaded.working_dir, Some(PathBuf::from("/tmp/wd")));
        assert!(loaded.multimodal.supports_images);
        assert!(loaded.multimodal.supports_video);
    }

    #[test]
    fn test_save_creates_parent_directory() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("nested").join("config.toml");
        assert!(!path.exists());
        let config = Config::default();
        config.save(Some(&path)).unwrap();
        assert!(path.exists());
    }
}
