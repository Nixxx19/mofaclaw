//! Agent event loop for autonomous, event-driven agent behaviour.
//!
//! `AgentEventLoop` wraps an existing `AgentLoop` and runs a persistent async
//! loop that subscribes to an `AgentMessageBus`.  Instead of being called
//! once per workflow step, an event-loop agent stays alive and reacts to
//! whatever the bus delivers:
//!
//! | Event | Default reaction |
//! |-------|-----------------|
//! | `WorkflowStepAssigned` | Execute the step and reply with `StepCompleted` |
//! | `ArtifactUpdate` | Log; reviewers can override to trigger a review |
//! | `Request` | Forward to `process_direct` and reply with `Response` |
//! | Other | Ignored (log at trace) |
//!
//! ## Usage
//!
//! ```rust,ignore
//! let event_loop = AgentEventLoop::new(agent_id, agent_loop, bus, workspace);
//! let (shutdown_tx, shutdown_rx) = tokio::sync::watch::channel(false);
//! tokio::spawn(async move { event_loop.run(shutdown_rx).await });
//! // … later:
//! let _ = shutdown_tx.send(true);
//! ```

use crate::agent::collaboration::workspace::SharedWorkspace;
use crate::agent::communication::AgentMessageBus;
use crate::agent::communication::protocol::{AgentId, AgentMessage, AgentMessageType, RequestType};
use crate::agent::loop_::AgentLoop;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use tokio::sync::{RwLock, broadcast, watch};
use tracing::{debug, info, warn};

fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

// ---------------------------------------------------------------------------
// Health tracking
// ---------------------------------------------------------------------------

/// Shared health counters updated atomically inside `run`.
///
/// Wrap in `Arc` and share between the spawned task and `TeamManager` so
/// the manager can read loop health without inter-task messaging.
#[derive(Debug, Default)]
pub struct LoopHealthCounters {
    /// Total events successfully processed (messages where `should_receive` was true).
    pub events_processed: AtomicU64,
    /// Count of non-fatal errors (e.g. lagged messages).
    pub errors: AtomicU64,
    /// Unix timestamp (seconds) of the last loop iteration.  0 = not started.
    pub last_heartbeat_secs: AtomicU64,
}

impl LoopHealthCounters {
    pub fn new() -> Arc<Self> {
        Arc::new(Self::default())
    }

    /// Take an immutable point-in-time snapshot (no lock required).
    pub fn snapshot(&self) -> LoopHealthSnapshot {
        LoopHealthSnapshot {
            events_processed: self.events_processed.load(Ordering::Relaxed),
            errors: self.errors.load(Ordering::Relaxed),
            last_heartbeat_secs: self.last_heartbeat_secs.load(Ordering::Relaxed),
        }
    }
}

/// Point-in-time copy of `LoopHealthCounters` values.
#[derive(Debug, Clone)]
pub struct LoopHealthSnapshot {
    pub events_processed: u64,
    pub errors: u64,
    /// Unix timestamp of last heartbeat; 0 means the loop has not started yet.
    pub last_heartbeat_secs: u64,
}

// ---------------------------------------------------------------------------
// State
// ---------------------------------------------------------------------------

/// Lifecycle state of an `AgentEventLoop`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EventLoopState {
    /// Created but not yet started.
    Idle,
    /// Running and processing events.
    Running,
    /// Received shutdown signal; cleaning up.
    ShuttingDown,
    /// Loop has exited.
    Stopped,
}

// ---------------------------------------------------------------------------
// AgentEventLoop
// ---------------------------------------------------------------------------

