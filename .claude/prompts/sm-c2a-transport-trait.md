# Scrum-Master Prompt: Sprint C.2a — Transport Trait + McpTransport Refactor

## Context

You are a scrum-master executing Sprint C.2a for the `agent-team-mail` project.

**Project**: `/Users/randlee/Documents/github/agent-team-mail`
**Integration branch**: `integrate/phase-C`
**Sprint branch**: `feature/pC-s2a-transport-trait` (create from `integrate/phase-C`)
**Worktree**: `../agent-team-mail-worktrees/feature/pC-s2a-transport-trait`
**Crate**: `crates/atm-agent-mcp`
**Target PR**: `feature/pC-s2a-transport-trait` → `integrate/phase-C`

Read `docs/project-plan.md` Phase C section for full sprint details before starting.
Read `docs/cross-platform-guidelines.md` — mandatory for all implementation.

## Goal

Extract a `CodexTransport` trait from `proxy.rs` and wrap the existing MCP protocol
in `McpTransport`. **Zero behaviour change** — all existing tests must pass unchanged.
This creates the seam that Sprint C.2b will use to plug in `JsonTransport`.

## arch-ctm Design Notes for proxy.rs

Key file: `crates/atm-agent-mcp/src/proxy.rs` (~2300 lines)

Seam points identified by arch-ctm (use these to scope the C.2a extraction):

1. **Entry/event loop seam** (`proxy.rs:393`, routing switch `:558`): `ProxyServer::run` is orchestration only — extract dispatcher trait boundary here. This is the primary seam for `CodexTransport`.
2. **Child lifecycle seam** (`proxy.rs:2120`, reader loop `:2169`, crash cleanup `:2284`): `spawn_child` + stdout/wait tasks can be a `ChildRuntime` module. The spawn site is `Command::new(&self.config.codex_bin)` with `arg("mcp-server")`.
3. **Child message router seam** (`proxy.rs:2611`): `route_child_message` is already an isolated free fn with elicitation bridge + tools/list intercept handoff — good boundary for `Router` trait.
4. **Event enrichment seam** (`proxy.rs:2356`): `forward_event` free fn injects `agent_id` and handles backpressure counter.
5. **Tools-list interception seam** (`proxy.rs:2708`): `intercept_tools_list` is a pure transform fn — easy unit.
6. **New tracing seam** (`proxy.rs:556`): per-request `debug_span!("mcp_request", request_id=...)` — the minimal observability join point for all upstream requests. **NOTE: must use `.instrument()` not `.entered()` here — `.entered()` is `!Send` and breaks async compilation.**

**Scope for C.2a (minimal extraction)**: Focus on seams 1 and 2 only. Extract `CodexTransport` trait at the `run()` entry/event loop boundary and wrap the existing MCP child spawn logic in `McpTransport`. Do NOT reorganize the other seams — that is out of scope for a zero-behaviour-change refactor.

## Implementation Steps

1. Create worktree: `git worktree add ../agent-team-mail-worktrees/feature/pC-s2a-transport-trait -b feature/pC-s2a-transport-trait origin/integrate/phase-C`
2. Study `crates/atm-agent-mcp/src/proxy.rs` — identify spawn, read loop, write loop, shutdown
3. Create `crates/atm-agent-mcp/src/transport.rs` with the `CodexTransport` trait
4. Implement `McpTransport` wrapping existing inline protocol
5. Refactor `proxy.rs` to dispatch through `Box<dyn CodexTransport>` (or generic)
6. Add `transport = "mcp"` config field to `.atm.toml` / `AtmConfig` (default: `"mcp"`)
7. Wire C.1 `emit_event_best_effort` calls for transport init/shutdown

## Exit Criteria

- [ ] `CodexTransport` trait in `crates/atm-agent-mcp/src/transport.rs`
- [ ] `McpTransport` wraps existing protocol; no behaviour change
- [ ] Proxy dispatches through trait; `transport = "mcp"` in `.atm.toml` selects `McpTransport`
- [ ] All existing tests pass unchanged
- [ ] C.1 structured log events emitted for transport init/shutdown
- [ ] `cargo clippy --all-targets --all-features -- -D warnings` clean
- [ ] No `#[allow]` suppressions — use `#[expect(lint, reason="...")]` only

## QA

After implementation, dual QA will be run (rust-qa-agent + atm-qa-agent).
QA approval triggers immediate C.2b launch — do NOT wait for CI.

## Notes

- proxy.rs is large (~2300 lines) — read carefully before touching
- The refactor must be purely mechanical: no logic changes, only extraction
- Use `#[async_trait]` crate if needed for the async trait methods
- Keep `McpTransport` in the same file as the trait or a `transport/mcp.rs` submodule
- C.2b will add `JsonTransport` in a separate file; design the module layout with that in mind
