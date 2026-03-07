//! Workflow management tools for multi-agent collaboration

use super::base::{SimpleTool, ToolInput, ToolResult};
use crate::agent::collaboration::{
    team::TeamManager, WorkflowEngine, create_code_review_workflow, create_design_workflow,
};
use async_trait::async_trait;
use mofa_sdk::agent::ToolCategory;
use serde_json::json;
use std::collections::HashMap;
use std::sync::Arc;

/// Tool to start a workflow execution
pub struct StartWorkflowTool {
    workflow_engine: Arc<WorkflowEngine>,
    team_manager: Arc<TeamManager>,
}

impl StartWorkflowTool {
    pub fn new(workflow_engine: Arc<WorkflowEngine>, team_manager: Arc<TeamManager>) -> Self {
        Self {
            workflow_engine,
            team_manager,
        }
    }
}

#[async_trait]
impl SimpleTool for StartWorkflowTool {
    fn name(&self) -> &str {
        "start_workflow"
    }

    fn description(&self) -> &str {
        "Start a workflow execution with a team. Available workflows: 'code_review', 'design'"
    }

    fn parameters_schema(&self) -> serde_json::Value {
        json!({
            "type": "object",
            "properties": {
                "workflow_name": {
                    "type": "string",
                    "description": "Name of the workflow to execute (code_review, design)",
                    "enum": ["code_review", "design"]
                },
                "team_id": {
                    "type": "string",
                    "description": "Team ID to execute the workflow with"
                },
                "initial_context": {
                    "type": "object",
                    "description": "Initial context variables for the workflow",
                    "additionalProperties": true
                }
            },
            "required": ["workflow_name", "team_id"]
        })
    }

    async fn execute(&self, input: ToolInput) -> ToolResult {
        let workflow_name = match input.get_str("workflow_name") {
            Some(name) => name,
            None => return ToolResult::failure("Missing 'workflow_name' parameter"),
        };

        let team_id = match input.get_str("team_id") {
            Some(id) => id,
            None => return ToolResult::failure("Missing 'team_id' parameter"),
        };

        // Get initial context (optional)
        let initial_context: HashMap<String, serde_json::Value> =
            match input.get::<serde_json::Value>("initial_context") {
                Some(v) => {
                    if let Some(obj) = v.as_object() {
                        let mut map = HashMap::new();
                        for (k, v) in obj {
                            map.insert(k.clone(), v.clone());
                        }
                        map
                    } else {
                        HashMap::new()
                    }
                }
                None => HashMap::new(),
            };

        // Get team from team manager
        let team = match self.team_manager.get_team(team_id).await {
            Some(t) => t,
            None => return ToolResult::failure(format!("Team '{}' not found", team_id)),
        };

        // Create workflow based on name
        let workflow = match workflow_name {
            "code_review" => create_code_review_workflow(),
            "design" => create_design_workflow(),
            _ => return ToolResult::failure(format!("Unknown workflow: {}", workflow_name)),
        };

        // Execute workflow
        match self
            .workflow_engine
            .execute_workflow(workflow, team, initial_context)
            .await
        {
            Ok(result) => ToolResult::success_text(
                serde_json::to_string(&json!({
                    "workflow_id": result.workflow_id,
                    "success": result.success,
                    "completed_steps": result.completed_steps,
                    "error": result.error
                }))
                .unwrap_or_else(|_| format!("Workflow executed: {}", result.workflow_id)),
            ),
            Err(e) => ToolResult::failure(format!("Workflow execution failed: {}", e)),
        }
    }

    fn category(&self) -> ToolCategory {
        ToolCategory::Agent
    }
}

/// Tool to get workflow status
pub struct GetWorkflowStatusTool {
    workflow_engine: Arc<WorkflowEngine>,
}

impl GetWorkflowStatusTool {
    pub fn new(workflow_engine: Arc<WorkflowEngine>) -> Self {
        Self { workflow_engine }
    }
}

#[async_trait]
impl SimpleTool for GetWorkflowStatusTool {
    fn name(&self) -> &str {
        "get_workflow_status"
    }

    fn description(&self) -> &str {
        "Get the current status of a workflow execution"
    }

    fn parameters_schema(&self) -> serde_json::Value {
        json!({
            "type": "object",
            "properties": {
                "workflow_id": {
                    "type": "string",
                    "description": "Workflow identifier"
                }
            },
            "required": ["workflow_id"]
        })
    }

    async fn execute(&self, input: ToolInput) -> ToolResult {
        let workflow_id = match input.get_str("workflow_id") {
            Some(id) => id,
            None => return ToolResult::failure("Missing 'workflow_id' parameter"),
        };

        match self.workflow_engine.get_workflow(workflow_id).await {
            Some(workflow) => ToolResult::success_text(
                serde_json::to_string(&json!({
                    "workflow_id": workflow.id,
                    "workflow_name": workflow.name,
                    "status": format!("{:?}", workflow.status),
                    "current_step": workflow.current_step,
                    "total_steps": workflow.steps.len(),
                    "started_at": workflow.started_at.map(|d| d.to_rfc3339()),
                    "completed_at": workflow.completed_at.map(|d| d.to_rfc3339())
                }))
                .unwrap_or_else(|_| format!("Workflow: {}", workflow.name)),
            ),
            None => ToolResult::failure(format!("Workflow '{}' not found", workflow_id)),
        }
    }

    fn category(&self) -> ToolCategory {
        ToolCategory::Agent
    }
}

/// Tool to list available workflows
pub struct ListWorkflowsTool {
    workflow_engine: Arc<WorkflowEngine>,
}

impl ListWorkflowsTool {
    pub fn new(workflow_engine: Arc<WorkflowEngine>) -> Self {
        Self { workflow_engine }
    }
}

#[async_trait]
impl SimpleTool for ListWorkflowsTool {
    fn name(&self) -> &str {
        "list_workflows"
    }

    fn description(&self) -> &str {
        "List all available workflow definitions"
    }

    fn parameters_schema(&self) -> serde_json::Value {
        json!({
            "type": "object",
            "properties": {},
            "required": []
        })
    }

    async fn execute(&self, _input: ToolInput) -> ToolResult {
        let workflows = self.workflow_engine.list_workflows().await;
        ToolResult::success_text(
            serde_json::to_string(&json!({
                "workflows": workflows,
                "available_workflows": ["code_review", "design"]
            }))
            .unwrap_or_else(|_| "Workflows listed".to_string()),
        )
    }

    fn category(&self) -> ToolCategory {
        ToolCategory::Agent
    }
}
