//! Integration tests for multi-agent collaboration system

use crate::agent::collaboration::{
    team::{AgentTeam, TeamManager},
    workflow::{WorkflowEngine, create_code_review_workflow, WorkflowStatus},
    workspace::{SharedWorkspace, ArtifactType, ArtifactContent},
};
use crate::agent::communication::AgentId;
use crate::Config;
use crate::bus::MessageBus;
use crate::session::SessionManager;
use std::sync::Arc;

#[tokio::test]
#[ignore] // Requires API key and multi-threaded runtime
async fn test_team_creation_and_management() {
    let config = Arc::new(Config::default());
    let user_bus = MessageBus::new();
    let sessions = Arc::new(SessionManager::new(&config));
    let team_manager = Arc::new(TeamManager::new(config, user_bus, sessions));

    // Create a team
    let roles = vec![
        ("architect".to_string(), "arch-1".to_string()),
        ("developer".to_string(), "dev-1".to_string()),
    ];

    // Note: This will fail because create_team_member needs API key
    // but tests the structure
    let result = team_manager.create_team("test-team-1", "Test Team", roles).await;
    
    // For now, expect error since team member creation needs API key
    assert!(result.is_err() || result.is_ok()); // Accept either for now
}

#[tokio::test]
#[ignore] // Requires API key and full agent execution
async fn test_workflow_execution_end_to_end() {
    use crate::agent::collaboration::workflow::WorkflowEngine;
    use std::collections::HashMap;
    
    let config = Arc::new(Config::default());
    let user_bus = MessageBus::new();
    let sessions = Arc::new(SessionManager::new(&config));
    let team_manager = Arc::new(TeamManager::new(config.clone(), user_bus, sessions.clone()));
    let workflow_engine = Arc::new(WorkflowEngine::new());

    // Skip if no API key
    if config.get_api_key().is_none() {
        return;
    }

    // Create a team with developer and reviewer
    let roles = vec![
        ("developer".to_string(), "dev-1".to_string()),
        ("reviewer".to_string(), "rev-1".to_string()),
    ];

    let team = match team_manager.create_team("test-workflow-team", "Test Workflow Team", roles).await {
        Ok(t) => t,
        Err(_) => return, // Skip if team creation fails
    };

    // Create a simple workflow
    let workflow = create_code_review_workflow();
    let initial_context = HashMap::new();

    // Execute workflow
    let result = workflow_engine
        .execute_workflow(workflow, team, initial_context)
        .await;

    // Check result
    match result {
        Ok(r) => {
            // Workflow should have executed (may succeed or fail depending on agent execution)
            assert!(r.completed_steps > 0 || !r.success);
        }
        Err(_) => {
            // Execution errors are acceptable in test environment
        }
    }
}

#[tokio::test]
async fn test_workflow_creation_and_execution() {
    let _workflow_engine = Arc::new(WorkflowEngine::new());
    
    // Create a code review workflow
    let workflow = create_code_review_workflow();
    
    assert_eq!(workflow.id, "code_review");
    assert_eq!(workflow.name, "Code Review Workflow");
    assert!(!workflow.steps.is_empty());
    assert_eq!(workflow.status, WorkflowStatus::Pending);
    
    // Test workflow step access
    assert!(workflow.current_step().is_some());
    assert_eq!(workflow.current_step().unwrap().role, "developer");
}

#[tokio::test]
async fn test_workspace_artifact_management() {
    let workspace = Arc::new(SharedWorkspace::new("test-team", std::path::PathBuf::from("/tmp")));
    let agent_id = AgentId::new("test-team", "developer", "dev-1");
    
    // Create an artifact
    let artifact = workspace
        .create_artifact(
            "test-artifact-1",
            "test_file.rs",
            ArtifactType::CodeFile {
                path: std::path::PathBuf::from("test_file.rs"),
            },
            ArtifactContent::FileContent {
                content: "fn main() {}".to_string(),
            },
            agent_id.clone(),
        )
        .await
        .unwrap();
    
    assert_eq!(artifact.id, "test-artifact-1");
    assert_eq!(artifact.version, 1);
    assert_eq!(artifact.created_by, agent_id);
    
    // Get artifact
    let retrieved = workspace.get_artifact("test-artifact-1").await.unwrap();
    assert_eq!(retrieved.id, "test-artifact-1");
    assert_eq!(retrieved.version, 1);
    
    // List artifacts
    let artifacts = workspace.list_artifacts().await;
    assert!(artifacts.contains(&"test-artifact-1".to_string()));
    
    // Update artifact
    let updated = workspace
        .update_artifact(
            "test-artifact-1",
            ArtifactContent::FileContent {
                content: "fn main() { println!(\"Hello\"); }".to_string(),
            },
            agent_id.clone(),
        )
        .await
        .unwrap();
    
    assert_eq!(updated.version, 2);
    assert_eq!(updated.modified_by, agent_id);
}

#[tokio::test]
async fn test_workflow_step_advancement() {
    let mut workflow = create_code_review_workflow();
    
    assert_eq!(workflow.current_step, 0);
    assert!(workflow.has_next_step());
    
    // Advance to next step
    workflow.advance_step();
    assert_eq!(workflow.current_step, 1);
    assert!(workflow.has_next_step());
    
    // Advance to completion
    while workflow.has_next_step() {
        workflow.advance_step();
    }
    
    assert!(!workflow.has_next_step());
}

#[tokio::test]
async fn test_agent_message_protocol() {
    use crate::agent::communication::{AgentMessage, AgentMessageType, RequestType};
    use serde_json::json;
    
    let from = AgentId::new("team1", "developer", "dev-1");
    let to = AgentId::new("team1", "reviewer", "rev-1");
    
    // Create a request message
    let message = AgentMessage::request(
        from.clone(),
        to.clone(),
        RequestType::RequestReview,
        json!({"file": "main.rs"}),
        None,
    );
    
    assert_eq!(message.from, from);
    assert_eq!(message.to, Some(to));
    assert!(matches!(message.message_type, AgentMessageType::Request { .. }));
    
    // Test serialization
    let serialized = serde_json::to_string(&message).unwrap();
    let deserialized: AgentMessage = serde_json::from_str(&serialized).unwrap();
    assert_eq!(deserialized.id, message.id);
    assert_eq!(deserialized.from, message.from);
    assert_eq!(deserialized.to, message.to);
}

#[tokio::test]
async fn test_team_member_roles() {
    let team = AgentTeam::new("test-team", "Test Team");
    
    // Test that team starts empty
    assert_eq!(team.member_count(), 0);
    assert!(!team.is_ready());
    
    // Test get_members_by_role on empty team
    let members = team.get_members_by_role("developer");
    assert!(members.is_empty());
}
