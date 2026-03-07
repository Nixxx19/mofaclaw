---
alwaysApply: true
---

# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## Git Conventions

- **Commit file by file**: Always commit changes file by file to create many small, focused commits. This makes the git history easier to review and understand.
- **Commit messages**: Keep commit messages short and lowercase. Use descriptive but concise messages.
- **Never use Co-Authored-By**: Never add `Co-Authored-By` trailers to commit messages.

## Build & Development Commands

```bash
# Build the binary
cargo build --release

# Install CLI from source
cargo install --path cli/

# Run directly
cargo run --release -- <command>

# Run tests
cargo test

# Run tests for a specific crate
cargo test -p mofaclaw-core

# Run a single test
cargo test -p mofaclaw-core <test_name>

# Format code
cargo fmt

# Lint
cargo clippy

# Check without building
cargo check
```

## Toolchain

Rust stable (1.85+) with `rustfmt` and `clippy` components (see `rust-toolchain.toml`).

## Architecture

mofaclaw is a Rust-based personal AI assistant with a workspace/workspace structure split into two crates:

- **`core/`** (`mofaclaw-core`) — The library crate containing all core logic
- **`cli/`** (`mofaclaw`) — The binary crate providing the `mofaclaw` CLI

### Core Subsystems (`core/src/`)

| Module | Purpose |
|--------|---------|
| `agent/` | Agent loop (`loop_.rs`), context builder (`context.rs`), subagent manager (`subagent.rs`) |
| `tools/` | Built-in tools: filesystem, shell, web fetch/search, spawn (subagents), message |
| `provider/` | Thin re-export layer over `mofa_sdk::llm` — no direct LLM code here |
| `channels/` | Channel integrations: Telegram, Discord, DingTalk, Feishu, WhatsApp |
| `bus/` | Async `MessageBus` (MPSC queue) decoupling channels from the agent loop |
| `session/` | Conversation history via `mofa_sdk`'s JSONL-backed `SessionManager` |
| `rbac/` | Role-Based Access Control for tools, skills, and filesystem paths |
| `cron/` | Scheduled task execution via cron expressions |
| `heartbeat/` | Periodic proactive wake-up service |
| `config.rs` | Config loading from `~/.mofaclaw/config.json` |
| `messages.rs` | `InboundMessage` / `OutboundMessage` types for bus communication |

### Key Dependency: `mofa-sdk`

LLM interaction, session storage, skills, and the agent loop core are delegated to the `mofa-sdk` crate (`mofa_sdk`). The `AgentLoop` wraps `mofa_sdk::llm::AgentLoop` and `TaskOrchestrator`. The `provider/mod.rs` re-exports `mofa_sdk::llm` types — no direct HTTP LLM calls exist in this codebase.

**IMPORTANT**: Always use MoFA framework components (`mofa-sdk`) for LLM interactions, agent creation, and tool execution. See [MoFA Framework Usage Guide](.claude/mofa-framework-usage.md) for detailed standards and patterns.

### Data Flow

```
Channel (Telegram/Discord/etc.)
    ↓ InboundMessage
  MessageBus (async MPSC)
    ↓
  AgentLoop
    → ContextBuilder (builds system prompt from workspace files)
    → mofa_sdk LLMAgent (LLM call)
    → ToolRegistry (execute tool calls)
    ↓ OutboundMessage
  MessageBus
    ↓
Channel (send reply)
```

### Workspace Files (`workspace/`)

The agent's personality and context come from markdown files in the configured workspace directory (default `~/.mofaclaw/workspace/`):
- `SOUL.md` — Agent personality/values
- `USER.md` — User profile
- `HEARTBEAT.md` — Heartbeat/proactive message instructions
- `AGENTS.md` — Subagent definitions
- `TOOLS.md` — Tool configuration
- `memory/MEMORY.md` — Persistent memory

### Skills (`skills/`)

Bundled skills live in `skills/<name>/SKILL.md` with YAML frontmatter. Workspace-local skills in `<workspace>/skills/` override bundled ones with the same name.

### RBAC

The RBAC system (`core/src/rbac/`) controls per-user access to tools and filesystem paths. Configured via `rbac` key in `~/.mofaclaw/config.json`. Channel-level `allow_from` lists provide coarse-grained access control before RBAC applies.

