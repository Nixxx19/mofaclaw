//! Team management for multi-agent collaboration
//!
//! Provides structures and functionality for creating and managing teams of agents
//! with different roles.
//!
//! Persistence: when constructed with `with_persistence`, team metadata
//! (id, name, roles) is saved to `<data_dir>/teams/<id>.json` on creation.
//! On restart `list_teams` includes both active (in-memory) and inactive
//! (disk-only) teams, with a clear status so the agent knows which need
//! to be recreated before use.

use crate::Config;
use crate::agent::collaboration::event_loop::{AgentEventLoop, LoopHealthCounters};
use crate::agent::collaboration::workspace::SharedWorkspace;
use crate::agent::communication::{AgentId, AgentMessageBus};
use crate::agent::context::ContextBuilder;
use crate::agent::loop_::AgentLoop;
use crate::agent::roles::{AgentRole, RoleRegistry};
use crate::bus::MessageBus;
use crate::error::Result;
use crate::provider::{LLMAgentBuilder, OpenAIConfig, OpenAIProvider};
use crate::session::SessionManager;
use crate::tools::registry::ToolRegistryExecutor;
use crate::tools::{
    EditFileTool, ExecTool, ListDirTool, MessageTool, ReadFileTool, ToolRegistry, WebFetchTool,
    WebSearchTool, WriteFileTool,
    agent_message::{
        BroadcastToTeamTool, RespondToApprovalTool, SendAgentMessageTool,
        WaitForAgentResponseTool,
    },
};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Weak};
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use tokio::sync::{RwLock, watch};
use tokio::task::JoinHandle;
use tracing::{info, warn};

/// Lightweight snapshot of a team saved to disk for persistence.
/// Does not include live agent state (AgentLoop instances).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TeamSnapshot {
    pub id: String,
    pub name: String,
    /// (role_name, instance_id) pairs — enough to recreate the team
    pub roles: Vec<(String, String)>,
    pub member_count: usize,
    pub created_at: DateTime<Utc>,
    /// Whether this team was created with event-driven mode enabled.
    /// Persisted so callers know to re-enable event loops on restart.
    #[serde(default)]
    pub event_driven: bool,
}

/// Summary of a team as returned by `list_teams_detailed`.
#[derive(Debug, Clone)]
pub struct TeamSummary {
    pub id: String,
    pub name: String,
    pub member_count: usize,
    pub created_at: DateTime<Utc>,
    /// True if the team has live agents and can run workflows immediately.
    /// False means it was persisted from a previous session and must be
    /// recreated with `create_team` before use.
    pub active: bool,
}

/// Status of an agent team
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TeamStatus {
    /// Team is being formed
    Forming,
    /// Team is active and working
    Active,
    /// Team is paused
    Paused,
    /// Team has completed its work
    Completed,
}

/// Status of a team member
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MemberStatus {
    /// Member is active
    Active,
    /// Member is idle
    Idle,
    /// Member is working on a task
    Working,
    /// Member has completed their task
    Completed,
}

/// A member of an agent team
pub struct TeamMember {
    /// Agent identifier
    pub agent_id: AgentId,
    /// Role definition for this member
    pub role: Arc<dyn AgentRole>,
    /// Agent loop instance
    pub agent_loop: Arc<AgentLoop>,
    /// Current status
    pub status: MemberStatus,
}

/// An agent team consisting of multiple agents with different roles
pub struct AgentTeam {
    /// Unique team identifier
    pub id: String,
    /// Team name
    pub name: String,
    /// Team members mapped by agent ID
    pub members: HashMap<String, TeamMember>,
    /// When the team was created
    pub created_at: DateTime<Utc>,
    /// Current team status
    pub status: TeamStatus,
    /// Agent message bus for inter-agent communication
    pub message_bus: Arc<AgentMessageBus>,
}

impl AgentTeam {
    /// Create a new team
    pub fn new(id: impl Into<String>, name: impl Into<String>) -> Self {
        Self {
            id: id.into(),
            name: name.into(),
            members: HashMap::new(),
            created_at: Utc::now(),
            status: TeamStatus::Forming,
            message_bus: Arc::new(AgentMessageBus::new()),
        }
    }

    /// Add a member to the team
    pub fn add_member(&mut self, member: TeamMember) {
        let agent_key = member.agent_id.to_string();
        info!("Adding member {} to team {}", agent_key, self.id);
        self.members.insert(agent_key, member);
    }

