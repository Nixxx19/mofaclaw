//! Agent communication tools for multi-agent collaboration

use super::base::{SimpleTool, ToolInput, ToolResult};
use crate::agent::collaboration::team::TeamManager;
use crate::agent::communication::protocol::AgentMessageType;
use crate::agent::communication::{AgentId, AgentMessage, RequestType};
use async_trait::async_trait;
use mofa_sdk::agent::ToolCategory;
use serde_json::json;
use std::sync::Arc;
use std::time::Duration;

/// Tool to send a message to another agent
pub struct SendAgentMessageTool {
    team_manager: Arc<TeamManager>,
}

impl SendAgentMessageTool {
    pub fn new(team_manager: Arc<TeamManager>) -> Self {
        Self { team_manager }
    }
}

#[async_trait]
impl SimpleTool for SendAgentMessageTool {
    fn name(&self) -> &str {
        "send_agent_message"
    }

    fn description(&self) -> &str {
        "Send a structured message to another agent in a team. Specify the target agent ID, message type, and payload."
    }

    fn parameters_schema(&self) -> serde_json::Value {
        json!({
            "type": "object",
            "properties": {
                "team_id": {
                    "type": "string",
                    "description": "The ID of the team"
                },
                "from_agent_id": {
                    "type": "string",
                    "description": "Agent ID of the sender (format: team_id:role:instance_id)"
                },
                "to_agent_id": {
                    "type": "string",
                    "description": "Agent ID of the recipient (format: team_id:role:instance_id)"
                },
                "request_type": {
                    "type": "string",
                    "description": "Type of request (ask_question, request_review, request_implementation, request_testing, share_finding, request_approval, request_information)",
                    "enum": ["ask_question", "request_review", "request_implementation", "request_testing", "share_finding", "request_approval", "request_information"]
                },
                "payload": {
                    "type": "object",
                    "description": "Message payload as JSON object",
                    "additionalProperties": true
                },
                "context_id": {
                    "type": "string",
                    "description": "Optional context ID for linking to workflow step or shared context"
                }
            },
            "required": ["team_id", "from_agent_id", "to_agent_id", "request_type", "payload"]
        })
    }

    async fn execute(&self, input: ToolInput) -> ToolResult {
        let team_id = match input.get_str("team_id") {
            Some(id) => id,
            None => return ToolResult::failure("Missing 'team_id' parameter"),
        };

        let from_agent_id_str = match input.get_str("from_agent_id") {
            Some(id) => id,
            None => return ToolResult::failure("Missing 'from_agent_id' parameter"),
        };

        let to_agent_id_str = match input.get_str("to_agent_id") {
            Some(id) => id,
            None => return ToolResult::failure("Missing 'to_agent_id' parameter"),
        };

        let request_type_str = match input.get_str("request_type") {
            Some(t) => t,
            None => return ToolResult::failure("Missing 'request_type' parameter"),
        };

        let payload: serde_json::Value = match input.get::<serde_json::Value>("payload") {
            Some(p) => p.clone(),
            None => return ToolResult::failure("Missing 'payload' parameter"),
        };

        let context_id = input.get_str("context_id").map(|s| s.to_string());

        // Parse agent IDs
        let from_agent_id = match AgentId::from_str(from_agent_id_str) {
            Some(id) => id,
            None => {
                return ToolResult::failure(format!(
                    "Invalid from_agent_id format: {}",
                    from_agent_id_str
                ));
            }
        };
        let to_agent_id = match AgentId::from_str(to_agent_id_str) {
            Some(id) => id,
            None => {
                return ToolResult::failure(format!(
                    "Invalid to_agent_id format: {}",
                    to_agent_id_str
                ));
            }
        };

        // Parse request type
        let request_type = match request_type_str {
            "ask_question" => RequestType::AskQuestion,
            "request_review" => RequestType::RequestReview,
            "request_implementation" => RequestType::RequestImplementation,
            "request_testing" => RequestType::RequestTesting,
            "share_finding" => RequestType::ShareFinding,
            "request_approval" => RequestType::RequestApproval,
            "request_information" => RequestType::RequestInformation,
            _ => return ToolResult::failure(format!("Invalid request_type: {}", request_type_str)),
        };

        // Get team
        let team = match self.team_manager.get_team(team_id).await {
            Some(t) => t,
            None => return ToolResult::failure(format!("Team '{}' not found", team_id)),
        };

        // Create message
        let message = AgentMessage::request(
            from_agent_id,
            to_agent_id.clone(),
            request_type,
            payload,
            context_id,
        );

        // Send message via team's message bus
        match team.broadcast(message.clone()).await {
            Ok(_) => {
                let result_json = json!({
                    "message_id": message.id,
                    "from": from_agent_id_str,
                    "to": to_agent_id_str,
                    "status": "sent",
                    "timestamp": message.timestamp.to_rfc3339(),
                });
                ToolResult::success_text(
                    serde_json::to_string(&result_json)
                        .unwrap_or_else(|_| format!("Message sent to agent {}", to_agent_id_str)),
                )
            }
            Err(e) => ToolResult::failure(format!("Failed to send message: {}", e)),
        }
    }

