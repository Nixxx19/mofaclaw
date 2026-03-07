# TODO: Fix SessionManager Blocking Issue in Tests

## Summary

Fix the blocking issue in `SessionManager::new()` that prevents two tests from running:
- `test_send_agent_message_tool`
- `test_broadcast_to_team_tool`

## Current Status

- [x] Issue documented
- [ ] Root cause identified
- [ ] Solution designed
- [ ] Implementation started
- [ ] Tests fixed
- [ ] Tests passing
- [ ] PR created

## Problem

`SessionManager::new()` uses `block_in_place()` → `Handle::current().block_on()` which deadlocks when called from within a nested async test context.

## Solution Approach

**Recommended:** Option 1 - Add async test constructor

1. Add `SessionManager::new_async()` for tests
2. Update tests to use async version
3. Keep production code unchanged

## Files to Modify

- [ ] `core/src/session/mod.rs` - Add `new_async()` method
- [ ] `core/src/tools/agent_message.rs` - Update tests to use `new_async()`

## Testing

After fix:
- [ ] `cargo test test_send_agent_message_tool` passes
- [ ] `cargo test test_broadcast_to_team_tool` passes
- [ ] All other tests still pass
- [ ] No regressions in production code

## Notes

- This is a test-only issue, production code works fine
- Low priority - not blocking any features
- Can be done in a separate PR

## Related

- Issue: `docs/ISSUE_SESSIONMANAGER_BLOCKING.md`
- Tests: `core/src/tools/agent_message.rs:422-480`
