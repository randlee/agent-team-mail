/*
REFERENCE COPY (from codex-rs) for atm-agent-mcp test strategy.

Use/adapt:
- fixture payloads, MCP process harness patterns, end-to-end flow assertions

Do not use as-is:
- codex-specific constants/paths/tool names when building atm-agent-mcp tests
- assumptions that bypass atm-agent-mcp session semantics (`agent_id`, queue/state rules)
*/

// Single integration test binary that aggregates all test modules.
// The submodules live in `tests/suite/`.
mod suite;