/// A persistent event-driven wrapper around `AgentLoop`.
///
/// Spawn with `tokio::spawn(event_loop.run(shutdown_rx))`.
pub struct AgentEventLoop {
    /// Identity of this agent on the bus.
    pub agent_id: AgentId,
    /// The underlying agent used to process prompts.
    agent_loop: Arc<AgentLoop>,
    /// Bus to subscribe to and publish on.
    event_bus: Arc<AgentMessageBus>,
    /// Shared workspace for artifact access.
    workspace: Arc<SharedWorkspace>,
    /// Current lifecycle state.
    state: Arc<RwLock<EventLoopState>>,
    /// Shared health counters — also held by `TeamManager` for monitoring.
    health: Arc<LoopHealthCounters>,
    /// Maximum time allowed for a single `process_direct` call.
    /// Prevents a hung LLM call from blocking the loop indefinitely.
    step_timeout: Duration,
}

impl AgentEventLoop {
    /// Create a new event loop with fresh health counters (does not start it — call `run`).
    pub fn new(
        agent_id: AgentId,
        agent_loop: Arc<AgentLoop>,
        event_bus: Arc<AgentMessageBus>,
        workspace: Arc<SharedWorkspace>,
    ) -> Self {
        Self::new_with_health(agent_id, agent_loop, event_bus, workspace, LoopHealthCounters::new())
    }

    /// Create a new event loop sharing pre-existing health counters.
    ///
    /// Use this when you need to retain the `Arc<LoopHealthCounters>` handle
    /// (e.g. inside `TeamManager` for monitoring and auto-restart).
    pub fn new_with_health(
        agent_id: AgentId,
        agent_loop: Arc<AgentLoop>,
        event_bus: Arc<AgentMessageBus>,
        workspace: Arc<SharedWorkspace>,
        health: Arc<LoopHealthCounters>,
    ) -> Self {
        Self {
            agent_id,
            agent_loop,
            event_bus,
            workspace,
            state: Arc::new(RwLock::new(EventLoopState::Idle)),
            health,
            step_timeout: Duration::from_secs(300), // 5 minutes default
        }
    }

    /// Override the per-step timeout (default: 5 minutes).
    pub fn with_step_timeout(mut self, timeout: Duration) -> Self {
        self.step_timeout = timeout;
        self
    }

    /// Current lifecycle state.
    pub async fn state(&self) -> EventLoopState {
        self.state.read().await.clone()
    }

    /// Access the shared health counters (for monitoring by `TeamManager`).
    pub fn health_counters(&self) -> Arc<LoopHealthCounters> {
        self.health.clone()
    }

    // ------------------------------------------------------------------
    // Main loop
    // ------------------------------------------------------------------

    /// Run until `shutdown` receives `true`.
    ///
    /// Designed to be spawned with `tokio::spawn`.
    pub async fn run(&self, mut shutdown: watch::Receiver<bool>) {
        *self.state.write().await = EventLoopState::Running;
        info!("AgentEventLoop started for {}", self.agent_id);

        let mut receiver = self.event_bus.subscribe_agent(&self.agent_id);

        loop {
            // Heartbeat: record that the loop is alive.
            self.health
                .last_heartbeat_secs
                .store(now_secs(), Ordering::Relaxed);

            tokio::select! {
                // Shutdown signal
                result = shutdown.changed() => {
                    if result.is_err() || *shutdown.borrow() {
                        break;
                    }
                }

                // Incoming event
                msg_result = receiver.recv() => {
                    match msg_result {
                        Ok(msg) => {
                            if self.event_bus.should_receive(&msg, &self.agent_id) {
                                self.handle_event(msg).await;
                                self.health.events_processed.fetch_add(1, Ordering::Relaxed);
                            }
                        }
                        Err(broadcast::error::RecvError::Lagged(n)) => {
                            self.health.errors.fetch_add(1, Ordering::Relaxed);
                            warn!(
                                "AgentEventLoop {} lagged by {} messages",
                                self.agent_id, n
                            );
                        }
                        Err(broadcast::error::RecvError::Closed) => {
                            info!("AgentEventLoop {}: bus closed, stopping", self.agent_id);
                            break;
                        }
                    }
                }
            }
        }

        *self.state.write().await = EventLoopState::Stopped;
        info!("AgentEventLoop stopped for {}", self.agent_id);
    }

