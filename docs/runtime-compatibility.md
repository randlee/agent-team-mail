# Runtime Compatibility (Gemini First, OpenCode Discovery)

Status:
- Gemini scope: accepted for docs/spec scope in R.0c (docs-only).
- OpenCode scope: discovery findings captured for review in this doc; adapter
  implementation is deferred to a follow-on sprint.

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

Proposed `atm teams spawn` signature (runtime-agnostic baseline):
- `atm teams spawn --agent <name> --team <team> --runtime <claude|codex|gemini|opencode> [--model <model>] [--cwd <path>] [--system-prompt <path>] [--sandbox <on|off>] [--approval-mode <mode>] [--include-directories <paths>] [--env KEY=VALUE ...] [--resume] [--resume-session-id <runtime_session_id>]`

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
2. wait grace window for normal exit (default: 15s, configurable)
3. if alive: `SIGINT` (wait 10s, configurable)
4. if still alive after timeout: `SIGTERM` (wait 10s, configurable)
5. if still alive: `SIGKILL`

Rationale:
- matches user requirement: request first, kill only if unresponsive
- aligns with Gemini signal-handling path

### 3.5 Lifecycle event mapping to ATM envelope

Emit ATM `hook-event` envelope with `source.kind = "agent_hook"` for Gemini hooks:
- `SessionStart` -> `session_start`
- agent idle/turn complete from stream-json/result boundary -> `teammate_idle`
- `SessionEnd` -> `session_end`

Clarification: `teammate_idle` is an existing canonical ATM lifecycle event
already defined in requirements section 4.5 (not a new event type introduced by
this doc).

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

R.0c scope is complete when:
- docs for Gemini compatibility are accepted,
- requirements/project-plan deltas are merged,
- open questions are tracked for implementation planning.

Implementation is explicitly deferred until the next runtime adapter
implementation sprint.

## 7. OpenCode Discovery Findings (Docs-Only, Pre-Adapter)

### 7.1 Verified Runtime Facts (OpenCode CLI)

#### 7.1.1 Launch and resume controls

OpenCode supports both TUI and headless paths with explicit session reuse
controls:
- default TUI command (`opencode`) accepts:
  - `--continue/-c` (continue most recent root session),
  - `--session/-s <session_id>`,
  - `--fork` (requires `--continue` or `--session`),
  - `--agent`, `--model`, `--prompt`.
- headless command (`opencode run`) accepts:
  - `--continue/-c`, `--session/-s <session_id>`, `--fork`,
  - `--agent`, `--model`, `--variant`,
  - `--format default|json`,
  - `--file/-f` (attach one or more files to the message),
  - `--title` (title for new session; truncated prompt if empty string),
  - `--attach <url>` (attach to a running `opencode serve` instance at the given URL),
  - `--dir` (directory to run in; if `--attach` is set, path on remote server),
  - `--port` (local server port; random if not specified),
  - `--thinking` (show thinking blocks in default format),
  - `--command` (run a named slash-command instead of a free-text message),
  - `--share` (share the session after completion).
- persistent server mode: `opencode serve` starts a headless HTTP server
  that accepts multiple prompt submissions via REST API (see 7.1.7).

Source refs:
- `../opencode/packages/opencode/src/cli/cmd/tui/thread.ts`
- `../opencode/packages/opencode/src/cli/cmd/run.ts`
- `../opencode/packages/opencode/src/cli/cmd/serve.ts`

#### 7.1.2 Session identity model

- Session IDs are OpenCode-native identifiers with `ses_` prefix.
- Session list ordering is by `time_updated DESC`.
- `--continue` behavior resolves latest root session (`parentID` absent).

Source refs:
- `../opencode/packages/opencode/src/id/id.ts`
- `../opencode/packages/opencode/src/session/index.ts`
- `../opencode/packages/opencode/src/cli/cmd/run.ts`

#### 7.1.3 System prompt and instruction model

OpenCode does NOT expose an `--instructions` flag or equivalent
single CLI flag analogous to Gemini's `GEMINI_SYSTEM_MD`. There is no
`--instructions` option on `opencode run` or any other subcommand.

