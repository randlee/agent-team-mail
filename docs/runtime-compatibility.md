# Runtime Compatibility Draft (Gemini First)

Status: draft for review, no implementation in this pass.

## 1. Scope

This document proposes ATM runtime integration behavior for **Gemini CLI** first.
Goals:
- tmux-pane launch parity with current Codex workflow
- steerability during turns (or explicit limits if runtime does not support it)
- controlled teardown (request first, force-kill fallback)
- resume-by-agent-id semantics for ATM
- clean identity mapping between ATM agent identity and Gemini session identity

Note: `../gemini` path was requested, but local repo path is `../gemini-cli` in this environment.

## 2. Verified Runtime Facts (Gemini CLI)

### 2.1 Binary and CLI flags

Verified locally:
- `gemini --version` => `0.29.0`
- `gemini --help` supports:
  - `--model/-m`
  - `--prompt/-p` (headless)
  - `--prompt-interactive/-i`
  - `--sandbox/-s`
  - `--approval-mode` (`default|auto_edit|yolo|plan`)
  - `--resume/-r`
  - `--list-sessions`
  - `--delete-session`
  - `--output-format` (`text|json|stream-json`)
  - `--experimental-acp`

Source refs:
- `../gemini-cli/packages/cli/src/config/config.ts`
- `../gemini-cli/docs/cli/cli-reference.md`

### 2.2 Session model

- Gemini sessions are auto-saved per project.
- Session storage path: `~/.gemini/tmp/<project_hash>/chats/`.
- Resume supports `--resume` with `latest`, index, or session UUID.
- CLI supports `--list-sessions` and `--delete-session`.

Source refs:
- `../gemini-cli/docs/cli/session-management.md`
- `../gemini-cli/packages/cli/src/gemini.tsx`
- `../gemini-cli/packages/cli/src/utils/sessionUtils.ts`

### 2.3 Non-interactive/stream output

- Headless mode via `-p/--prompt`.
- Structured output via `--output-format json`.
- Event stream via `--output-format stream-json`.
- Stream emits init/message/tool_use/tool_result/result/error event types.

Source refs:
- `../gemini-cli/packages/cli/src/nonInteractiveCli.ts`
- `../gemini-cli/docs/cli/cli-reference.md`

### 2.4 Hooks/lifecycle support

Gemini CLI supports hooks including:
- `SessionStart`, `SessionEnd`
- `BeforeToolSelection`, `BeforeTool`, `AfterTool`
- `BeforeAgent`, `AfterAgent`, `BeforeModel`, `AfterModel`

Source refs:
- `../gemini-cli/docs/hooks/index.md`
- `../gemini-cli/docs/hooks/reference.md`
- `../gemini-cli/packages/cli/src/gemini.tsx`

### 2.5 Signal/shutdown behavior

- Gemini CLI registers signal handlers (`SIGHUP`, `SIGTERM`, `SIGINT`) for graceful shutdown path.

Source refs:
- `../gemini-cli/packages/cli/src/utils/cleanup.ts`

### 2.6 System prompt override

Gemini supports full system prompt replacement via:
- `GEMINI_SYSTEM_MD=1|true` => `.gemini/system.md`
- `GEMINI_SYSTEM_MD=<path>` => custom system prompt file path

Source refs:
- `../gemini-cli/docs/cli/system-prompt.md`

### 2.7 Home/state isolation

Gemini supports home override:
- `GEMINI_CLI_HOME=<dir>`

Source refs:
- `../gemini-cli/packages/core/src/utils/paths.ts`
- `../gemini-cli/docs/reference/configuration.md`

## 3. Proposed ATM Design (Gemini Adapter)

### 3.1 Identity model (ATM first, runtime second)

Use two IDs:
- `atm_agent_id` (canonical ATM identity): `<agent>@<team>`
- `runtime_session_id` (Gemini session UUID)

Daemon registry record proposal:
- `team`
- `agent`
- `runtime = "gemini"`
- `process_id`
- `pane_id`
- `runtime_session_id` (Gemini UUID)
- `runtime_home` (agent-isolated `GEMINI_CLI_HOME`)
- `state`
- `updated_at`

Reasoning:
- ATM identity remains stable across runtime swaps.
- Gemini session IDs are runtime-local and mutable per resume/fresh launch.

### 3.2 Spawn model (tmux first)

