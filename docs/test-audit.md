# Ignored Test Audit

This document tracks the workspace's ignored or conditionally skipped tests and
records whether each one should be fixed, deleted, or kept as manual smoke
coverage.

Audit date: March 10, 2026.

## Summary

After the fixes in this branch:

- 18 Rust tests remain under `#[ignore]`
- 1 Python test remains conditionally skipped on Windows

Categories used in this audit:

- `readiness polling`: keep the test shape, but replace timing races with an
  explicit readiness check
- `dependency injection`: replace the external dependency with a fake backend,
  stub transport, or extracted pure helper
- `infra-dependent`: keep as manual smoke or move to a dedicated non-blocking
  environment
- `obsolete/delete`: coverage is duplicated or the current test shape is no
  longer worth keeping

## Fixes Applied In This Branch

### `crates/atm-daemon/tests/provider_loader_integration.rs`

- Status: fixed and unignored
- Change: build the stub provider in worktree-relative `target/debug` instead
  of `target/release`
- Reason: the old ignore reason was stale; the path and build model are
  portable enough for normal CI

### `crates/atm-daemon/src/daemon/socket.rs`

- Status: fixed and unignored
- Change: update `test_socket_server_control_stdin_roundtrip` to serialize a
  real `ControlRequest` using the current schema and restore `ATM_HOME` through
  the existing `EnvGuard` helper
- Reason: the test was broken by payload schema drift, not by infrastructure;
  once unignored it also needed the standard global env restore guard

## Remaining Ignored Tests

### `crates/atm-core/src/daemon_client.rs`

#### `test_ensure_daemon_running_restarts_identity_mismatch_daemon`

- Current reason: `smoke coverage only; exercises real subprocess and socket timing`
- Category: `readiness polling`
- Recommendation: keep deterministic correctness in the pure helper tests, and
  if this smoke test remains, replace the fixed timing assumptions with an
  explicit readiness handshake from the spawned test daemon

## `crates/atm-daemon/tests/agent_state_integration.rs`

### `test_hook_watcher_picks_up_event`

- Current reason: `requires reliable FSEvents/kqueue delivery; logic covered by hook_watcher unit tests`
- Category: `obsolete/delete`
- Recommendation: delete or rewrite around deterministic file reads instead of
  filesystem event delivery

### `test_hook_watcher_incremental_reads`

- Current reason: `requires reliable FSEvents/kqueue delivery; logic covered by hook_watcher unit tests`
- Category: `obsolete/delete`
- Recommendation: same as above; current behavior is better covered by unit
  tests plus non-ignored startup/replay integration tests

### `test_hook_watcher_full_lifecycle`

- Current reason: `requires reliable FSEvents/kqueue delivery; logic covered by hook_watcher unit tests`
- Category: `obsolete/delete`
- Recommendation: same as above; keep lifecycle proofs in deterministic state
  tests, not FSEvents delivery smoke

## `crates/atm-daemon/tests/tmux_integration.rs`

### `tmux_worker_autostarts`

- Current reason: `requires a real tmux backend; set ATM_TEST_TMUX=1 and run with --ignored`
- Category: `infra-dependent`
- Recommendation: keep ignored in default CI; optionally run in a dedicated
  tmux-enabled nightly or QA lane

### `tmux_worker_receives_message`

- Current reason: `requires a real tmux backend; set ATM_TEST_TMUX=1 and run with --ignored`
- Category: `infra-dependent`
- Recommendation: same as above

### `tmux_delivery_method_comparison`

- Current reason: `requires a real tmux backend; set ATM_TEST_TMUX=1 and run with --ignored`
- Category: `infra-dependent`
- Recommendation: same as above

## `crates/atm-daemon/tests/worker_adapter_tests.rs`

### `test_real_tmux_spawn_requires_tmux`

- Current reason: `requires active tmux server; run locally with cargo test -- --ignored`
- Category: `infra-dependent`
- Recommendation: keep as manual tmux smoke coverage only

### `test_handle_message_routes_to_agent`

- Current reason: `requires a real tmux backend; set ATM_TEST_TMUX=1 and run with --ignored`
- Category: `dependency injection`
- Recommendation: replace this with a `MockTmuxBackend` routing test that
  proves recipient resolution, worker spawn/send behavior, and inbox writeback
  without a live tmux session; keep any real tmux error-path coverage separate
  if it remains useful

## `crates/atm-agent-mcp/tests/mcp_integration.rs`

### `test_mcp_atm_send`

- Current reason: `requires live codex binary with MCP server; run manually with --ignored`
- Category: `dependency injection`
- Recommendation: convert from placeholder smoke coverage into deterministic
  transport tests using `MockTransport` or a stub child-process harness

### `test_mcp_atm_read`

- Current reason: `requires live codex binary with MCP server; run manually with --ignored`
- Category: `dependency injection`
- Recommendation: same as above

### `test_mcp_atm_broadcast`

- Current reason: `requires live codex binary with MCP server; run manually with --ignored`
- Category: `dependency injection`
- Recommendation: same as above

### `test_mcp_atm_pending_count`

- Current reason: `requires live codex binary with MCP server; run manually with --ignored`
- Category: `dependency injection`
- Recommendation: same as above

### `test_json_atm_send`

- Current reason: `requires live codex binary with --json flag; run manually with --ignored`
- Category: `dependency injection`
- Recommendation: same as above, but against the JSON transport path

### `test_json_atm_read`

- Current reason: `requires live codex binary with --json flag; run manually with --ignored`
- Category: `dependency injection`
- Recommendation: same as above

### `test_json_atm_broadcast`

- Current reason: `requires live codex binary with --json flag; run manually with --ignored`
- Category: `dependency injection`
- Recommendation: same as above

### `test_json_atm_pending_count`

- Current reason: `requires live codex binary with --json flag; run manually with --ignored`
- Category: `dependency injection`
- Recommendation: same as above

### `test_json_stdin_queue_inject`

- Current reason: `requires live codex binary with --json flag; run manually with --ignored`
- Category: `dependency injection`
- Recommendation: same as above

## Non-Rust Conditional Skip

### `crates/atm/tests/hook-scripts/test_atm_identity_write.py::test_file_permissions_unix`

- Current reason: `chmod` semantics do not apply on Windows
- Category: `infra-dependent`
- Recommendation: keep the platform guard; this is a legitimate OS-specific
  skip, not flaky ignored coverage

## Follow-Up Priorities

Recommended next steps, in order:

1. Replace the MCP placeholder ignored tests with deterministic
   `MockTransport`-based end-to-end coverage.
2. Replace `test_handle_message_routes_to_agent` with a mock-backend routing
   test.
3. Remove or rewrite the FSEvents-dependent `agent_state_integration` ignored
   tests.
4. If the daemon identity-mismatch smoke test stays, change it to explicit
   readiness polling or a handshake protocol.