Instead, system instruction composition is runtime-internal and includes:
- model-specific built-in prompt templates,
- environment/system context,
- discovered instruction files (including `AGENTS.md` and `CLAUDE.md`),
- optional extra instruction globs/URLs via config `instructions`.

**Instruction file discovery order** (source: `instruction.ts`):

1. Project walk-up: searches for the first of `["AGENTS.md", "CLAUDE.md",
   "CONTEXT.md"]` walking up from the current working directory to the
   worktree root. First match wins (the entire chain is NOT loaded).

2. Global instruction file: checks, in order:
   - `$OPENCODE_CONFIG_DIR/AGENTS.md` (if `OPENCODE_CONFIG_DIR` is set),
   - `$XDG_CONFIG_HOME/opencode/AGENTS.md`,
   - `~/.claude/CLAUDE.md` (unless `OPENCODE_DISABLE_CLAUDE_CODE_PROMPT=1`).
   First existing file wins.

3. Config `instructions` array (in `opencode.json`/TOML): each entry is
   treated as a glob pattern, absolute path, or HTTP/HTTPS URL. All matches
   are loaded and concatenated.

**Environment flags for instruction control:**

| Flag | Effect |
|---|---|
| `OPENCODE_CONFIG_DIR=<path>` | Override config/instruction root |
| `OPENCODE_DISABLE_PROJECT_CONFIG=1` | Skip project walk-up discovery |
| `OPENCODE_DISABLE_CLAUDE_CODE_PROMPT=1` | Skip `~/.claude/CLAUDE.md` |
| `OPENCODE_DISABLE_CLAUDE_CODE=1` | Disables all Claude Code integration |

**ATM injection strategy:** since there is no `--instructions` flag,
ATM system prompt injection for OpenCode must use one of:
- Place a generated `AGENTS.md` in the per-agent working directory
  (discovered automatically via walk-up),
- Set `OPENCODE_CONFIG_DIR` to an agent-isolated directory that contains
  an `AGENTS.md` with the desired system content,
- Use the `system` field in the `POST /session/{sessionID}/message`
  API body (the `PromptInput.system` field, applied per-turn, not globally).

**The `system` field on `PromptInput`** is a per-prompt string that
is appended to the system context for that turn only. It does not persist
across session turns. This is most suitable for per-prompt steering rather
than global agent configuration.

Source refs:
- `../opencode/packages/opencode/src/session/system.ts`
- `../opencode/packages/opencode/src/session/instruction.ts`
- `../opencode/packages/opencode/src/session/prompt.ts`
- `../opencode/packages/opencode/src/config/config.ts`
- `../opencode/packages/opencode/src/flag/flag.ts`

#### 7.1.4 Runtime state location/isolation

OpenCode state/config/data roots are derived from XDG paths with `opencode`
subdirectories:
- `$XDG_DATA_HOME/opencode`
- `$XDG_CONFIG_HOME/opencode`
- `$XDG_STATE_HOME/opencode`
- `$XDG_CACHE_HOME/opencode`

There is no single `OPENCODE_HOME` override flag equivalent to Gemini's
`GEMINI_CLI_HOME`. Isolation requires overriding the standard XDG env vars:
- `XDG_DATA_HOME`
- `XDG_CONFIG_HOME`
- `XDG_STATE_HOME`
- `XDG_CACHE_HOME`

Additionally, `OPENCODE_TEST_HOME` overrides `os.homedir()` for test isolation.
`OPENCODE_CONFIG_DIR` overrides the config directory specifically.

Source refs:
- `../opencode/packages/opencode/src/global/index.ts`
- `../opencode/packages/opencode/src/flag/flag.ts`

Inference for ATM isolation:
- Agent-isolated runtime homes require per-agent XDG env var overrides
  (all four dirs) rather than a single runtime-specific flag like Gemini.
- `OPENCODE_CONFIG_DIR` can be used as a lighter-weight override when
  only instruction/config isolation is needed (not full state isolation).

#### 7.1.5 Teardown/interrupt behavior

- API-level interrupt is explicit: `POST /session/{sessionID}/abort` ->
  `SessionPrompt.cancel(sessionID)`.