    fn category(&self) -> ToolCategory {
        ToolCategory::Agent
    }
}

/// Tool to broadcast a message to all team members
pub struct BroadcastToTeamTool {
    team_manager: Arc<TeamManager>,
}

impl BroadcastToTeamTool {
    pub fn new(team_manager: Arc<TeamManager>) -> Self {
        Self { team_manager }
    }
}

#[async_trait]
impl SimpleTool for BroadcastToTeamTool {
    fn name(&self) -> &str {
        "broadcast_to_team"
    }

    fn description(&self) -> &str {
        "Broadcast a message to all members of a team on a specific topic."
    }

    fn parameters_schema(&self) -> serde_json::Value {
        json!({
            "type": "object",
            "properties": {
                "team_id": {
                    "type": "string",
                    "description": "The ID of the team"
                },
                "from_agent_id": {
                    "type": "string",
                    "description": "Agent ID of the sender (format: team_id:role:instance_id)"
                },
                "topic": {
                    "type": "string",
                    "description": "Topic for the broadcast (e.g., 'status', 'artifact_update', 'new_task')"
                },
                "payload": {
                    "type": "object",
                    "description": "Message payload as JSON object",
                    "additionalProperties": true
                }
            },
            "required": ["team_id", "from_agent_id", "topic", "payload"]
        })
    }

    async fn execute(&self, input: ToolInput) -> ToolResult {
        let team_id = match input.get_str("team_id") {
            Some(id) => id,
            None => return ToolResult::failure("Missing 'team_id' parameter"),
        };

        let from_agent_id_str = match input.get_str("from_agent_id") {
            Some(id) => id,
            None => return ToolResult::failure("Missing 'from_agent_id' parameter"),
        };

        let topic = match input.get_str("topic") {
            Some(t) => t,
            None => return ToolResult::failure("Missing 'topic' parameter"),
        };

        let payload: serde_json::Value = match input.get::<serde_json::Value>("payload") {
            Some(p) => p.clone(),
            None => return ToolResult::failure("Missing 'payload' parameter"),
        };

        // Parse agent ID
        let from_agent_id = match AgentId::from_str(from_agent_id_str) {
            Some(id) => id,
            None => {
                return ToolResult::failure(format!(
                    "Invalid from_agent_id format: {}",
                    from_agent_id_str
                ));
            }
        };

        // Get team
        let team = match self.team_manager.get_team(team_id).await {
            Some(t) => t,
            None => return ToolResult::failure(format!("Team '{}' not found", team_id)),
        };

        // Create broadcast message
        let message = AgentMessage::broadcast(from_agent_id, topic, payload);

        // Broadcast message
        match team.broadcast(message.clone()).await {
            Ok(_) => {
                let result_json = json!({
                    "message_id": message.id,
                    "from": from_agent_id_str,
                    "topic": topic,
                    "recipient_count": team.member_count(),
                    "status": "broadcast",
                    "timestamp": message.timestamp.to_rfc3339(),
                });
                ToolResult::success_text(serde_json::to_string(&result_json).unwrap_or_else(|_| {
                    format!("Message broadcast to {} team members", team.member_count())
                }))
            }
            Err(e) => ToolResult::failure(format!("Failed to broadcast message: {}", e)),
        }
    }

