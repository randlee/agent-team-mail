# Scrum-Master Prompt: Sprint C.2b — JsonTransport + Stdin Queue + Integration Tests

## Context

You are a scrum-master executing Sprint C.2b for the `agent-team-mail` project.

**Project**: `/Users/randlee/Documents/github/agent-team-mail`
**Base branch**: `feature/pC-s2a-transport-trait` (C.2a must be QA-approved before this starts)
**Sprint branch**: `feature/pC-s2b-json-transport` (create from `feature/pC-s2a-transport-trait`)
**Worktree**: `../agent-team-mail-worktrees/feature/pC-s2b-json-transport`
**Crate**: `crates/atm-agent-mcp`
**Target PR**: `feature/pC-s2b-json-transport` → `integrate/phase-C`

Read `docs/project-plan.md` Phase C section (Sprint C.2b) for full details.
Read `docs/cross-platform-guidelines.md` — mandatory.
Read `docs/codex-json-schema.md` if it exists (created by C.2a or you).

## Goal

Implement `JsonTransport` (spawns `codex exec --json`, parses JSONL stream) and the
stdin queue for non-destructive message injection. Wire config switch. Add local-only
integration tests verifying both MCP and JSON transport modes end-to-end.

## Prerequisites (from C.2a)

- `CodexTransport` trait exists in `crates/atm-agent-mcp/src/transport.rs`
- `McpTransport` wraps existing protocol
- `transport = "mcp"` config field exists in `.atm.toml`

## Implementation Steps

### 1. JsonTransport

Create `crates/atm-agent-mcp/src/transport/json.rs` (or `json_transport.rs`):
- Spawn `codex exec --json` with piped stdin/stdout
- Parse JSONL stream line by line into typed events
- Implement `CodexTransport` trait methods
- `is_idle()` returns true when last event was `idle` type
- `recv_frame()` parses next JSONL line into `serde_json::Value`
- `send_frame()` writes JSON line + `\n` to child stdin

JSONL event types to handle: `agent_message`, `tool_call`, `tool_result`, `file_change`, `idle`, `done`

### 2. Stdin Queue

Create `crates/atm-agent-mcp/src/stdin_queue.rs`:
- Queue dir: `{ATM_HOME}/.config/atm/agent-sessions/{team}/{agent_id}/stdin_queue/`
- Atomic claim: `{uuid}.json` → `{uuid}.claimed` (rename, not copy)
- Drain trigger: on `idle` event OR 30s timeout
- TTL cleanup: delete entries older than 10 minutes on each drain
- Message format: Codex tool result JSON matching `codex exec --json` stdin format
- Identity injected in initial prompt (same as current MCP context injection)

### 3. Config Switch

Wire `transport = "json"` in `.atm.toml` to select `JsonTransport`.
Default remains `"mcp"`.

### 4. C.1 Logging

Emit `emit_event_best_effort` events for:
- Transport selected (json vs mcp)
- Each inject/idle/drain cycle
- Idle detection fires

### 5. Local Integration Tests

Create `crates/atm-agent-mcp/tests/mcp_integration.rs`:
- All tests marked `#[ignore]` (not run in CI)
- Use `codex-mini-latest` model
- Test matrix: both transport modes × all 4 ATM tools
- Run with: `cargo test --test mcp_integration -- --ignored`

Test cases:
- `test_mcp_atm_send` / `test_json_atm_send`
- `test_mcp_atm_read` / `test_json_atm_read`
- `test_mcp_atm_broadcast` / `test_json_atm_broadcast`
- `test_mcp_atm_pending_count` / `test_json_atm_pending_count`
- `test_json_stdin_queue_inject` — inject ATM message mid-session, verify it reaches Codex

## Exit Criteria

- [ ] `JsonTransport` spawns `codex exec --json`, parses all JSONL event types without panics
- [ ] `transport = "json"` in `.atm.toml` selects `JsonTransport`
- [ ] Stdin queue atomic claim works; no double-delivery under concurrent writers
- [ ] Queue drained on `idle` event or 30s timeout; TTL cleanup on drain
- [ ] ATM message injected via stdin queue reaches Codex mid-session
- [ ] Idle detection fires within 2s of Codex entering wait state
- [ ] C.1 log events emitted for every inject/idle/drain cycle
- [ ] Local integration tests: both modes × 4 ATM tools (all `#[ignore]`)
- [ ] `cargo clippy --all-targets --all-features -- -D warnings` clean
- [ ] All existing tests pass unchanged
- [ ] `docs/codex-json-schema.md` documents the event schema
- [ ] `docs/adr/003-json-mode-transport.md` written
- [ ] No `#[allow]` — use `#[expect(lint, reason="...")]` only

## QA

Dual QA (rust-qa-agent + atm-qa-agent) runs after implementation.
C.2b PR targets `integrate/phase-C`.

## Notes

- proxy.rs integration: `JsonTransport` plugs into the `Box<dyn CodexTransport>` seam from C.2a
- atm-agent-mcp writes JSON-RPC to stdout — `JsonTransport` child process stdout must NOT leak to parent stdout
- Idle detection: buffer the last N events; `is_idle()` checks if last was `idle` type
- stdin queue uses `get_home_dir()` (not `dirs::home_dir()`) — cross-platform compliance
- `async_trait` crate available from C.2a if needed