- TUI interrupt keybind defaults to `escape`; prompt UI requires repeated
  interrupt action before issuing `session.abort`.
- Worker shutdown path calls `shutdown` and aborts stream subscriptions before
  disposal.

Source refs:
- `../opencode/packages/opencode/src/server/routes/session.ts`
- `../opencode/packages/opencode/src/session/prompt.ts`
- `../opencode/packages/opencode/src/cli/cmd/tui/component/prompt/index.tsx`
- `../opencode/packages/opencode/src/cli/cmd/tui/worker.ts`
- `../opencode/packages/opencode/src/config/config.ts`

#### 7.1.6 The `--variant` flag and agent/persistent-mode clarification

**What `--variant` controls:**

`--variant` is a provider-specific reasoning effort selector. It maps to a
named entry in the model's `variants` table, which in turn expands to
provider-specific API options (e.g., `reasoningEffort`, `thinking.budgetTokens`).
The flag does NOT control agent personality, mode, or session lifetime.

**Known variant names per provider** (source: `provider/transform.ts`):

| Provider SDK | Supported variant names |
|---|---|
| `@ai-sdk/openai` | `none`, `minimal`, `low`, `medium`, `high`, `xhigh` (model-dependent) |
| `@ai-sdk/azure` | `low`, `medium`, `high` (`minimal` added for gpt-5 class) |
| `@ai-sdk/github-copilot` | `low`, `medium`, `high` (`xhigh` for o5.1-codex-max/5.2/5.3) |
| `@ai-sdk/gateway` (Anthropic) | `low`, `medium`, `high`, `max` (adaptive for claude-opus/sonnet-4.6); `high`, `max` (older Anthropic via thinking budget) |
| `@ai-sdk/gateway` (Google Gemini 2.5) | `high`, `max` (thinking budget) |
| `@openrouter/ai-sdk-provider` | `none`, `minimal`, `low`, `medium`, `high`, `xhigh` (for GPT/Gemini-3/Claude models) |
| Various OpenAI-compatible | `low`, `medium`, `high` |
| `@ai-sdk/xai` (Grok-3-mini) | `low`, `high` |
| DeepSeek, MiniMax, Mistral, Kimi | no variants (reasoning effort not supported) |

Variant names are only available for models that have `capabilities.reasoning = true`.
Silently ignored for models without reasoning capability.

**ATM adapter note:** `--variant` should be exposed as an optional
parameter in the ATM spawn/steer API for OpenCode. Pass directly to
`sdk.session.prompt({ ..., variant: args.variant })` or to
`opencode run --variant <name>`. Variant selection is per-prompt, not
per-session; any prompt can override it independently.

Source refs:
- `../opencode/packages/opencode/src/cli/cmd/run.ts` (line 291-294)
- `../opencode/packages/opencode/src/provider/transform.ts` (lines 329-504)
- `../opencode/packages/opencode/src/session/llm.ts` (lines 95-109)

**Does OpenCode have a persistent agent mode?**

`opencode run` is ALWAYS single-turn. The event loop in `run.ts` breaks
immediately when `session.status` transitions to `idle` (line 532-536 of
`run.ts`). There is no `--wait`, `--loop`, or interactive daemon mode for
`opencode run`. Each invocation:
1. Creates or resumes a session,
2. Sends one prompt (or command),
3. Streams events until the session becomes idle,
4. Exits.

For continuous/persistent agent operation, the correct model is:
- Use `opencode serve` to start a long-running headless server,
- Submit successive prompts via `POST /session/{sessionID}/message`
  (synchronous, streamed JSON response) or
  `POST /session/{sessionID}/prompt_async` (fire-and-forget),
- Alternatively, use `opencode run --attach <server-url>` to submit a
  single prompt to a running server and exit after one turn.

There is no equivalent to Claude Code's interactive agent mode where the
process waits for successive inputs. `opencode serve` is the persistent
runtime; `opencode run` and `opencode run --attach` are both single-turn
dispatch tools.

#### 7.1.7 `opencode serve` API surface

`opencode serve` starts a persistent Hono HTTP server (default port 4096,
random fallback) that exposes the full OpenCode REST API. All session
operations are available while the server runs.