    // ------------------------------------------------------------------
    // Event dispatch
    // ------------------------------------------------------------------

    async fn handle_event(&self, msg: AgentMessage) {
        debug!(
            "AgentEventLoop {} received event: {:?}",
            self.agent_id,
            std::mem::discriminant(&msg.message_type)
        );

        match &msg.message_type {
            AgentMessageType::WorkflowStepAssigned {
                workflow_id,
                step_id,
                step_name,
                task_prompt,
                ..
            } => {
                self.handle_step_assigned(
                    msg.from.clone(),
                    workflow_id.clone(),
                    step_id.clone(),
                    step_name.clone(),
                    task_prompt.clone(),
                    msg.correlation_id.clone(),
                )
                .await;
            }

            AgentMessageType::Request {
                request_type,
                payload,
            } => {
                self.handle_request(
                    msg.from.clone(),
                    request_type.clone(),
                    payload.clone(),
                    msg.correlation_id.clone(),
                )
                .await;
            }

            AgentMessageType::ArtifactUpdate {
                artifact_id,
                version,
                change_summary,
            } => {
                self.handle_artifact_update(artifact_id.clone(), *version, change_summary.clone())
                    .await;
            }

            // StepCompleted events are consumed by WorkflowEngine, not agents
            AgentMessageType::StepCompleted { .. } => {}

            // Responses and broadcasts not addressed to us — ignore
            AgentMessageType::Response { .. } | AgentMessageType::Broadcast { .. } => {}
        }
    }

    // ------------------------------------------------------------------
    // Handlers
    // ------------------------------------------------------------------

    /// Execute a workflow step assigned by the `WorkflowEngine` and publish
    /// a `StepCompleted` event back on the bus.
    async fn handle_step_assigned(
        &self,
        engine_id: AgentId,
        workflow_id: String,
        step_id: String,
        step_name: String,
        task_prompt: String,
        correlation_id: Option<String>,
    ) {
        info!(
            "Agent {} executing step '{}' (workflow {})",
            self.agent_id, step_name, workflow_id
        );

        let session_key = format!("workflow:{}:{}", workflow_id, step_id);
        let (success, output, error) = match tokio::time::timeout(
            self.step_timeout,
            self.agent_loop.process_direct(&task_prompt, &session_key),
        )
        .await
        {
            Ok(Ok(out)) => {
                info!("Step '{}' completed by {}", step_name, self.agent_id);
                (true, out, None)
            }
            Ok(Err(e)) => {
                warn!("Step '{}' failed for {}: {}", step_name, self.agent_id, e);
                (false, String::new(), Some(e.to_string()))
            }
            Err(_) => {
                let secs = self.step_timeout.as_secs();
                warn!(
                    "Step '{}' timed out after {}s for {}",
                    step_name, secs, self.agent_id
                );
                self.health.errors.fetch_add(1, Ordering::Relaxed);
                (
                    false,
                    String::new(),
                    Some(format!("step timed out after {secs}s")),
                )
            }
        };

        let reply = AgentMessage::step_completed(
            workflow_id,
            step_id,
            success,
            output,
            vec![],
            error,
            self.agent_id.clone(),
            engine_id,
            correlation_id,
        );

        if let Err(e) = self.event_bus.publish(reply).await {
            warn!(
                "AgentEventLoop {}: failed to publish StepCompleted: {}",
                self.agent_id, e
            );
        }
    }