    fn category(&self) -> ToolCategory {
        ToolCategory::Agent
    }
}

/// Tool to respond to an approval request
pub struct RespondToApprovalTool {
    team_manager: Arc<TeamManager>,
}

impl RespondToApprovalTool {
    pub fn new(team_manager: Arc<TeamManager>) -> Self {
        Self { team_manager }
    }
}

#[async_trait]
impl SimpleTool for RespondToApprovalTool {
    fn name(&self) -> &str {
        "respond_to_approval"
    }

    fn description(&self) -> &str {
        "Respond to an approval request from a workflow. Requires correlation_id (from the approval request), approval status (approved/rejected), and optional comment."
    }

    fn parameters_schema(&self) -> serde_json::Value {
        json!({
            "type": "object",
            "properties": {
                "team_id": {
                    "type": "string",
                    "description": "The ID of the team"
                },
                "from_agent_id": {
                    "type": "string",
                    "description": "Agent ID of the responder (format: team_id:role:instance_id)"
                },
                "correlation_id": {
                    "type": "string",
                    "description": "Correlation ID from the approval request message"
                },
                "approved": {
                    "type": "boolean",
                    "description": "Whether to approve (true) or reject (false)"
                },
                "comment": {
                    "type": "string",
                    "description": "Optional comment explaining the decision"
                }
            },
            "required": ["team_id", "from_agent_id", "correlation_id", "approved"]
        })
    }

    async fn execute(&self, input: ToolInput) -> ToolResult {
        let team_id = match input.get_str("team_id") {
            Some(id) => id,
            None => return ToolResult::failure("Missing 'team_id' parameter"),
        };
        let from_agent_id_str = match input.get_str("from_agent_id") {
            Some(id) => id,
            None => return ToolResult::failure("Missing 'from_agent_id' parameter"),
        };
        let correlation_id = match input.get_str("correlation_id") {
            Some(id) => id,
            None => return ToolResult::failure("Missing 'correlation_id' parameter"),
        };
        let approved = match input.get::<serde_json::Value>("approved") {
            Some(v) => match v.as_bool() {
                Some(b) => b,
                None => {
                    return ToolResult::failure("Invalid 'approved' parameter: expected a boolean");
                }
            },
            None => return ToolResult::failure("Missing 'approved' parameter"),
        };
        let comment = input.get_str("comment").map(|s| s.to_string());

        // Parse agent ID
        let from_agent_id = match AgentId::from_str(from_agent_id_str) {
            Some(id) => id,
            None => {
                return ToolResult::failure(format!(
                    "Invalid from_agent_id format: {}",
                    from_agent_id_str
                ));
            }
        };

        // Get team
        let team = match self.team_manager.get_team(team_id).await {
            Some(t) => t,
            None => return ToolResult::failure(format!("Team '{}' not found", team_id)),
        };

        // Find the workflow engine or approver (for now, send to workflow engine)
        let to_agent_id = AgentId::new(team_id, "workflow", "engine");

        // Create response message
        let payload = json!({
            "approved": approved,
            "comment": comment,
            "context_id": correlation_id
        });

        let response = AgentMessage::response(
            from_agent_id.clone(),
            to_agent_id,
            correlation_id.to_string(),
            approved,
            payload,
        );

        // Send response via team's message bus
        match team.message_bus.publish(response.clone()).await {
            Ok(_) => {
                let result_json = json!({
                    "message_id": response.id,
                    "from": from_agent_id_str,
                    "correlation_id": correlation_id,
                    "approved": approved,
                    "status": "sent",
                    "timestamp": response.timestamp.to_rfc3339(),
                });
                ToolResult::success_text(serde_json::to_string(&result_json).unwrap_or_else(|_| {
                    format!(
                        "Approval response sent: {}",
                        if approved { "approved" } else { "rejected" }
                    )
                }))
            }
            Err(e) => ToolResult::failure(format!("Failed to send approval response: {}", e)),
        }
    }