**Auth:** optional `OPENCODE_SERVER_PASSWORD` + `OPENCODE_SERVER_USERNAME`
(defaults to `opencode`) enables HTTP Basic Auth on all routes. Without
the password env var, the server is unsecured.

**Key session endpoints:**

| Method | Path | Description |
|---|---|---|
| `GET` | `/session` | List all sessions (filters: `directory`, `roots`, `start`, `search`, `limit`) |
| `GET` | `/session/status` | Get status of all active sessions |
| `POST` | `/session` | Create a new session |
| `GET` | `/session/:id` | Get session metadata |
| `DELETE` | `/session/:id` | Delete a session |
| `PATCH` | `/session/:id` | Update session title or archived time |
| `POST` | `/session/:id/fork` | Fork a session at a message boundary |
| `POST` | `/session/:id/abort` | Abort an in-progress session turn |
| `POST` | `/session/:id/message` | Send a prompt; **streams** JSON response until turn completes |
| `POST` | `/session/:id/prompt_async` | Send a prompt asynchronously (returns 204 immediately) |
| `POST` | `/session/:id/command` | Send a slash-command to the session |
| `POST` | `/session/:id/shell` | Execute a shell command within session context |
| `POST` | `/session/:id/revert` | Revert to a prior message state |
| `GET` | `/session/:id/message` | List all messages in a session |
| `POST` | `/session/:id/summarize` | Compact/summarize session history |
| `POST` | `/session/:id/share` | Generate a shareable URL |

**Event streaming:**

| Method | Path | Description |
|---|---|---|
| `GET` | `/event` | SSE stream of all bus events for the current project instance |
| `GET` | `/global/event` | SSE stream of global events across all projects |

Event types emitted on the SSE stream include: `session.status`,
`message.updated`, `message.part.updated`, `session.error`,
`permission.asked`, `server.connected`, `server.heartbeat`, and others.

**Other notable endpoints:**

| Method | Path | Description |
|---|---|---|
| `GET` | `/global/health` | Health check (`{healthy: true, version: "..."}`) |
| `GET` | `/global/config` | Get/patch global config |
| `GET` | `/agent` | List configured agents |
| `GET` | `/provider` | List available providers/models |
| `GET` | `/doc` | OpenAPI spec (OpenAPI 3.1.1) |
| `PUT` | `/auth/:providerID` | Set auth credentials |

**Session lifecycle for multi-turn automation:**

1. `POST /session` → get `sessionID`
2. `GET /event` (SSE subscribe, keep open)
3. `POST /session/{id}/message` with `{parts: [{type: "text", text: "..."}], variant: "high"}` → streams response
4. Wait for `session.status` event with `status.type === "idle"` on SSE stream
5. Repeat step 3 for next turn
6. `POST /session/{id}/abort` to cancel a running turn
7. `DELETE /session/{id}` when done

**ATM control model recommendation** (answers open question 1 in 7.3):

Prefer **server/API control** over CLI-in-pane control for OpenCode:
- `opencode serve` provides a clean, stable API surface without TTY/stdin
  concerns.
- `POST /session/{id}/message` is the correct steer injection point
  (no send-keys required).
- `POST /session/{id}/abort` is the correct interrupt mechanism (no SIGINT
  to tmux pane).
- `POST /session/{id}/prompt_async` enables fire-and-forget dispatch when
  ATM does not need to wait for turn completion.
- `opencode run --attach <url>` is available as a thin single-turn dispatch
  wrapper when CLI-dispatch semantics are preferred over direct HTTP calls.
- TUI pane mode remains viable as a fallback for interactive/debug sessions,
  but should NOT be the primary ATM control path.

Source refs:
- `../opencode/packages/opencode/src/cli/cmd/serve.ts`
- `../opencode/packages/opencode/src/server/server.ts`
- `../opencode/packages/opencode/src/server/routes/session.ts`
- `../opencode/packages/opencode/src/server/routes/global.ts`
- `../opencode/packages/opencode/src/session/prompt.ts` (`PromptInput` schema)

### 7.2 Proposed ATM Design Deltas for OpenCode Adapter