    /// Remove a member from the team
    pub fn remove_member(&mut self, agent_id: &AgentId) -> Option<TeamMember> {
        let agent_key = agent_id.to_string();
        info!("Removing member {} from team {}", agent_key, self.id);
        self.members.remove(&agent_key)
    }

    /// Get a member by agent ID
    pub fn get_member(&self, agent_id: &AgentId) -> Option<&TeamMember> {
        let agent_key = agent_id.to_string();
        self.members.get(&agent_key)
    }

    /// Get all members with a specific role
    pub fn get_members_by_role(&self, role: &str) -> Vec<&TeamMember> {
        self.members
            .values()
            .filter(|member| member.agent_id.role == role)
            .collect()
    }

    /// Broadcast a message to all team members
    pub async fn broadcast(
        &self,
        message: crate::agent::communication::AgentMessage,
    ) -> Result<()> {
        self.message_bus.publish(message).await
    }

    /// Get the number of members in the team
    pub fn member_count(&self) -> usize {
        self.members.len()
    }

    /// Check if team is ready (has at least one member)
    pub fn is_ready(&self) -> bool {
        !self.members.is_empty()
    }
}

/// Health status of a single agent event loop within a team.
#[derive(Debug, Clone)]
pub struct EventLoopStatus {
    pub agent_id: AgentId,
    /// Whether the Tokio task is still running (not finished).
    pub is_alive: bool,
    pub events_processed: u64,
    pub errors: u64,
    /// Seconds elapsed since the loop last ticked.  `None` if it has never started.
    pub secs_since_heartbeat: Option<u64>,
}

/// Internal bookkeeping for a team's spawned event loops.
struct TeamEventLoopSet {
    shutdown_tx: watch::Sender<bool>,
    /// `(agent_id, join_handle, health_counters)` — one entry per team member.
    loops: Vec<(AgentId, JoinHandle<()>, Arc<LoopHealthCounters>)>,
}

/// Manager for creating and managing agent teams
pub struct TeamManager {
    /// Live teams with instantiated agents
    teams: Arc<RwLock<HashMap<String, Arc<AgentTeam>>>>,
    /// Persisted snapshots loaded from disk (includes inactive teams)
    snapshots: Arc<RwLock<HashMap<String, TeamSnapshot>>>,
    /// Role registry
    role_registry: Arc<RoleRegistry>,
    #[allow(dead_code)]
    user_bus: MessageBus,
    sessions: Arc<SessionManager>,
    config: Arc<Config>,
    data_dir: Option<PathBuf>,
    /// Weak self-reference for tool registration (set after wrapping in Arc)
    self_ref: Arc<RwLock<Option<Weak<TeamManager>>>>,
    /// Shared workspace used when spawning `AgentEventLoop` instances.
    /// Set via `set_workspace` after wrapping in Arc.
    workspace: Arc<RwLock<Option<Arc<SharedWorkspace>>>>,
    /// Active event loop sets, keyed by team ID.
    event_loop_handles: Arc<RwLock<HashMap<String, TeamEventLoopSet>>>,
}

impl TeamManager {
    /// Create a team manager with no persistence (in-memory only).
    pub fn new(config: Arc<Config>, user_bus: MessageBus, sessions: Arc<SessionManager>) -> Self {
        Self {
            teams: Arc::new(RwLock::new(HashMap::new())),
            snapshots: Arc::new(RwLock::new(HashMap::new())),
            role_registry: Arc::new(RoleRegistry::new()),
            user_bus,
            sessions,
            config,
            data_dir: None,
            self_ref: Arc::new(RwLock::new(None)),
            workspace: Arc::new(RwLock::new(None)),
            event_loop_handles: Arc::new(RwLock::new(HashMap::new())),
        }
    }

    /// Create a team manager that persists team metadata to `data_dir/teams/`.
    /// Previously-created teams appear in `list_teams` as inactive so the
    /// agent knows they existed and can decide whether to recreate them.
    pub async fn with_persistence(
        config: Arc<Config>,
        user_bus: MessageBus,
        sessions: Arc<SessionManager>,
        data_dir: PathBuf,
    ) -> Self {
        let manager = Self {
            teams: Arc::new(RwLock::new(HashMap::new())),
            snapshots: Arc::new(RwLock::new(HashMap::new())),
            role_registry: Arc::new(RoleRegistry::new()),
            user_bus,
            sessions,
            config,
            data_dir: Some(data_dir),
            self_ref: Arc::new(RwLock::new(None)),
            workspace: Arc::new(RwLock::new(None)),
            event_loop_handles: Arc::new(RwLock::new(HashMap::new())),
        };
        manager.load_snapshots().await;
        manager
    }