## Config File

`~/.mofaclaw/config.json` — loaded by `load_config()` in `core/src/config.rs`. Data directory: `~/.mofaclaw/data/`. Sessions stored as JSONL in `~/.mofaclaw/data/sessions/`.

## MoFA Framework Integration Standards

This codebase uses the **MoFA framework** (`mofa-sdk` crate) as the foundation for LLM interactions, agent management, and tool execution. All new code should integrate with MoFA components to ensure standardization and reusability.

### Core MoFA Components

#### 1. LLM Provider Setup

**Always use MoFA's provider abstractions:**

```rust
use mofa_sdk::llm::openai::{OpenAIConfig, OpenAIProvider};
use std::sync::Arc;

let openai_config = OpenAIConfig::new(&api_key)
    .with_model(&model)
    .with_base_url(api_base);  // Works with OpenRouter, vLLM, etc.
let provider = Arc::new(OpenAIProvider::with_config(openai_config));
```

**Do NOT** create custom HTTP clients for LLM calls or bypass MoFA's provider abstraction.

#### 2. Agent Creation

**Always use `LLMAgentBuilder`:**

```rust
use mofa_sdk::llm::LLMAgentBuilder;

let llm_agent = Arc::new(
    LLMAgentBuilder::new()
        .with_id("agent-id")
        .with_name("Agent Name")
        .with_provider(provider.clone())
        .with_system_prompt(system_prompt)
        .with_tool_executor(tool_executor)
        .build_async()
        .await,
);
```

#### 3. Tool Integration

**Bridge mofaclaw tools with MoFA using `ToolRegistryExecutor`:**

```rust
let tools = Arc::new(RwLock::new(ToolRegistry::new()));
// register tools...
let tool_executor = Arc::new(ToolRegistryExecutor::new(tools.clone()));
```

#### 4. Agent Loop Integration

```rust
// Standard setup
let agent = Arc::new(
    AgentLoop::with_agent_and_tools(&config, llm_agent, provider, bus, sessions, tools).await?,
);

// Role-based with overrides
let agent = Arc::new(
    AgentLoop::with_agent_and_tools_custom(
        &config, llm_agent, provider, bus, sessions, tools,
        max_iterations_override, temperature_override,
    ).await?,
);
```

### Standard Agent Setup Pattern

```rust
// 1. Provider
let provider = Arc::new(OpenAIProvider::with_config(
    OpenAIConfig::new(&api_key).with_model(&model).with_base_url(api_base)
));
// 2. Infrastructure
let bus = MessageBus::new();
let sessions = Arc::new(SessionManager::new(&config));
let tools = Arc::new(RwLock::new(ToolRegistry::new()));
// 3. Register tools
{ let mut g = tools.write().await; AgentLoop::register_default_tools(&mut g, &workspace, brave_api_key, bus.clone()); }
// 4. Build agent
let tool_executor = Arc::new(ToolRegistryExecutor::new(tools.clone()));
let system_prompt = ContextBuilder::new(&config).build_system_prompt(None).await?;
let llm_agent = Arc::new(LLMAgentBuilder::new().with_id("id").with_name("name")
    .with_provider(provider.clone()).with_system_prompt(system_prompt)
    .with_tool_executor(tool_executor).build_async().await);
let agent = Arc::new(AgentLoop::with_agent_and_tools(&config, llm_agent, provider, bus, sessions, tools).await?);
```

### Import Standards

```rust
use mofa_sdk::llm::{LLMAgentBuilder, openai::{OpenAIConfig, OpenAIProvider}};
use mofaclaw_core::provider::{OpenAIConfig, OpenAIProvider};  // re-exported
```

### Anti-Patterns to Avoid

- ❌ Custom HTTP LLM clients — use `OpenAIProvider`
- ❌ Bypassing `AgentLoop` — use `agent.process_direct()`
- ❌ Custom tool dispatch — use `ToolRegistry` + `ToolRegistryExecutor`

### Multi-Agent Systems

Each agent in a team must use `LLMAgentBuilder` + `AgentLoop::with_agent_and_tools_custom` with role-specific overrides. Share `ToolRegistry` instances where agents need the same tools.
