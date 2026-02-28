# OpenCode Execution Models (Architecture Notes)

Status: discovery notes for ATM design review (docs-only).

## 1. Executive Summary

OpenCode uses one core backend application (`Server.App()`) and multiple client
transports:
- local in-process fetch bridge (CLI/TUI default),
- local/remote HTTP server (`opencode serve` / TUI network mode),
- attach mode (`opencode attach <url>`).

Model/provider support is modular at the provider API layer (AI SDK providers +
models catalog + auth plugins), not by spawning external CLIs like Codex/Claude
or Gemini binaries.

## 2. Runtime Topology

### 2.1 Shared Backend

OpenCode’s backend surface is a Hono app built in `Server.App()` with routes
for `/session`, `/provider`, `/config`, `/mcp`, `/event`, etc.

Sources:
- `../opencode/packages/opencode/src/server/server.ts`

### 2.2 TUI Default Path (No External HTTP Required)

`opencode` (TUI command) starts a worker thread and builds an RPC client to it.
When network flags are not enabled, TUI uses:
- `url = "http://opencode.internal"` (logical base URL),
- custom `fetch` that RPC-calls `worker.fetch`,
- event source from worker RPC events.

Worker-side `fetch` executes requests via `Server.App().fetch(request)` in
process.

Sources:
- `../opencode/packages/opencode/src/cli/cmd/tui/thread.ts`
- `../opencode/packages/opencode/src/cli/cmd/tui/worker.ts`

### 2.3 TUI Network Path

If `--port`, `--hostname`, `--mdns`, or matching config enables server mode,
TUI asks worker to start `Server.listen(...)`, then talks to the returned HTTP
URL.

Sources:
- `../opencode/packages/opencode/src/cli/cmd/tui/thread.ts`
- `../opencode/packages/opencode/src/cli/network.ts`
- `../opencode/packages/opencode/src/server/server.ts`

### 2.4 Attach Path

`opencode attach <url>` connects a TUI instance directly to an existing HTTP
server and can pass basic auth via `--password`/`OPENCODE_SERVER_PASSWORD`.

Sources:
- `../opencode/packages/opencode/src/cli/cmd/tui/attach.ts`

## 3. Headless/CLI Execution Models

### 3.1 `opencode run` (Headless Prompt Runner)

`opencode run` supports:
- resume controls: `--continue`, `--session`, `--fork`,
- model/agent selectors: `--model`, `--agent`, `--variant`,
- output formats: `--format default|json`,
- optional remote attach target: `--attach <url>`.

Sources:
- `../opencode/packages/opencode/src/cli/cmd/run.ts`

### 3.2 Output Semantics (`--format default|json`)

`--format json` is a newline-delimited JSON event stream emitted from subscribed
runtime events while the turn is in progress (not a single final JSON blob).
The loop exits when `session.status` becomes `idle`.

There is no `stream-json` CLI flag equivalent; streaming is implemented by event
subscription + NDJSON writes.

Sources:
- `../opencode/packages/opencode/src/cli/cmd/run.ts`
- `../opencode/packages/opencode/src/server/server.ts` (`/event` SSE endpoint)

## 4. Session and Lifecycle Model

- Session IDs are OpenCode-native (`ses_*`).
- `--continue` resolves the latest root session (no `parentID`).
- Abort is first-class API: `POST /session/{sessionID}/abort` calls
  `SessionPrompt.cancel(sessionID)`.

Sources:
- `../opencode/packages/opencode/src/id/id.ts`
- `../opencode/packages/opencode/src/session/index.ts`
- `../opencode/packages/opencode/src/server/routes/session.ts`
- `../opencode/packages/opencode/src/session/prompt.ts`

## 5. Providers, Models, and “Modularity”

### 5.1 Provider Layer

Provider support is modular through:
- models catalog (`models.dev` + config filters),
- bundled AI SDK provider factories (`@ai-sdk/openai`, `@ai-sdk/anthropic`,
  `@ai-sdk/google`, etc.),
- optional plugin-provided auth loaders.

Sources:
- `../opencode/packages/opencode/src/provider/provider.ts`
- `../opencode/packages/opencode/src/provider/models.ts`
- `../opencode/packages/opencode/src/server/routes/provider.ts`

### 5.2 Does OpenCode Run Codex CLI?

Not in this architecture. “Codex mode” is implemented as provider/auth behavior
in the OpenCode runtime:
- Codex auth plugin obtains OpenAI OAuth tokens and targets Codex API endpoint.
- LLM path treats OpenAI OAuth sessions as `isCodex` and adjusts request options.

This is API-level integration, not spawning an external `codex` CLI process.

Sources:
- `../opencode/packages/opencode/src/plugin/codex.ts`
- `../opencode/packages/opencode/src/session/llm.ts`

### 5.3 Claude / Gemini / Others

Claude, Gemini, OpenAI, and many others are model/provider selections routed via
the provider layer and AI SDK adapters. System prompt template selection is
model-aware (e.g., `claude`, `gemini-`, `gpt-*`) but still inside the same
runtime pipeline.

Sources:
- `../opencode/packages/opencode/src/session/system.ts`
- `../opencode/packages/opencode/src/session/llm.ts`
- `../opencode/packages/opencode/src/provider/provider.ts`

## 6. Agents vs Plugins

- Agents (`build`, `plan`, `general`, etc.) are primarily config-defined
  execution profiles (permissions, prompt, model, options), with built-in
  defaults and user overrides.
- Plugins are separate extension hooks (auth/event/chat transforms/tool hooks)
  loaded from built-ins + configured plugin sources.

So, “different AI” support is mostly provider/model modularity; agents are
behavior profiles; plugins are extension points.

Sources:
- `../opencode/packages/opencode/src/agent/agent.ts`
- `../opencode/packages/opencode/src/plugin/index.ts`
- `../opencode/packages/opencode/src/config/config.ts`

## 7. ATM Integration Implications

1. Prefer a runtime adapter abstraction that separates:
   - transport (`inproc`, `http`, `attach`),
   - session control (`create`, `resume`, `abort`, `fork`),
   - provider/model selection.
2. OpenCode can be integrated without tmux stdin-steering by driving session
   APIs (`/session/*`) and event stream (`/event`).
3. Resume-by-agent in ATM should map to OpenCode session ID lookup + either:
   - `--continue` (latest root), or
   - explicit `--session <ses_...>`.
4. Teardown should call API abort first, then process escalation if needed.