    /// Set self-reference (call after wrapping in Arc)
    pub async fn set_self_ref(self_ref: &Arc<TeamManager>) {
        *self_ref.self_ref.write().await = Some(Arc::downgrade(self_ref));
    }

    /// Attach the shared workspace used when spawning `AgentEventLoop` instances.
    /// Call after both `TeamManager` and `SharedWorkspace` are wrapped in `Arc`.
    pub async fn set_workspace(manager: &Arc<TeamManager>, workspace: Arc<SharedWorkspace>) {
        *manager.workspace.write().await = Some(workspace);
    }

    // ------------------------------------------------------------------
    // Persistence helpers
    // ------------------------------------------------------------------

    fn teams_dir(&self) -> Option<PathBuf> {
        self.data_dir.as_ref().map(|d| d.join("teams"))
    }

    async fn save_snapshot(&self, snapshot: &TeamSnapshot) {
        let Some(dir) = self.teams_dir() else { return };
        if let Err(e) = tokio::fs::create_dir_all(&dir).await {
            warn!("Failed to create teams dir: {}", e);
            return;
        }
        let path = dir.join(format!("{}.json", snapshot.id));
        match serde_json::to_string_pretty(snapshot) {
            Ok(json) => {
                if let Err(e) = tokio::fs::write(&path, json).await {
                    warn!("Failed to save team snapshot {}: {}", snapshot.id, e);
                }
            }
            Err(e) => warn!("Failed to serialize team {}: {}", snapshot.id, e),
        }
    }

    async fn load_snapshots(&self) {
        let Some(dir) = self.teams_dir() else { return };
        let mut entries = match tokio::fs::read_dir(&dir).await {
            Ok(e) => e,
            Err(_) => return,
        };
        let mut snapshots = self.snapshots.write().await;
        while let Ok(Some(entry)) = entries.next_entry().await {
            let path = entry.path();
            if path.extension().is_some_and(|e| e == "json")
                && let Ok(bytes) = tokio::fs::read(&path).await
                && let Ok(snap) = serde_json::from_slice::<TeamSnapshot>(&bytes)
            {
                info!("Loaded team snapshot: {} ({})", snap.id, snap.name);
                snapshots.insert(snap.id.clone(), snap);
            }
        }
    }

    /// Create a new team with specified roles
    ///
    /// This will:
    /// 1. Create a new team
    /// 2. Instantiate agents for each role
    /// 3. Configure each agent with role-specific settings
    /// 4. Add agents to the team
    pub async fn create_team(
        &self,
        team_id: impl Into<String>,
        team_name: impl Into<String>,
        roles: Vec<(String, String)>, // (role_name, instance_id)
    ) -> Result<Arc<AgentTeam>> {
        let team_id = team_id.into();
        let team_name = team_name.into();

        info!(
            "Creating team {} ({}) with roles: {:?}",
            team_id, team_name, roles
        );

        let mut team = AgentTeam::new(&team_id, &team_name);

        // Capture roles for snapshot before consuming the Vec
        let roles_for_snapshot = roles.clone();

        // Create agents for each role
        for (role_name, instance_id) in roles {
            let role = self.role_registry.get_role(&role_name).ok_or_else(|| {
                crate::error::MofaclawError::Other(format!("Role '{}' not found", role_name))
            })?;

            let agent_id = AgentId::new(&team_id, &role_name, &instance_id);

            // Create agent loop with role-specific configuration
            let member = self.create_team_member(agent_id, role).await?;
            team.add_member(member);
        }

        // Mark team as active
        team.status = TeamStatus::Active;

        let member_count = team.member_count();
        let created_at = team.created_at;

        // Store live team
        let team_arc = Arc::new(team);
        let team_id_clone = team_arc.id.clone();
        {
            let mut teams = self.teams.write().await;
            teams.insert(team_id_clone.clone(), team_arc);
        }

        // Persist snapshot so the team appears across restarts.
        // Mark event_driven=true when a workspace is attached (loops will be spawned).
        let has_workspace = self.workspace.read().await.is_some();
        let snapshot = TeamSnapshot {
            id: team_id_clone.clone(),
            name: team_name.clone(),
            roles: roles_for_snapshot,
            event_driven: has_workspace,
            member_count,
            created_at,
        };
        {
            let mut snaps = self.snapshots.write().await;
            snaps.insert(team_id_clone.clone(), snapshot.clone());
        }
        self.save_snapshot(&snapshot).await;

        info!("Team {} created with {} members", team_id, member_count);

        // Spawn AgentEventLoop for each member if a workspace is attached.
        // One watch channel per team — all member loops share the same receiver.
        let workspace_guard = self.workspace.read().await;
        if let Some(ref workspace) = *workspace_guard {
            let teams_guard = self.teams.read().await;
            let team = teams_guard.get(&team_id_clone).unwrap();
            let (shutdown_tx, shutdown_rx) = watch::channel(false);
            let mut loop_entries = Vec::new();
            for member in team.members.values() {
                let health = LoopHealthCounters::new();
                let el = AgentEventLoop::new_with_health(
                    member.agent_id.clone(),
                    member.agent_loop.clone(),
                    team.message_bus.clone(),
                    workspace.clone(),
                    health.clone(),
                );
                let rx = shutdown_rx.clone();
                let handle = tokio::spawn(async move { el.run(rx).await });
                loop_entries.push((member.agent_id.clone(), handle, health));
            }
            drop(teams_guard);
            let set = TeamEventLoopSet { shutdown_tx, loops: loop_entries };
            self.event_loop_handles
                .write()
                .await
                .insert(team_id_clone.clone(), set);
            info!("Spawned AgentEventLoop instances for team {}", team_id_clone);
        }

        let teams = self.teams.read().await;
        Ok(teams.get(&team_id_clone).unwrap().clone())
    }

