//! 启动装配：按配置创建 LLM 客户端与完整的工具注册表，
//! TUI 与 GUI 共用同一套工具集。

use anyhow::Result;
use std::path::Path;
use std::sync::Arc;

use crate::agent::llm::{LlmClient, OpenAiClient};
use crate::agent::subagent_manager::SubagentManager;
use crate::config::{Config, PermissionConfig};
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

/// 创建工具注册表：注册全部本地工具，再基于共享的 SubagentManager 注册子 Agent 与协作工具。
pub fn create_tool_registry(
    working_dir: &Path,
    client: Arc<dyn LlmClient>,
    permissions: Option<PermissionConfig>,
) -> ToolRegistry {
    let mut registry = ToolRegistry::new(ToolContext {
        working_dir: working_dir.to_path_buf(),
        permissions,
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

/// 按配置创建 OpenAI 兼容 LLM 客户端。
pub fn create_llm_client(config: &Config) -> Result<Arc<dyn LlmClient>> {
    let client = OpenAiClient::from_config(&config.llm)?;
    Ok(Arc::new(client))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent::llm::{LlmResponse, Message, ToolDefinition};

    struct FakeLlm;

    #[async_trait::async_trait]
    impl LlmClient for FakeLlm {
        async fn chat(
            &self,
            _messages: Vec<Message>,
            _tools: Vec<ToolDefinition>,
        ) -> Result<LlmResponse> {
            Ok(LlmResponse::Text("ok".to_string()))
        }
    }

    #[test]
    fn test_create_tool_registry() {
        let client: Arc<dyn LlmClient> = Arc::new(FakeLlm);
        let registry = create_tool_registry(Path::new("/tmp"), client, None);
        let names = registry.names();
        assert!(names.contains(&"fs_read".to_string()));
        assert!(names.contains(&"shell".to_string()));
        assert!(names.contains(&"subagent_create".to_string()));
        assert!(names.contains(&"collaborate_parallel".to_string()));
        assert!(names.contains(&"write_skill".to_string()));
        assert!(names.contains(&"read_media_file".to_string()));
        assert!(names.contains(&"render_to_image".to_string()));
        // 未配置 permissions 时不需要审批（向后兼容）
        assert!(!registry.requires_approval("shell"));
    }

    #[test]
    fn test_all_tools_have_valid_schema() {
        let client: Arc<dyn LlmClient> = Arc::new(FakeLlm);
        let registry = create_tool_registry(Path::new("/tmp"), client, None);
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
    fn test_create_llm_client() {
        let mut config = Config::default();
        config.llm.api_key = "sk-test".to_string();
        let client = create_llm_client(&config).unwrap();
        // 仅验证创建成功即可
        drop(client);
    }
}
