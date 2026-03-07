//! Multi-agent tool registration helper

use crate::agent::collaboration::team::TeamManager;
use crate::agent::collaboration::workflow::WorkflowEngine;
use crate::agent::collaboration::workspace::SharedWorkspace;
use crate::agent::communication::AgentId;
use crate::tools::{
    agent_message::{BroadcastToTeamTool, RespondToApprovalTool, SendAgentMessageTool},
    team::{CreateTeamTool, GetTeamStatusTool, ListTeamsTool},
    workflow::{GetWorkflowStatusTool, ListWorkflowsTool, StartWorkflowTool},
    workspace::{CreateArtifactTool, GetArtifactTool, ListArtifactsTool},
    ToolRegistry,
};
use std::sync::Arc;

/// Register all multi-agent collaboration tools
///
/// This function registers team, workflow, workspace, and agent communication tools.
/// It requires instances of TeamManager, WorkflowEngine, and SharedWorkspace.
///
/// Note: For agent-specific tools (like SendAgentMessageTool), you may need to
/// provide the agent's AgentId. These tools are typically registered per-agent
/// when creating team members.
pub async fn register_multi_agent_tools(
    registry: &mut ToolRegistry,
    team_manager: Arc<TeamManager>,
    workflow_engine: Arc<WorkflowEngine>,
    workspace: Arc<SharedWorkspace>,
    agent_id: Option<Arc<AgentId>>,
) {
    // Team management tools
    registry.register(CreateTeamTool::new(team_manager.clone()));
    registry.register(ListTeamsTool::new(team_manager.clone()));
    registry.register(GetTeamStatusTool::new(team_manager.clone()));

    // Workflow tools
    registry.register(StartWorkflowTool::new(
        workflow_engine.clone(),
        team_manager.clone(),
    ));
    registry.register(GetWorkflowStatusTool::new(workflow_engine.clone()));
    registry.register(ListWorkflowsTool::new(workflow_engine.clone()));

    // Workspace tools
    if let Some(ref agent_id) = agent_id {
        registry.register(CreateArtifactTool::new(workspace.clone(), agent_id.clone()));
    }
    registry.register(GetArtifactTool::new(workspace.clone()));
    registry.register(ListArtifactsTool::new(workspace.clone()));

    // Agent communication tools
    registry.register(SendAgentMessageTool::new(team_manager.clone()));
    registry.register(BroadcastToTeamTool::new(team_manager.clone()));
    registry.register(RespondToApprovalTool::new(team_manager.clone()));
}