    fn category(&self) -> ToolCategory {
        ToolCategory::Agent
    }
}

/// Tool that blocks (async) until a `Response` message matching the given
/// `correlation_id` arrives on the team's message bus, then returns its payload
/// to the LLM — enabling mid-step peer-to-peer consultation between agents.
///
/// Typical usage pattern inside a workflow step:
/// 1. Agent calls `send_agent_message` with a `correlation_id` and a question.
/// 2. Agent calls `wait_for_agent_response` with the same `correlation_id`.
/// 3. The target agent's `AgentEventLoop` handles the `Request`, calls
///    `process_direct`, and publishes a `Response` with the matching id.
/// 4. This tool returns the response payload; the LLM continues reasoning.
///
/// A `timeout_secs` parameter (default 60) prevents permanent deadlock.
pub struct WaitForAgentResponseTool {
    team_manager: Arc<TeamManager>,
}

impl WaitForAgentResponseTool {
    pub fn new(team_manager: Arc<TeamManager>) -> Self {
        Self { team_manager }
    }
}

#[async_trait]
impl SimpleTool for WaitForAgentResponseTool {
    fn name(&self) -> &str {
        "wait_for_agent_response"
    }

    fn description(&self) -> &str {
        "Wait for a response from another agent. Use after send_agent_message — \
         pass the same correlation_id used when sending. Blocks until the response \
         arrives or the timeout expires. Returns the response payload."
    }

    fn parameters_schema(&self) -> serde_json::Value {
        json!({
            "type": "object",
            "properties": {
                "team_id": {
                    "type": "string",
                    "description": "The ID of the team"
                },
                "correlation_id": {
                    "type": "string",
                    "description": "The correlation_id used in the send_agent_message call you are waiting for"
                },
                "timeout_secs": {
                    "type": "integer",
                    "description": "Maximum seconds to wait (default 60). Prevents deadlock if the other agent is busy or unreachable.",
                    "default": 60
                }
            },
            "required": ["team_id", "correlation_id"]
        })
    }

    async fn execute(&self, input: ToolInput) -> ToolResult {
        let team_id = match input.get_str("team_id") {
            Some(id) => id,
            None => return ToolResult::failure("Missing 'team_id' parameter"),
        };
        let correlation_id = match input.get_str("correlation_id") {
            Some(id) => id.to_string(),
            None => return ToolResult::failure("Missing 'correlation_id' parameter"),
        };
        let timeout_secs = input
            .get::<serde_json::Value>("timeout_secs")
            .and_then(|v| v.as_u64())
            .unwrap_or(60);

        // Get the team's bus
        let team = match self.team_manager.get_team(team_id).await {
            Some(t) => t,
            None => return ToolResult::failure(format!("Team '{}' not found", team_id)),
        };

        // Create an independent receiver — does not interfere with the AgentEventLoop's
        // own receiver since AgentMessageBus is a broadcast channel.
        let mut rx = team.message_bus.subscribe_all();
        let cid = correlation_id.clone();

        let wait_result = tokio::time::timeout(Duration::from_secs(timeout_secs), async move {
            loop {
                match rx.recv().await {
                    Ok(msg) => {
                        // Match on Response messages with the right correlation_id
                        if msg.correlation_id.as_deref() == Some(&cid) {
                            if let AgentMessageType::Response {
                                success, payload, ..
                            } = msg.message_type
                            {
                                return Ok(json!({
                                    "success": success,
                                    "from": msg.from.to_string(),
                                    "correlation_id": cid,
                                    "response": payload,
                                }));
                            }
                        }
                    }
                    Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => continue,
                    Err(tokio::sync::broadcast::error::RecvError::Closed) => {
                        return Err("message bus closed");
                    }
                }
            }
        })
        .await;

        match wait_result {
            Ok(Ok(payload)) => ToolResult::success_text(
                serde_json::to_string(&payload).unwrap_or_else(|_| payload.to_string()),
            ),
            Ok(Err(e)) => ToolResult::failure(format!("Bus error while waiting: {}", e)),
            Err(_) => ToolResult::failure(format!(
                "Timed out after {}s waiting for response to correlation_id '{}'. \
                 The other agent may be busy or unreachable — continue without the response.",
                timeout_secs, correlation_id
            )),
        }
    }