    /// Create a team member with role-specific configuration
    async fn create_team_member(
        &self,
        agent_id: AgentId,
        role: Arc<dyn AgentRole>,
    ) -> Result<TeamMember> {
        info!(
            "Creating team member {} with role {}",
            agent_id,
            role.name()
        );

        // Get API key and base URL from config
        let api_key = self.config.get_api_key().ok_or_else(|| {
            crate::error::MofaclawError::Other("No API key configured".to_string())
        })?;
        let api_base = self.config.get_api_base();

        // Create OpenAI provider
        let openai_config = OpenAIConfig::new(&api_key)
            .with_model(&self.config.agents.defaults.model)
            .with_base_url(api_base.unwrap_or_else(|| "https://openrouter.ai/api/v1".to_string()));
        let provider = Arc::new(OpenAIProvider::with_config(openai_config));

        // Create a separate message bus for this agent (or use team's message bus)
        // For now, create a minimal bus for agent-to-agent communication
        let agent_bus = MessageBus::new();

        // Create tool registry
        let tools = Arc::new(RwLock::new(ToolRegistry::new()));
        let _workspace = self.config.workspace_path();
        let brave_api_key = self.config.get_brave_api_key();

        // Register tools filtered by role capabilities
        {
            let mut tools_guard = tools.write().await;
            let allowed_tools = role.allowed_tools();

            // Register default tools, but filter based on role
            if allowed_tools.is_empty() || allowed_tools.contains(&"read_file".to_string()) {
                tools_guard.register(ReadFileTool::new());
            }
            if allowed_tools.is_empty() || allowed_tools.contains(&"write_file".to_string()) {
                tools_guard.register(WriteFileTool::new());
            }
            if allowed_tools.is_empty() || allowed_tools.contains(&"edit_file".to_string()) {
                tools_guard.register(EditFileTool::new());
            }
            if allowed_tools.is_empty() || allowed_tools.contains(&"list_dir".to_string()) {
                tools_guard.register(ListDirTool::new());
            }
            if (allowed_tools.is_empty() || allowed_tools.contains(&"exec".to_string()))
                && role.capabilities().can_execute_commands
            {
                tools_guard.register(ExecTool::new());
            }
            if allowed_tools.is_empty() || allowed_tools.contains(&"web_search".to_string()) {
                tools_guard.register(WebSearchTool::new(brave_api_key.clone()));
            }
            if allowed_tools.is_empty() || allowed_tools.contains(&"web_fetch".to_string()) {
                tools_guard.register(WebFetchTool::new());
            }
            if allowed_tools.is_empty() || allowed_tools.contains(&"message".to_string()) {
                tools_guard.register(MessageTool::new());
            }

            // Register multi-agent collaboration tools
            // These tools enable agents to communicate and coordinate
            if let Some(weak_manager) = self.self_ref.read().await.as_ref()
                && let Some(manager) = weak_manager.upgrade()
            {
                tools_guard.register(SendAgentMessageTool::new(manager.clone()));
                tools_guard.register(BroadcastToTeamTool::new(manager.clone()));
                tools_guard.register(RespondToApprovalTool::new(manager.clone()));
                tools_guard.register(WaitForAgentResponseTool::new(manager.clone()));
            }
        }

        // Create tool executor
        let tool_executor = Arc::new(ToolRegistryExecutor::new(tools.clone()));

        // Build role-specific system prompt
        let role_system_prompt = role.system_prompt();

        // Create LLMAgent with role-specific system prompt
        let llm_agent = Arc::new(
            LLMAgentBuilder::new()
                .with_id(agent_id.to_string())
                .with_name(format!("{} Agent", role.name()))
                .with_provider(provider.clone())
                .with_system_prompt(role_system_prompt)
                .with_tool_executor(tool_executor)
                .build_async()
                .await,
        );

        // Create AgentLoop with role-specific configuration
        let _capabilities = role.capabilities();
        // Note: max_iterations and temperature are set via LLMAgentBuilder's system prompt
        // The AgentLoop will use config defaults, but the role's system prompt provides
        // role-specific behavior guidance

        // Create context builder (will use role-specific prompt when needed)
        let _context = ContextBuilder::new(&self.config);

        // Get role capabilities for custom configuration
        let capabilities = role.capabilities();
        let max_iterations_override = capabilities.max_iterations.map(|v| v as usize);
        let temperature_override = capabilities.temperature;

        // Create agent loop with role-specific configuration
        let agent_loop = AgentLoop::with_agent_and_tools_custom(
            &self.config,
            llm_agent,
            provider,
            agent_bus,
            self.sessions.clone(),
            tools.clone(),
            max_iterations_override,
            temperature_override,
        )
        .await?;

        Ok(TeamMember {
            agent_id,
            role,
            agent_loop: Arc::new(agent_loop),
            status: MemberStatus::Idle,
        })
    }