    /// Handle a `Request` message by running the payload through `process_direct`
    /// and replying with a `Response`.
    async fn handle_request(
        &self,
        from: AgentId,
        request_type: RequestType,
        payload: serde_json::Value,
        correlation_id: Option<String>,
    ) {
        let prompt = format!(
            "[{}] {}",
            format!("{:?}", request_type).to_lowercase(),
            payload
        );
        // Use correlation_id as the session key so each request gets an
        // isolated conversation context rather than accumulating into a single
        // shared session per agent.
        let session_key = match &correlation_id {
            Some(cid) => format!("agent_request:{}:{}", self.agent_id, cid),
            None => format!("agent_request:{}", self.agent_id),
        };

        let (success, response_payload) = match tokio::time::timeout(
            self.step_timeout,
            self.agent_loop.process_direct(&prompt, &session_key),
        )
        .await
        {
            Ok(Ok(out)) => (true, serde_json::json!({"response": out})),
            Ok(Err(e)) => (false, serde_json::json!({"error": e.to_string()})),
            Err(_) => {
                let secs = self.step_timeout.as_secs();
                warn!(
                    "AgentEventLoop {}: request timed out after {}s",
                    self.agent_id, secs
                );
                self.health.errors.fetch_add(1, Ordering::Relaxed);
                (false, serde_json::json!({"error": format!("request timed out after {secs}s")}))
            }
        };

        if let Some(cid) = correlation_id {
            let reply =
                AgentMessage::response(self.agent_id.clone(), from, cid, success, response_payload);
            if let Err(e) = self.event_bus.publish(reply).await {
                warn!(
                    "AgentEventLoop {}: failed to publish Response: {}",
                    self.agent_id, e
                );
            }
        }
    }

