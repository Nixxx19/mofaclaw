# Issue: SessionManager Blocking in Async Tests

## Problem

Two tests in `core/src/tools/agent_message.rs` are currently ignored due to a SessionManager blocking issue:

1. `test_send_agent_message_tool`
2. `test_broadcast_to_team_tool`

## Root Cause

The issue occurs when `SessionManager::new()` is called within a nested async context:

1. Tests use `#[tokio::test(flavor = "multi_thread")]` (already in tokio runtime)
2. Tests create a new `Runtime::new()` and call `rt.block_on()` inside the async test
3. `SessionManager::new()` uses `tokio::task::block_in_place()` which calls `tokio::runtime::Handle::current().block_on()`
4. This creates a nested blocking scenario that can deadlock or fail

## Code Location

**File:** `core/src/tools/agent_message.rs`

```rust
#[tokio::test(flavor = "multi_thread")]
#[ignore] // TODO: Fix SessionManager blocking issue
async fn test_send_agent_message_tool() {
    let rt = Runtime::new().unwrap();
    rt.block_on(async {
        let config = Arc::new(Config::default());
        let user_bus = MessageBus::new();
        let sessions = Arc::new(SessionManager::new(&config)); // ← Blocking issue here
        // ...
    });
}
```

**File:** `core/src/session/mod.rs`

```rust
pub fn new(_config: &Config) -> Self {
    let sessions_dir = get_data_dir().join("sessions");
    let inner = tokio::task::block_in_place(|| {
        tokio::runtime::Handle::current().block_on(async move {
            // This nested block_on can deadlock in test context
            // ...
        })
    });
    // ...
}
```

## Impact

- **Tests ignored:** 2 tests cannot run
- **Functionality:** The tools themselves work fine in production
- **Test coverage:** Reduced coverage for `SendAgentMessageTool` and `BroadcastToTeamTool`

## Proposed Solutions

### Option 1: Make SessionManager async-friendly for tests

Create a test-only async constructor:

```rust
impl SessionManager {
    /// Async constructor for tests (avoids blocking)
    #[cfg(test)]
    pub async fn new_async(config: &Config) -> Self {
        let sessions_dir = get_data_dir().join("sessions");
        tokio::fs::create_dir_all(&sessions_dir).await.ok();
        
        let inner = match JsonlSessionStorage::new(&sessions_dir).await {
            Ok(storage) => MofaSessionManager::with_storage(Box::new(storage)),
            Err(_) => MofaSessionManager::with_storage(Box::new(MemorySessionStorage::new())),
        };
        
        Self { inner, sessions_dir }
    }
}
```

Then update tests to use `new_async()`:

```rust
#[tokio::test]
async fn test_send_agent_message_tool() {
    let config = Arc::new(Config::default());
    let user_bus = MessageBus::new();
    let sessions = Arc::new(SessionManager::new_async(&config).await); // ← Use async version
    // ...
}
```

### Option 2: Refactor SessionManager::new() to be fully async

Change the public API to be async (breaking change):

```rust
impl SessionManager {
    pub async fn new(config: &Config) -> Self {
        let sessions_dir = get_data_dir().join("sessions");
        tokio::fs::create_dir_all(&sessions_dir).await.ok();
        
        let inner = match JsonlSessionStorage::new(&sessions_dir).await {
            Ok(storage) => MofaSessionManager::with_storage(Box::new(storage)),
            Err(_) => MofaSessionManager::with_storage(Box::new(MemorySessionStorage::new())),
        };
        
        Self { inner, sessions_dir }
    }
}
```

**Note:** This would require updating all call sites to use `.await`.

### Option 3: Use lazy initialization

Initialize SessionManager lazily on first use:

```rust
impl SessionManager {
    pub fn new(config: &Config) -> Self {
        Self {
            inner: None, // Lazy init
            sessions_dir: get_data_dir().join("sessions"),
            config: config.clone(),
        }
    }
    
    async fn ensure_initialized(&self) {
        if self.inner.is_none() {
            // Initialize here
        }
    }
}
```

## Recommendation

**Option 1** is the safest:
- Non-breaking change
- Tests can use async version
- Production code unchanged
- Minimal impact

## Related Files

- `core/src/tools/agent_message.rs` - Tests that are ignored
- `core/src/session/mod.rs` - SessionManager implementation
- `core/src/agent/collaboration/team.rs` - Uses SessionManager

## Status

- **Priority:** Low (tests work in production, only test issue)
- **Type:** Technical debt
- **Created:** 2025-03-07
- **Related:** Part of test infrastructure, not blocking multi-agent collaboration features