    /// Get a team by ID
    pub async fn get_team(&self, team_id: &str) -> Option<Arc<AgentTeam>> {
        let teams = self.teams.read().await;
        teams.get(team_id).cloned()
    }

    /// List IDs of all known teams: both active (in-memory) and inactive
    /// (persisted from previous sessions).
    pub async fn list_teams(&self) -> Vec<String> {
        let mut ids: std::collections::HashSet<String> =
            self.teams.read().await.keys().cloned().collect();
        for id in self.snapshots.read().await.keys() {
            ids.insert(id.clone());
        }
        ids.into_iter().collect()
    }

    /// Return rich summaries of all known teams, distinguishing active from
    /// inactive (persisted-only) teams.
    pub async fn list_teams_detailed(&self) -> Vec<TeamSummary> {
        let active_teams = self.teams.read().await;
        let snapshots = self.snapshots.read().await;

        let mut summaries: HashMap<String, TeamSummary> = HashMap::new();

        // Active teams take priority
        for (id, team) in active_teams.iter() {
            summaries.insert(
                id.clone(),
                TeamSummary {
                    id: id.clone(),
                    name: team.name.clone(),
                    member_count: team.member_count(),
                    created_at: team.created_at,
                    active: true,
                },
            );
        }

        // Add inactive (snapshot-only) teams
        for (id, snap) in snapshots.iter() {
            if !summaries.contains_key(id) {
                summaries.insert(
                    id.clone(),
                    TeamSummary {
                        id: id.clone(),
                        name: snap.name.clone(),
                        member_count: snap.member_count,
                        created_at: snap.created_at,
                        active: false,
                    },
                );
            }
        }

        summaries.into_values().collect()
    }

    /// Get the persisted snapshot for a team (works even if the team is
    /// not currently active in memory).
    pub async fn get_team_snapshot(&self, team_id: &str) -> Option<TeamSnapshot> {
        self.snapshots.read().await.get(team_id).cloned()
    }

