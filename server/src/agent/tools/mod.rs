pub mod agnes_media;
pub mod artifact_edit;
pub mod chart_generate;
pub mod data_tools;
pub mod doc_generate;
pub mod drawio_generate;
pub mod image_prompt;
pub mod local_video;
pub mod md_generate;
pub mod ppt_generate;
pub mod ppt_plan;
pub mod propose_plan;
pub mod sheet_generate;
pub mod schedule_tools;
pub mod skill_tools;
pub mod video_batch_generate;
pub mod video_generate;
pub mod video_storyboard;
pub mod web_search_generic;
pub mod workspace_tools;

use super::registry::REGISTRY;
use std::sync::Arc;

pub async fn register_all_tools() {
    REGISTRY.register(Arc::new(ppt_plan::PptPlanTool)).await;
    REGISTRY
        .register(Arc::new(ppt_generate::PptGenerateTool))
        .await;
    REGISTRY
        .register(Arc::new(doc_generate::DocGenerateTool))
        .await;
    REGISTRY
        .register(Arc::new(md_generate::MarkdownGenerateTool))
        .await;
    REGISTRY
        .register(Arc::new(sheet_generate::SheetGenerateTool))
        .await;
    REGISTRY
        .register(Arc::new(chart_generate::ChartGenerateTool))
        .await;
    REGISTRY
        .register(Arc::new(drawio_generate::DrawioGenerateTool))
        .await;
    REGISTRY
        .register(Arc::new(image_prompt::ImagePromptTool))
        .await;
    REGISTRY
        .register(Arc::new(video_generate::VideoGenerateTool))
        .await;
    REGISTRY
        .register(Arc::new(video_batch_generate::VideoBatchGenerateTool))
        .await;
    REGISTRY
        .register(Arc::new(video_storyboard::VideoStoryboardTool))
        .await;
    REGISTRY.register(Arc::new(web_search_generic::WebSearchTool)).await;
    REGISTRY.register(Arc::new(propose_plan::ProposePlanTool)).await;
    REGISTRY.register(Arc::new(artifact_edit::ArtifactEditTool)).await;
    REGISTRY.register(Arc::new(workspace_tools::WorkspaceListTool)).await;
    REGISTRY.register(Arc::new(data_tools::DataProfileTool)).await;
    REGISTRY.register(Arc::new(skill_tools::UseSkillTool)).await;
    REGISTRY.register(Arc::new(schedule_tools::ScheduleCreateTool)).await;
    REGISTRY.register(Arc::new(schedule_tools::ScheduleListTool)).await;
    REGISTRY.register(Arc::new(skill_tools::ReadSkillFileTool)).await;
    REGISTRY.register(Arc::new(data_tools::DataQueryTool)).await;
    REGISTRY.register(Arc::new(workspace_tools::WorkspaceSearchTool)).await;
    REGISTRY.register(Arc::new(workspace_tools::WorkspaceReadTool)).await;
    REGISTRY.register(Arc::new(workspace_tools::WorkspaceWriteTool)).await;
    REGISTRY.register(Arc::new(workspace_tools::WorkspaceOrganizeTool)).await;

    let tools = REGISTRY.list().await;
    tracing::info!(
        "[AgentTools] 已注册 {} 个工具: {}",
        tools.len(),
        tools
            .iter()
            .map(|t| t.name())
            .collect::<Vec<_>>()
            .join(", ")
    );
}