1. **Preferred control model: server/API** (open question 1 answered in 7.1.7):
- Primary ATM control backend should be `opencode serve` + REST API.
- No send-keys, no TTY dependencies, no pane management required for
  normal operation.
- `opencode run --attach <url>` available as single-turn CLI dispatch wrapper.
- TUI pane retained as optional interactive/debug mode only.

2. Resume semantics mapping:
- ATM `--resume` for runtime `opencode` maps to:
  - default: `--continue`,
  - explicit runtime session override: `--session <ses_...>`.
- When using server/API control: pass `sessionID` directly to prompt endpoint.

3. System prompt mapping (open question 2 answered in 7.1.3):
- There is no `--instructions` CLI flag on any OpenCode subcommand.
- Recommended strategy: place a generated `AGENTS.md` in the per-agent
  working directory (`OPENCODE_CONFIG_DIR/<agent>/AGENTS.md`) so it is
  discovered automatically at session start.
- Alternatively, use `OPENCODE_CONFIG_DIR` to point to an agent-isolated
  directory that contains the desired `AGENTS.md`.
- Per-turn system text injection is possible via the `system` field in
  `PromptInput`, but it applies only for that turn, not globally.

4. Runtime state isolation:
- Adapter must set per-agent XDG env overrides for OpenCode process launch:
  `XDG_DATA_HOME`, `XDG_CONFIG_HOME`, `XDG_STATE_HOME`, `XDG_CACHE_HOME`.
- If only config/instruction isolation is needed, `OPENCODE_CONFIG_DIR` alone
  may suffice (lighter weight than four-var XDG isolation).

5. Teardown semantics:
- With server/API control: call `POST /session/{id}/abort` first (graceful
  abort), then process escalation (`SIGINT` -> `SIGTERM` -> `SIGKILL`)
  only if server process does not exit within grace window.
- With TUI pane (fallback): send polite ATM message, then SIGINT -> SIGTERM
  -> SIGKILL escalation.

6. Steerability semantics:
- For OpenCode server/API control: `POST /session/{id}/message` with
  `{parts: [{type: "text", text: "..."}]}` is the steer injection point.
- `POST /session/{id}/command` handles slash-commands.
- `POST /session/{id}/prompt_async` for fire-and-forget steering.
- In-turn mutation requires `POST /session/{id}/abort` first (cancel-then-steer
  semantics); no mid-turn injection is supported.

7. Variant selection:
- Expose `--variant` in ATM spawn/steer API as an optional passthrough.
- Document supported variant names per provider in ATM configuration guide.
- Note: only affects models with `capabilities.reasoning = true`; silently
  ignored otherwise.

### 7.3 Open Questions (OpenCode-Specific)

**Answered by 7.1.6 and 7.1.7 research:**

1. ~~Should ATM OpenCode adapter primarily use CLI-in-pane or server/API
   control?~~ **Answered: Server/API control (`opencode serve`) is the
   recommended primary backend.** CLI-in-pane is retained as a fallback
   for interactive/debug use only. See 7.1.7 for rationale.

2. ~~What is the preferred adapter policy for system prompt injection?~~
   **Answered: Use `AGENTS.md` file placed in the per-agent config directory
   (`OPENCODE_CONFIG_DIR`), discovered automatically at session start.** No
   `--instructions` flag exists. Per-turn `system` field is available for
   turn-scoped injection. See 7.1.3 for full decision tree.

**Still open (implementation decisions for future sprint):**

3. Should OpenCode runtime session IDs (`ses_*`) be exposed in
   `atm status --verbose` by default, or only in debug mode?
4. Should `OPENCODE_CONFIG_DIR` isolation or full four-var XDG isolation
   be the default for per-agent state isolation? The XDG approach provides
   stronger guarantees but is more complex to manage.
5. Should ATM use `POST /session/{id}/message` (synchronous, streamed) or
   `POST /session/{id}/prompt_async` (fire-and-forget) as the default steer
   mode? Synchronous mode enables turn-completion detection from the HTTP
   response without requiring a separate SSE subscription.
6. Should variant selection be per-agent (set at spawn time) or per-turn
   (overridable in each `atm send` invocation)?
