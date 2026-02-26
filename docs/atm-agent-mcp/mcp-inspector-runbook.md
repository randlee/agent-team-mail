# MCP Inspector Runbook for ATM

## Prerequisites
- Node.js compatible with Inspector (see upstream README; currently Node 22+).
- `atm-agent-mcp` built in this repo.

## Repo Clone Location (local reference)
Inspector source cloned at:
- `../github/modelcontextprotocol/inspector` (relative to `agent-team-mail` repo root)

## Quick Start (Web UI)
1. Start ATM MCP server in one terminal:

```bash
export ATM_IDENTITY=arch-ctm
export ATM_TEAM=atm-dev
cargo run -p agent-team-mail-mcp -- serve
```

2. Start Inspector in another terminal:

```bash
npx @modelcontextprotocol/inspector
```

3. Open `http://localhost:6274`.
4. Configure target server as stdio command for `atm-agent-mcp`.
5. Run:
- `tools/list`
- targeted `tools/call` cases

## CLI Mode (Deterministic Smoke)
This mode is suitable for CI when restricted to MCP contract checks only.

Example pattern:

```bash
npx @modelcontextprotocol/inspector --cli cargo run -p agent-team-mail-mcp -- serve --method tools/list
```

For tool calls:

```bash
npx @modelcontextprotocol/inspector --cli cargo run -p agent-team-mail-mcp -- serve \
  --method tools/call \
  --tool-name <tool_name> \
  --tool-arg key=value
```

## Suggested ATM Smoke Matrix
- `tools/list` returns expected ATM tools.
- Invalid tool args return structured errors (no crash).
- Valid `atm_send` call path succeeds.
- Valid `atm_read` call path succeeds.
- Large input and timeout edges return bounded failures.

## CI Profile (No Codex Execution)
- Goal: validate MCP wiring and tool contracts without launching Codex execution flows.
- Allowed:
  - `tools/list`
  - basic `tools/call` cases for ATM tool surfaces with bounded payloads
- Disallowed in this gate:
  - any test that depends on running `codex exec`, `codex mcp-server`, or app-server sessions
  - long-running interactive approval/review flows
- Suggested CI sequence:

```bash
export ATM_IDENTITY=arch-ctm
export ATM_TEAM=atm-dev

# Example: list tools
npx @modelcontextprotocol/inspector --cli cargo run -p agent-team-mail-mcp -- serve --method tools/list

# Example: basic tool call
npx @modelcontextprotocol/inspector --cli cargo run -p agent-team-mail-mcp -- serve \
  --method tools/call \
  --tool-name atm_pending_count \
  --tool-arg team=atm-dev
```

## Security Defaults
- Keep Inspector auth enabled.
- Keep localhost binding unless explicitly needed.
- Do not disable auth with dangerous override flags.

## Next Integration Step
- Convert the CLI smoke matrix into a repeatable script and optionally a CI job.
