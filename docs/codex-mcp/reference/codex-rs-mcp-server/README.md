# Codex MCP Server Reference Copy

These files are copied from:
`/Users/randlee/Documents/github/codex/codex-rs/mcp-server`

Purpose: implementation reference for `atm-agent-mcp` while preserving clear guidance on what to adopt vs what to avoid.

## Use / Adapt

- `lib.rs`
  - Use task/channel layout pattern (`stdin reader` -> `processor` -> `stdout writer`).
  - Use `run_main` style startup orchestration.
- `message_processor.rs`
  - Use centralized request dispatch and explicit handlers for initialize/tools/list/tools/call.
  - Use request lifecycle bookkeeping patterns (`request_id` maps, cancellation handling).
- `outgoing_message.rs`
  - Use unified outgoing envelope + callback correlation (`request_id` -> oneshot sender).
  - Use strong serialization tests for JSON-RPC flattening behavior.
- `codex_tool_config.rs`
  - Use typed tool input structs + generated JSON schema + schema snapshot tests.
  - Use conversion layer from tool params into runtime config.
- `test_mcp_process.rs`, `test_mock_model_server.rs`
  - Use harness patterns for end-to-end MCP process testing and deterministic mock responses.

## Do Not Use As-Is

- Newline-delimited transport assumptions from `lib.rs` test/runtime flow.
  - `atm-agent-mcp` should keep explicit protocol-framing compatibility behavior.
- Codex-specific runtime internals (`ThreadManager`, `AuthManager`, codex-only event types).
  - Replace with `atm-agent-mcp` session state machine and registry.
- Codex API field names as external contract (`threadId`, `thread_id`).
  - External contract should remain `agent_id`; keep `backend_id` internal/metadata.
- Legacy env var naming from upstream examples.
  - Prefer `ATM_AGENT_MCP_*` names for this project.

## Notes

- This is a docs/reference copy only. These files are not compiled in this repo.
- Keep this folder in sync manually when upstream patterns materially change.