    /// Remove a team and stop any running event loops for it.
    pub async fn remove_team(&self, team_id: &str) -> Option<Arc<AgentTeam>> {
        if let Some(set) = self.event_loop_handles.write().await.remove(team_id) {
            let _ = set.shutdown_tx.send(true);
        }
        let mut teams = self.teams.write().await;
        teams.remove(team_id)
    }

    /// Return the `AgentMessageBus` for every currently-active team.
    pub async fn all_team_buses(&self) -> Vec<Arc<AgentMessageBus>> {
        self.teams
            .read()
            .await
            .values()
            .map(|t| t.message_bus.clone())
            .collect()
    }

    /// Test helper: inject a pre-built `AgentTeam` into the live team map.
    /// Allows testing tools that require a team without needing an API key.
    #[cfg(test)]
    pub async fn insert_test_team(&self, team: AgentTeam) {
        let arc = Arc::new(team);
        self.teams.write().await.insert(arc.id.clone(), arc);
    }

    /// Test helper: Check if event loops are running for a team.
    #[cfg(test)]
    pub async fn has_event_loops(&self, team_id: &str) -> bool {
        self.event_loop_handles
            .read()
            .await
            .get(team_id)
            .map(|s| !s.loops.is_empty())
            .unwrap_or(false)
    }

    /// Test helper: Get total count of tracked event loop tasks across all teams.
    #[cfg(test)]
    pub async fn event_loop_count(&self) -> usize {
        self.event_loop_handles
            .read()
            .await
            .values()
            .map(|s| s.loops.len())
            .sum()
    }

    /// Return the health status of every event loop running for `team_id`.
    ///
    /// Returns an empty `Vec` if the team has no spawned loops (e.g. no workspace
    /// was set when the team was created).
    pub async fn team_health(&self, team_id: &str) -> Vec<EventLoopStatus> {
        let handles = self.event_loop_handles.read().await;
        let Some(set) = handles.get(team_id) else {
            return vec![];
        };
        let now_secs = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();
        set.loops
            .iter()
            .map(|(agent_id, handle, health)| {
                let snap = health.snapshot();
                let secs_since = if snap.last_heartbeat_secs > 0 {
                    Some(now_secs.saturating_sub(snap.last_heartbeat_secs))
                } else {
                    None
                };
                EventLoopStatus {
                    agent_id: agent_id.clone(),
                    is_alive: !handle.is_finished(),
                    events_processed: snap.events_processed,
                    errors: snap.errors,
                    secs_since_heartbeat: secs_since,
                }
            })
            .collect()
    }

    /// Return health status for every event loop across all teams.
    pub async fn all_team_health(&self) -> HashMap<String, Vec<EventLoopStatus>> {
        let team_ids: Vec<String> = self
            .event_loop_handles
            .read()
            .await
            .keys()
            .cloned()
            .collect();
        let mut result = HashMap::new();
        for id in team_ids {
            result.insert(id.clone(), self.team_health(&id).await);
        }
        result
    }

    /// Restart any event loop tasks that have finished unexpectedly.
    ///
    /// Returns the number of loops that were restarted.  A workspace must be
    /// attached via `set_workspace` for restarts to succeed.
    pub async fn restart_dead_loops(&self, team_id: &str) -> usize {
        let workspace_guard = self.workspace.read().await;
        let Some(ref workspace) = *workspace_guard else {
            return 0;
        };
        let workspace = workspace.clone();
        drop(workspace_guard);

        let teams_guard = self.teams.read().await;
        let Some(team) = teams_guard.get(team_id) else {
            return 0;
        };
        let team = team.clone();
        drop(teams_guard);

        let mut handles = self.event_loop_handles.write().await;
        let Some(set) = handles.get_mut(team_id) else {
            return 0;
        };

        let mut restarted = 0;
        for (agent_id, handle, health) in set.loops.iter_mut() {
            if handle.is_finished() {
                warn!(
                    "AgentEventLoop for {} finished unexpectedly; restarting",
                    agent_id
                );
                let new_health = LoopHealthCounters::new();
                if let Some(member) = team.get_member(agent_id) {
                    let el = AgentEventLoop::new_with_health(
                        agent_id.clone(),
                        member.agent_loop.clone(),
                        team.message_bus.clone(),
                        workspace.clone(),
                        new_health.clone(),
                    );
                    let rx = set.shutdown_tx.subscribe();
                    *handle = tokio::spawn(async move { el.run(rx).await });
                    *health = new_health;
                    restarted += 1;
                    info!("Restarted AgentEventLoop for {}", agent_id);
                }
            }
        }
        restarted
    }

