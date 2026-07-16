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

fn default_timeout_seconds() -> u64 {
    600
}

/// LLM 配置：OpenAI 兼容接口的模型、地址、密钥与采样参数。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LlmConfig {
    #[serde(default = "default_model")]
    pub model: String,
    #[serde(default = "default_base_url")]
    pub base_url: String,
    pub api_key: String,
    #[serde(default = "default_timeout_seconds")]
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
            timeout_seconds: default_timeout_seconds(),
            temperature: default_temperature(),
        }
    }
}

/// TUI 界面配置。
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct TuiConfig {
    #[serde(default)]
    pub theme: String,
    #[serde(default)]
    pub show_sidebar: bool,
}

/// 存储配置：SQLite 数据库路径（可选，缺省用平台数据目录）。
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct StorageConfig {
    #[serde(default)]
    pub db_path: Option<PathBuf>,
}

/// 多模态能力配置：声明模型是否支持图片/视频输入。
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct MultimodalConfig {
    #[serde(default)]
    pub supports_images: bool,
    #[serde(default)]
    pub supports_video: bool,
}

fn default_auto_approve() -> Vec<String> {
    vec![
        "fs_read".to_string(),
        "fs_list".to_string(),
        "web_fetch".to_string(),
    ]
}

/// 工具审批配置：yolo 为 true 时全部自动批准；
/// 否则仅 auto_approve 列表中的工具自动批准，其余工具执行前需要用户确认。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PermissionConfig {
    #[serde(default)]
    pub yolo: bool,
    #[serde(default = "default_auto_approve")]
    pub auto_approve: Vec<String>,
}

impl Default for PermissionConfig {
    fn default() -> Self {
        Self {
            yolo: false,
            auto_approve: default_auto_approve(),
        }
    }
}

impl PermissionConfig {
    /// 判断指定工具执行前是否需要用户审批。
    pub fn requires_approval(&self, tool_name: &str) -> bool {
        !self.yolo && !self.auto_approve.iter().any(|t| t == tool_name)
    }
}

/// Clerk 顶层配置，对应 config.toml。
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
    /// 工具审批配置；缺省（None）时保持旧行为：所有工具无需审批直接执行。
    #[serde(default)]
    pub permissions: Option<PermissionConfig>,
    #[serde(default)]
    pub working_dir: Option<PathBuf>,
}

impl Config {
    /// 加载配置文件；路径缺省用 `~/.config/clerk/config.toml`，文件不存在时返回默认配置。
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

    /// 返回默认配置路径（不存在则创建目录）。
    pub fn default_config_path() -> Result<PathBuf> {
        let dirs =
            ProjectDirs::from("com", "mikesolar", "clerk").context("无法确定项目配置目录")?;
        let config_dir = dirs.config_dir();
        fs::create_dir_all(config_dir)
            .with_context(|| format!("创建配置目录失败: {}", config_dir.display()))?;
        Ok(config_dir.join("config.toml"))
    }

    /// 将配置以 TOML 写入指定路径（缺省为默认配置路径）。
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

    /// 返回默认数据库路径（不存在则创建目录）。
    pub fn default_db_path() -> Result<PathBuf> {
        let dirs =
            ProjectDirs::from("com", "mikesolar", "clerk").context("无法确定项目数据目录")?;
        let data_dir = dirs.data_dir();
        fs::create_dir_all(data_dir)
            .with_context(|| format!("创建数据目录失败: {}", data_dir.display()))?;
        Ok(data_dir.join("clerk.db"))
    }

    /// 校验配置；API key 为空时仅警告（本地功能仍可用）。
    pub fn validate(&self) -> Result<()> {
        if self.llm.api_key.is_empty() {
            warn!("LLM API key 未配置，运行时可能无法调用模型");
        }
        Ok(())
    }
}

/// 生成示例配置文本（与 config.example.toml 内容一致）。
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

# 工具审批：配置后，除 auto_approve 外的工具执行前需要用户确认。
# [permissions]
# yolo = false
# auto_approve = ["fs_read", "fs_list", "web_fetch"]

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
        assert_eq!(config.llm.timeout_seconds, 600);
        assert!((config.llm.temperature - 0.7_f32).abs() < f32::EPSILON);
        assert!(!config.tui.show_sidebar);
        assert!(config.storage.db_path.is_none());
        assert!(config.permissions.is_none());
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
        assert!(example.contains("[permissions]"));
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
    fn test_load_permissions_config() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("config.toml");
        fs::write(
            &path,
            r#"
[llm]
api_key = "sk-test"

[permissions]
yolo = true
auto_approve = ["fs_read"]
"#,
        )
        .unwrap();

        let config = Config::load(Some(&path)).unwrap();
        let permissions = config.permissions.unwrap();
        assert!(permissions.yolo);
        assert_eq!(permissions.auto_approve, vec!["fs_read".to_string()]);
    }

    #[test]
    fn test_load_empty_permissions_uses_defaults() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("config.toml");
        fs::write(
            &path,
            r#"
[llm]
api_key = "sk-test"

[permissions]
"#,
        )
        .unwrap();

        let config = Config::load(Some(&path)).unwrap();
        let permissions = config.permissions.unwrap();
        assert!(!permissions.yolo);
        assert_eq!(
            permissions.auto_approve,
            vec![
                "fs_read".to_string(),
                "fs_list".to_string(),
                "web_fetch".to_string()
            ]
        );
    }

    #[test]
    fn test_load_without_permissions_is_none() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("config.toml");
        fs::write(
            &path,
            r#"
[llm]
api_key = "sk-test"
"#,
        )
        .unwrap();

        let config = Config::load(Some(&path)).unwrap();
        assert!(config.permissions.is_none());
    }

    #[test]
    fn test_permission_requires_approval() {
        let permissions = PermissionConfig::default();
        assert!(!permissions.requires_approval("fs_read"));
        assert!(!permissions.requires_approval("fs_list"));
        assert!(!permissions.requires_approval("web_fetch"));
        assert!(permissions.requires_approval("fs_write"));
        assert!(permissions.requires_approval("shell"));

        let yolo = PermissionConfig {
            yolo: true,
            ..Default::default()
        };
        assert!(!yolo.requires_approval("shell"));

        let custom = PermissionConfig {
            yolo: false,
            auto_approve: vec!["shell".to_string()],
        };
        assert!(!custom.requires_approval("shell"));
        assert!(custom.requires_approval("fs_read"));
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
        config.permissions = Some(PermissionConfig {
            yolo: false,
            auto_approve: vec!["fs_read".to_string()],
        });

        config.save(Some(&path)).unwrap();
        let loaded = Config::load(Some(&path)).unwrap();
        assert_eq!(loaded.llm.model, "gpt-4o");
        assert_eq!(loaded.llm.api_key, "sk-save");
        assert_eq!(loaded.llm.timeout_seconds, 90);
        assert!((loaded.llm.temperature - 1.0_f32).abs() < f32::EPSILON);
        assert_eq!(loaded.working_dir, Some(PathBuf::from("/tmp/wd")));
        assert!(loaded.multimodal.supports_images);
        assert!(loaded.multimodal.supports_video);
        let permissions = loaded.permissions.unwrap();
        assert!(!permissions.yolo);
        assert_eq!(permissions.auto_approve, vec!["fs_read".to_string()]);
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