    /// Handle an `ArtifactUpdate` event.
    ///
    /// Default: log only.  Override behaviour by subclassing or wrapping
    /// when role-specific reactions are needed (e.g. reviewer auto-triggers).
    async fn handle_artifact_update(
        &self,
        artifact_id: String,
        version: u32,
        change_summary: String,
    ) {
        debug!(
            "Agent {} notified: artifact '{}' updated to v{} — {}",
            self.agent_id, artifact_id, version, change_summary
        );

        // Role-based reactions: reviewer agents auto-initiate review
        if self.agent_id.role == "reviewer" {
            if let Some(artifact) = self.workspace.get_artifact(&artifact_id).await {
                use crate::agent::collaboration::workspace::ArtifactType;
                if matches!(artifact.artifact_type, ArtifactType::CodeFile { .. }) {
                    info!(
                        "Reviewer {} auto-triggering review of artifact '{}'",
                        self.agent_id, artifact_id
                    );
                    let prompt = format!(
                        "A new version (v{}) of artifact '{}' has been created: {}\n\
                         Please review it for correctness, code quality, and best practices.",
                        version, artifact_id, change_summary
                    );
                    let session_key = format!("review:{}:{}", artifact_id, version);
                    match self.agent_loop.process_direct(&prompt, &session_key).await {
                        Ok(review) => info!(
                            "Reviewer {} completed auto-review of '{}': {}",
                            self.agent_id,
                            artifact_id,
                            &review[..review.len().min(100)]
                        ),
                        Err(e) => warn!(
                            "Reviewer {} failed auto-review of '{}': {}",
                            self.agent_id, artifact_id, e
                        ),
                    }
                }
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent::collaboration::workspace::SharedWorkspace;
    use crate::agent::communication::AgentMessageBus;
    use std::path::PathBuf;
    use std::time::Duration;

    #[test]
    fn test_health_counters_default_zero() {
        let h = LoopHealthCounters::new();
        let snap = h.snapshot();
        assert_eq!(snap.events_processed, 0);
        assert_eq!(snap.errors, 0);
        assert_eq!(snap.last_heartbeat_secs, 0);
    }

    #[test]
    fn test_health_counters_increment() {
        let h = LoopHealthCounters::new();
        h.events_processed.fetch_add(3, Ordering::Relaxed);
        h.errors.fetch_add(1, Ordering::Relaxed);
        h.last_heartbeat_secs.store(9_999_999, Ordering::Relaxed);
        let snap = h.snapshot();
        assert_eq!(snap.events_processed, 3);
        assert_eq!(snap.errors, 1);
        assert_eq!(snap.last_heartbeat_secs, 9_999_999);
    }

    #[test]
    fn test_health_counters_arc_shared() {
        // Both handles see the same underlying data.
        let h = LoopHealthCounters::new();
        let h2 = h.clone();
        h.events_processed.fetch_add(7, Ordering::Relaxed);
        assert_eq!(h2.snapshot().events_processed, 7);
    }

    #[tokio::test]
    async fn test_event_loop_state_transitions() {
        let agent_id = AgentId::new("team1", "developer", "dev-1");
        let bus = Arc::new(AgentMessageBus::new());
        let workspace = Arc::new(SharedWorkspace::new("team1", PathBuf::from("/tmp")));

        // Only test state field directly without running the loop
        let state = Arc::new(RwLock::new(EventLoopState::Idle));
        assert_eq!(*state.read().await, EventLoopState::Idle);

        *state.write().await = EventLoopState::Running;
        assert_eq!(*state.read().await, EventLoopState::Running);

        *state.write().await = EventLoopState::Stopped;
        assert_eq!(*state.read().await, EventLoopState::Stopped);

        // Suppress unused warnings
        let _ = agent_id;
        let _ = bus;
        let _ = workspace;
    }

    #[tokio::test]
    async fn test_shutdown_signal() {
        // Verify watch channel works for shutdown coordination
        let (tx, rx) = watch::channel(false);
        assert!(!*rx.borrow());
        tx.send(true).unwrap();
        assert!(*rx.borrow());
    }

    #[tokio::test]
    async fn test_workflow_step_assigned_message_roundtrip() {
        use std::collections::HashMap;

        let engine_id = AgentId::new("team1", "workflow", "engine");
        let agent_id = AgentId::new("team1", "developer", "dev-1");

        let msg = AgentMessage::workflow_step_assigned(
            "wf-001",
            "implement",
            "Implement Feature",
            "developer",
            "Build the login system",
            HashMap::new(),
            engine_id.clone(),
            agent_id.clone(),
        );

        assert_eq!(msg.from, engine_id);
        assert_eq!(msg.to, Some(agent_id));

        if let AgentMessageType::WorkflowStepAssigned {
            workflow_id,
            step_id,
            step_name,
            ..
        } = &msg.message_type
        {
            assert_eq!(workflow_id, "wf-001");
            assert_eq!(step_id, "implement");
            assert_eq!(step_name, "Implement Feature");
        } else {
            panic!("expected WorkflowStepAssigned");
        }

        // Round-trip serialization
        let json = serde_json::to_string(&msg).unwrap();
        let back: AgentMessage = serde_json::from_str(&json).unwrap();
        assert_eq!(back.id, msg.id);
    }

    #[tokio::test]
    async fn test_step_completed_message_roundtrip() {
        let agent_id = AgentId::new("team1", "developer", "dev-1");
        let engine_id = AgentId::new("team1", "workflow", "engine");

        let msg = AgentMessage::step_completed(
            "wf-001",
            "implement",
            true,
            "Done!",
            vec!["implementation".to_string()],
            None,
            agent_id.clone(),
            engine_id.clone(),
            Some("corr-123".to_string()),
        );

        assert_eq!(msg.from, agent_id);
        assert_eq!(msg.to, Some(engine_id));
        assert_eq!(msg.correlation_id, Some("corr-123".to_string()));

        if let AgentMessageType::StepCompleted {
            workflow_id,
            step_id,
            success,
            output,
            ..
        } = &msg.message_type
        {
            assert_eq!(workflow_id, "wf-001");
            assert_eq!(step_id, "implement");
            assert!(success);
            assert_eq!(output, "Done!");
        } else {
            panic!("expected StepCompleted");
        }

        let json = serde_json::to_string(&msg).unwrap();
        let back: AgentMessage = serde_json::from_str(&json).unwrap();
        assert_eq!(back.id, msg.id);
    }
}