Per-agent isolated home:
- `GEMINI_CLI_HOME=<ATM_HOME>/runtime/gemini/<team>/<agent>/home`

Fresh spawn (interactive pane baseline):
- `GEMINI_CLI_HOME=... GEMINI_SYSTEM_MD=<path-or-1> gemini --model <model> [--sandbox] [--approval-mode <mode>]`

Resume spawn:
- `GEMINI_CLI_HOME=... GEMINI_SYSTEM_MD=<path-or-1> gemini --resume <runtime_session_id> --model <model> [--sandbox] [--approval-mode <mode>]`

### 3.3 Steer model

Phase 1 (tmux steering):
- steer by sending prompt text to pane stdin (same operational pattern as Codex tmux worker control)
- turn observability from pane stream + Gemini hook events

Phase 2 (MCP/headless bridge):
- use `-p` + `--output-format stream-json` for structured run-event transport
- evaluate `--experimental-acp` as future interactive control transport once stability is acceptable

Important limitation:
- Gemini does not expose a documented "inject new user input into currently-running turn" API in this pass.
- Steering during a long turn is effectively: cancel + follow-up steer prompt.

### 3.4 Teardown model

Required sequence:
1. polite shutdown request (ATM message + in-pane steer text)
2. wait grace window for normal exit
3. if alive: `SIGINT`
4. if still alive after timeout: `SIGTERM`
5. if still alive: `SIGKILL`

Rationale:
- matches user requirement: request first, kill only if unresponsive
- aligns with Gemini signal-handling path

### 3.5 Lifecycle event mapping to ATM envelope

Emit ATM `hook-event` envelope with `source.kind = "agent_hook"` for Gemini hooks:
- `SessionStart` -> `session_start`
- agent idle/turn complete from stream-json/result boundary -> `teammate_idle`
- `SessionEnd` -> `session_end`

## 4. Proposed Requirements Deltas (Draft)

These are proposed requirement additions/changes for review. No code in this pass.

### R-GEM-1 Spawn Contract

ATM must support runtime-aware teammate spawn for Gemini with:
- fresh mode (system prompt enabled)
- resume mode (`--resume <runtime_session_id>`)
- per-agent `GEMINI_CLI_HOME` isolation

### R-GEM-2 Identity Contract

ATM session registry must store:
- canonical ATM identity (`team`, `agent`)
- runtime identity (`runtime_session_id`)
- runtime metadata (`runtime`, `runtime_home`, `pane_id`, `process_id`)

### R-GEM-3 Teardown Contract

`atm teams shutdown <agent>` runtime behavior for Gemini:
- polite request first
- bounded grace wait
- SIGINT -> SIGTERM -> SIGKILL escalation
- escalation events must be logged to unified log stream

### R-GEM-4 Steering Contract

ATM must define two steer modes for Gemini:
- tmux interactive steer (stdin injection into pane)
- headless JSON steer transport (`--output-format stream-json`) for MCP path

If in-turn mutation is not supported by runtime, ATM must document and enforce cancel-then-steer semantics.

### R-GEM-5 Lifecycle Hook Contract

Gemini hook/lifecycle events must map into existing ATM unified lifecycle envelope (`hook-event`) using `source.kind = "agent_hook"` and team/member validation consistent with current daemon policy.

### R-GEM-6 Observability Contract

Unified logging (`4.6`) must include runtime adapter fields for Gemini operations:
- `runtime=gemini`
- `runtime_session_id`
- `teardown_stage` (`request|sigint|sigterm|sigkill`)
- `spawn_mode` (`fresh|resume`)

### R-GEM-7 Resume-by-Agent-ID UX

ATM user-facing resume should remain agent-centric:
- `atm teams spawn --agent <name> --runtime gemini --resume`

Implementation resolves runtime session ID from ATM registry/state, not from user-provided Gemini UUID in common flow.

## 5. Open Review Items

1. Should Gemini default spawn mode be `--sandbox` on, matching conservative policy, or follow current ATM/Codex defaults?
2. Should ACP be gated behind a feature flag until we validate reliability for steer/control parity?
3. Do we want to allow explicit user override of runtime session id for emergency resume (`--resume-session-id <uuid>`) in addition to agent-based default?

## 6. Suggested Next Step (Still Docs-Only)

After review approval, update canonical docs with accepted deltas:
- `docs/requirements.md`
- `docs/project-plan.md` (Phase R planning rows for Gemini adapter)

No code changes should begin until those docs are accepted.