    /// Spawn a background task that periodically restarts any dead event loops.
    ///
    /// The supervisor holds only a `Weak` reference to `TeamManager` — it stops
    /// automatically when the last `Arc<TeamManager>` is dropped, so no explicit
    /// shutdown is needed.
    ///
    /// `interval` — how often to check all teams (default: 30 seconds is reasonable).
    ///
    /// Call once after setup:
    /// ```rust,ignore
    /// TeamManager::start_health_supervisor(&manager, Duration::from_secs(30));
    /// ```
    /// On startup, restore teams that were running with event-driven mode in the previous
    /// session.
    ///
    /// Runs entirely in the background — the process remains responsive immediately.
    /// A 2-second delay lets the rest of startup finish before the first API call is made.
    /// Errors are logged as warnings; the process never crashes due to a failed restoration.
    ///
    /// **Must be called after `set_workspace`** — otherwise event loops won't be spawned
    /// for the restored teams.
    pub fn restore_event_driven_teams_background(manager: &Arc<Self>) {
        let weak = Arc::downgrade(manager);
        tokio::spawn(async move {
            // Let the rest of startup (LLM agent build, channel connections) finish first.
            tokio::time::sleep(Duration::from_secs(2)).await;

            let Some(mgr) = weak.upgrade() else { return };

            let snapshots: Vec<TeamSnapshot> = mgr
                .snapshots
                .read()
                .await
                .values()
                .filter(|s| s.event_driven)
                .cloned()
                .collect();

            if snapshots.is_empty() {
                return;
            }

            info!(
                "Restoring {} event-driven team(s) from previous session",
                snapshots.len()
            );

            for snap in snapshots {
                // Already recreated (e.g. user ran create_team before restoration fired).
                if mgr.teams.read().await.contains_key(&snap.id) {
                    info!("Team '{}' already active, skipping restoration", snap.id);
                    continue;
                }

                info!("Restoring team '{}' ({})", snap.id, snap.name);
                match mgr
                    .create_team(&snap.id, &snap.name, snap.roles.clone())
                    .await
                {
                    Ok(_) => info!("Restored team '{}'", snap.id),
                    Err(e) => warn!(
                        "Failed to restore team '{}': {} — recreate it manually if needed",
                        snap.id, e
                    ),
                }
            }
        });
    }

    pub fn start_health_supervisor(manager: &Arc<Self>, interval: Duration) {
        let weak = Arc::downgrade(manager);
        tokio::spawn(async move {
            let mut ticker = tokio::time::interval(interval);
            ticker.tick().await; // skip the immediate first tick
            loop {
                ticker.tick().await;
                let Some(mgr) = weak.upgrade() else {
                    info!("Health supervisor: TeamManager dropped, stopping");
                    break;
                };
                let team_ids: Vec<String> = mgr
                    .event_loop_handles
                    .read()
                    .await
                    .keys()
                    .cloned()
                    .collect();
                for id in team_ids {
                    let restarted = mgr.restart_dead_loops(&id).await;
                    if restarted > 0 {
                        info!(
                            "Health supervisor restarted {} loop(s) for team {}",
                            restarted, id
                        );
                    }
                }
            }
        });
    }

    /// Broadcast a message to all members of a team
    pub async fn broadcast_to_team(
        &self,
        team_id: &str,
        message: crate::agent::communication::AgentMessage,
    ) -> Result<()> {
        let team = self.get_team(team_id).await.ok_or_else(|| {
            crate::error::MofaclawError::Other(format!("Team '{}' not found", team_id))
        })?;
        team.broadcast(message).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_agent_team_creation() {
        let team = AgentTeam::new("team1", "Test Team");
        assert_eq!(team.id, "team1");
        assert_eq!(team.name, "Test Team");
        assert_eq!(team.status, TeamStatus::Forming);
        assert_eq!(team.member_count(), 0);
    }

    #[test]
    fn test_agent_team_member_management() {
        // Note: This test is simplified since we can't easily create TeamMember
        // without full AgentLoop setup
        let team = AgentTeam::new("team1", "Test Team");
        assert_eq!(team.member_count(), 0);
        assert!(!team.is_ready());
    }

    #[test]
    fn test_team_status() {
        let team = AgentTeam::new("team1", "Test Team");
        assert_eq!(team.status, TeamStatus::Forming);
    }
}
