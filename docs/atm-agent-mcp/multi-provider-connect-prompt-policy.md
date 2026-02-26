# Multi-Provider Connect and Prompt Policy (Draft)

## Purpose
Define a consistent contract for multi-provider MCP connectivity (`codex`, `gemini`), per-instance tool exposure, prompt input handling, and urgent ATM message interruption behavior.

## Dynamic Tool Surface
- `tools/list` MAY expand dynamically based on provider availability and active connections.
- Provider connect tools SHOULD be surfaced as:
  - `codex.connect`
  - `gemini.connect`
- `gemini.connect` may initially return a structured `not_implemented` error until runtime support lands.
- After a successful connect for ATM identity `<agent-name>`, session-scoped tools SHOULD be exposed under provider+identity namespace, for example:
  - `codex.<agent-name>.reply`
  - `codex.<agent-name>.status`
  - `codex.<agent-name>.interrupt`

## Connect Semantics
- `*.connect` MUST bind the session to an ATM identity (`atm_name`).
- On successful connect, the proxy MUST:
  - ensure inbox/mailbox exists for `atm_name` (create if missing),
  - upsert team roster entry with the resulting `agent_id`,
  - return explicit status fields (`mailbox_created|existing`, `roster_updated|unchanged`, `agent_id`).
- Connect SHOULD fail if roster/session persistence fails (avoid partially bound state).

## Prompt Input Contract
- `connect` and `reply` SHOULD accept prompt input in one of three mutually exclusive forms:
  - text,
  - JSON payload,
  - full-path file input.
- If multiple prompt forms are provided in one request, the call MUST fail with a validation error.

## Model Contract
- `connect` MAY specify model override.
- `reply` inherits session model by default.
- `reply` model changes SHOULD require explicit opt-in (for example `allow_model_switch=true`) and policy allowance.

## System Prompt Composition
Suggested prompt layering order:
1. Base Codex prompt: `gpt-5.2-codex_prompt.md`
2. Optional collaboration layer when `experimental=true`: `templates/collab/experimental_prompt.md`
3. ATM template layer (identity/team/runtime behavior), including injected variables:
   - `agent_name`
   - `team_name`
   - `team_lead_alias`

### System Prompt Mode Controls
- `connect` SHOULD support `system_prompt_mode` with values:
  - `default`
  - `override`
  - `skip`
- If `system_prompt_mode` is omitted, `default` MUST be applied implicitly.
- `override` SHOULD accept exactly one prompt source (`system_prompt` inline text or `system_prompt_file` full path).
- `skip` SHOULD require explicit opt-in (for example `allow_prompt_skip=true`) to prevent accidental unsafe runs.

## ATM Message Injection
- Auto-injected ATM read notifications SHOULD use a stable envelope prefix:
  - `[ATM MESSAGE FROM <sender> <priority>]`
- The ATM template layer SHOULD document that notification injection is automatic.

## Urgent Message Policy
- `priority=urgent` SHOULD trigger interruption handling with safe-point guardrails:
  - interrupt immediately at safe boundaries,
  - defer if currently in non-interrupt-safe operation (patch apply, migration, lock-sensitive write),
  - resume urgent injection at next safe checkpoint.
- Deferred urgent handling SHOULD emit a short status record indicating deferral reason and pending urgent source.