    fn category(&self) -> ToolCategory {
        ToolCategory::Agent
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Config;
    use crate::agent::collaboration::team::{AgentTeam, TeamManager};
    use crate::agent::communication::protocol::AgentId;
    use crate::bus::MessageBus;
    use crate::session::SessionManager;
    use tokio::runtime::Runtime;

    // -----------------------------------------------------------------------
    // WaitForAgentResponseTool tests
    // -----------------------------------------------------------------------

    #[tokio::test(flavor = "multi_thread")]
    async fn test_wait_for_agent_response_team_not_found() {
        let config = Arc::new(Config::default());
        let team_manager = Arc::new(TeamManager::new(config, MessageBus::new(), Arc::new(SessionManager::new(&Config::default()))));
        let tool = WaitForAgentResponseTool::new(team_manager);

        let input = ToolInput::from_json(json!({
            "team_id": "ghost-team",
            "correlation_id": "abc"
        }));
        let result = tool.execute(input).await;
        assert!(!result.success);
        assert!(result.error.as_deref().unwrap_or("").contains("not found"));
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn test_wait_for_agent_response_timeout() {
        let config = Arc::new(Config::default());
        let team_manager = Arc::new(TeamManager::new(config, MessageBus::new(), Arc::new(SessionManager::new(&Config::default()))));

        // Inject a real team — no API key needed since we use insert_test_team
        let team = AgentTeam::new("t1", "Team 1");
        team_manager.insert_test_team(team).await;

        let tool = WaitForAgentResponseTool::new(team_manager);
        let input = ToolInput::from_json(json!({
            "team_id": "t1",
            "correlation_id": "will-never-arrive",
            "timeout_secs": 1
        }));

        let result = tool.execute(input).await;
        assert!(!result.success);
        let text = result.error.as_deref().unwrap_or("").to_lowercase();
        assert!(text.contains("timed out") || text.contains("timeout"));
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn test_wait_for_agent_response_receives_matching_response() {
        let config = Arc::new(Config::default());
        let team_manager = Arc::new(TeamManager::new(config, MessageBus::new(), Arc::new(SessionManager::new(&Config::default()))));

        let team = AgentTeam::new("t2", "Team 2");
        let bus = team.message_bus.clone();
        team_manager.insert_test_team(team).await;

        // Publish a matching Response after a short delay so the tool's
        // subscriber is set up before the message arrives.
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(50)).await;
            let from = AgentId::new("t2", "reviewer", "rev-1");
            let to = AgentId::new("t2", "developer", "dev-1");
            let msg = crate::agent::communication::AgentMessage::response(
                from,
                to,
                "corr-42".to_string(),
                true,
                json!({"answer": "looks correct"}),
            );
            bus.publish(msg).await.unwrap();
        });

        let tool = WaitForAgentResponseTool::new(team_manager);
        let input = ToolInput::from_json(json!({
            "team_id": "t2",
            "correlation_id": "corr-42",
            "timeout_secs": 5
        }));

        let result = tool.execute(input).await;
        assert!(result.success, "expected success, got: {:?}", result.as_text());
        let text = result.as_text().unwrap();
        assert!(text.contains("corr-42"));
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn test_wait_for_agent_response_ignores_wrong_correlation_id() {
        let config = Arc::new(Config::default());
        let team_manager = Arc::new(TeamManager::new(config, MessageBus::new(), Arc::new(SessionManager::new(&Config::default()))));

        let team = AgentTeam::new("t3", "Team 3");
        let bus = team.message_bus.clone();
        team_manager.insert_test_team(team).await;

        let bus2 = bus.clone();
        tokio::spawn(async move {
            // Wrong correlation_id first — tool must skip it
            tokio::time::sleep(Duration::from_millis(30)).await;
            let from = AgentId::new("t3", "reviewer", "rev-1");
            let to = AgentId::new("t3", "developer", "dev-1");
            let wrong = crate::agent::communication::AgentMessage::response(
                from.clone(), to.clone(),
                "wrong-corr".to_string(), true,
                json!({"answer": "wrong"}),
            );
            bus.publish(wrong).await.unwrap();

            // Correct correlation_id second
            tokio::time::sleep(Duration::from_millis(30)).await;
            let right = crate::agent::communication::AgentMessage::response(
                from, to,
                "right-corr".to_string(), true,
                json!({"answer": "correct"}),
            );
            bus2.publish(right).await.unwrap();
        });

        let tool = WaitForAgentResponseTool::new(team_manager);
        let input = ToolInput::from_json(json!({
            "team_id": "t3",
            "correlation_id": "right-corr",
            "timeout_secs": 5
        }));

        let result = tool.execute(input).await;
        assert!(result.success);
        let text = result.as_text().unwrap();
        assert!(text.contains("right-corr"));
    }

    #[tokio::test(flavor = "multi_thread")]
    #[ignore] // TODO: Fix SessionManager blocking issue
    async fn test_send_agent_message_tool() {
        let rt = Runtime::new().unwrap();
        rt.block_on(async {
            let config = Arc::new(Config::default());
            let user_bus = MessageBus::new();
            let sessions = Arc::new(SessionManager::new(&config));
            let team_manager = Arc::new(TeamManager::new(config, user_bus, sessions));
            let tool = SendAgentMessageTool::new(team_manager.clone());

            // This test will fail because team doesn't exist, but tests the tool structure
            let input = ToolInput::from_json(json!({
                "team_id": "test-team",
                "from_agent_id": "test-team:developer:dev-1",
                "to_agent_id": "test-team:reviewer:rev-1",
                "request_type": "ask_question",
                "payload": {"question": "Can you review this?"}
            }));

            let result = tool.execute(input).await;
            assert!(!result.success); // Should fail because team doesn't exist
            assert!(
                result
                    .as_text()
                    .unwrap()
                    .contains("Team 'test-team' not found")
            );
        });
    }

    #[tokio::test(flavor = "multi_thread")]
    #[ignore] // TODO: Fix SessionManager blocking issue
    async fn test_broadcast_to_team_tool() {
        let rt = Runtime::new().unwrap();
        rt.block_on(async {
            let config = Arc::new(Config::default());
            let user_bus = MessageBus::new();
            let sessions = Arc::new(SessionManager::new(&config));
            let team_manager = Arc::new(TeamManager::new(config, user_bus, sessions));
            let tool = BroadcastToTeamTool::new(team_manager.clone());

            let input = ToolInput::from_json(json!({
                "team_id": "test-team",
                "from_agent_id": "test-team:developer:dev-1",
                "topic": "status",
                "payload": {"status": "working"}
            }));

            let result = tool.execute(input).await;
            assert!(!result.success); // Should fail because team doesn't exist
            assert!(
                result
                    .as_text()
                    .unwrap()
                    .contains("Team 'test-team' not found")
            );
        });
    }
}
