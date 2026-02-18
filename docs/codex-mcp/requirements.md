# codex-mcp Requirements

> **Status**: APPROVED by team-lead + arch-ctm (2026-02-18, updated: single-proxy-multi-session model)
> **Crate**: `crates/codex-mcp`
> **Binary**: `codex-mcp`

---

## 1. Problem Statement

Codex CLI can run as an MCP server (`codex mcp-server`), exposing `codex` and `codex-reply` tools for starting and continuing agentic coding sessions. However, using Codex as a Claude subagent today requires manual setup of identity, team context, communication channels, and session persistence.

**Current pain points:**
- No automatic identity/team injection — every call must manually set context
- No persistent session management — threadIds are lost between Claude sessions
- No native ATM integration — Codex must shell out to `atm` CLI, triggering approval policy friction
- No incoming mail handling — Codex can't reactively respond to team messages
- No session resume — context is lost on shutdown/crash
- No subagent awareness — Codex's native multi-agent tools are disconnected from ATM

**Goal:** A thin Rust MCP proxy that wraps a single `codex mcp-server` child process, managing multiple concurrent Codex sessions (threads) with per-thread identity, team context, communication, and lifecycle — making one or many Codex agents first-class ATM team members through a single MCP connection.

> **Architecture Decision**: One proxy instance manages all Codex sessions. Each session has a `codex_id` (proxy-assigned, exposed to Claude) which maps internally to a Codex `threadId`. Each `codex_id` maps 1:1 to an ATM identity. The proxy owns the identity namespace — no external collision detection needed. Claude opens one MCP connection and can run N concurrent Codex agents through it.
>
> **Naming Convention**: `codex_id` is the MCP tool parameter Claude uses to reference sessions. Internally, the proxy maps `codex_id` → Codex `threadId`. This avoids collision with Claude Code's `agentId` (Task tool) namespace.

---

## 2. Actors

| Actor | Description |
|-------|-------------|
| **Claude** | MCP client (orchestrator). Sends `codex`/`codex-reply` tool calls. |
| **codex-mcp** | MCP proxy server. Intercepts, augments, and forwards requests. |
| **codex mcp-server** | Downstream Codex MCP server (child process). |
| **Codex subagents** | Native subagents spawned by Codex via `spawn_agent`. |
| **ATM team members** | Other agents (Claude teammates, humans, CI) communicating via ATM. |

---

## 3. Functional Requirements

### FR-1: MCP Proxy Pass-Through

- **FR-1.1**: Proxy MUST forward all standard MCP requests/responses between Claude and `codex mcp-server` without modification, except for intercepted tool calls listed below.
- **FR-1.2**: Proxy MUST correctly handle JSON-RPC framing (content-length headers, partial reads, interleaved messages).
- **FR-1.3**: Proxy MUST handle `codex mcp-server` process lifecycle (spawn on startup, terminate on shutdown, detect crashes).

### FR-2: Per-Thread Identity and Context Injection

> **Design Decision**: Identity is per-session, not per-proxy. Each `codex` call specifies (or defaults) an identity. The proxy maintains a 1:1 mapping of codex_id→identity and enforces uniqueness.

- **FR-2.1**: On every `codex` tool call, proxy MUST inject `developer-instructions` containing session context (identity, team, repo_root, repo_name, branch, cwd). Identity is determined from the caller's `codex` parameters (see FR-2.5).
- **FR-2.2**: If caller already provides `developer-instructions`, proxy MUST append (not replace) its context.
- **FR-2.3**: If caller provides `base-instructions`, proxy MUST respect it and only inject via `developer-instructions`.
- **FR-2.4**: Proxy MUST set `cwd` to `repo_root` (or caller-supplied `cwd` if provided).
- **FR-2.5**: Identity for a new session is determined by: explicit `identity` parameter in the `codex` call → proxy default from config (`[plugins.codex-mcp].default_identity`) → "codex". The proxy MUST reject a `codex` call that requests an identity already bound to an active session (return error with the conflicting `codex_id`).
- **FR-2.6**: Session context (branch, repo_root) MUST be refreshed on each `codex` call (not captured once at startup). Context injected into `developer-instructions` MUST reflect current state, or be explicitly labeled as "launch-time" values if refresh is impractical.
- **FR-2.7**: Per-thread `cwd` MUST be persisted in the registry so that `codex-reply` calls can restore the correct working directory for each thread.
- **FR-2.8**: On `codex-reply`, proxy MUST look up the `codex_id` in the registry to resolve the bound identity. ATM tools called within that session use that identity automatically.

### FR-3: Identity Namespace Management

> **Design Decision**: The proxy owns the identity namespace for all threads it manages. Since there is one proxy instance per MCP connection, identity uniqueness is guaranteed in-process — no PID files, liveness checks, or collision suffixes needed.

- **FR-3.1**: Proxy MUST maintain an in-memory map of identity→codex_id. A `codex` call requesting an identity already bound to an active session MUST be rejected with an error indicating the conflict.
- **FR-3.2**: On startup, proxy MUST load the persisted registry and mark all previously-active threads as "stale" (since the previous proxy process is gone). Stale threads may be resumed via `--resume` or their identities reused by new threads.
- **FR-3.3**: Proxy MUST support a `max_concurrent_threads` config (default: 10) to prevent unbounded resource consumption.
- **FR-3.4**: When a thread completes or is explicitly closed, its identity MUST be released and available for reuse by a new `codex` call.

### FR-4: ATM Communication Tools

- **FR-4.1**: Proxy MUST expose `atm_send`, `atm_read`, and `atm_broadcast` as MCP tools in the `tools/list` response.
- **FR-4.2**: `atm_send` MUST accept `to` (agent or agent@team format), `message`, and optional `summary`. The proxy parses `@` notation into separate recipient/team fields.
- **FR-4.3**: `atm_read` MUST return unread messages for this identity, with option to mark as read. Returns array of `{from, message, timestamp, message_id}`.
- **FR-4.4**: `atm_broadcast` MUST send to all team members via `atm-core`.
- **FR-4.5**: All ATM tools MUST use the calling thread's bound identity as sender — no impersonation. ATM tools called outside a thread context (e.g., from Claude directly via MCP) MUST require an explicit `identity` parameter; if omitted, the call MUST be rejected with an error.
- **FR-4.6**: All ATM tool calls MUST be logged to an audit trail (see FR-9).

### FR-5: Thread Registry and Persistence

- **FR-5.1**: Proxy MUST track all active threadIds in an in-memory registry, persisted to disk on every thread creation/update.
- **FR-5.2**: Registry entries MUST include: codex_id, thread_id (Codex native), identity, team, repo_root, repo_name, branch, cwd, started_at, last_active, status, tag.
- **FR-5.3**: Registry MUST use a single file (`registry.json`) since the proxy is the sole writer. Atomic writes (via `atm-core`) prevent corruption on crash, but no file locking or CAS is needed.
- **FR-5.4**: On `codex`/`codex-reply` response, proxy MUST extract the Codex `threadId`, assign a `codex_id`, and register the mapping.
- **FR-5.5**: Registry MUST be persisted atomically on every state change (thread create, update, close) to survive proxy crashes.

### FR-6: Session Resume

- **FR-6.1**: `codex-mcp serve --resume` MUST resume the most recent session for this identity by prepending the saved summary to `developer-instructions` on the first turn.
- **FR-6.2**: `codex-mcp serve --resume <thread-id>` MUST resume a specific thread.
- **FR-6.3**: If no summary exists for the resumed thread (crash/SIGKILL), proxy MUST resume without summary context and log a warning.
- **FR-6.4**: Summary files written to `~/.config/atm/codex-sessions/<identity>/<thread-id>/summary.md`.

### FR-7: Graceful Shutdown

- **FR-7.1**: On SIGTERM/SIGINT, proxy MUST request a compacted summary from each active thread via `codex-reply` with a summary prompt.
- **FR-7.2**: Summary request MUST have a 10-second timeout. If timed out, persist registry with status "interrupted".
- **FR-7.3**: Proxy MUST persist final registry state, deregister all thread identities from team, and terminate child process.
- **FR-7.4**: On parent disconnect (stdio EOF), proxy MUST treat as SIGTERM equivalent.

### FR-8: Incoming Mail Handling (Automatic Turn Injection)

> **Design Decision**: The MCP protocol constraint (server cannot push to client) applies to the Claude→proxy direction. However, the proxy owns the `codex mcp-server` child process and CAN initiate `codex-reply` calls to it directly. Mail delivery uses automatic turn injection at the proxy→Codex boundary.

**Mail delivery triggers:**

- **FR-8.1**: **Post-turn mail check** — When a Codex turn ends (proxy receives the response from `codex mcp-server`), proxy MUST check for unread mail addressed to the thread's bound identity. If mail exists, proxy MUST automatically issue a `codex-reply` with the mail content, starting a new Codex turn.
- **FR-8.2**: **Idle mail delivery** — If no Codex turn is active for a thread's identity and mail arrives, proxy MUST automatically start a new Codex turn via `codex-reply` with the mail content. The proxy polls for new mail on a configurable interval (default: 5s) when threads are idle.
- **FR-8.3**: **Mail routing** — Mail is always delivered to the thread bound to the addressed identity (1:1 mapping). No heuristic routing. If no thread is bound to the addressed identity, mail remains unread in the ATM inbox.

**Mail content handling:**

- **FR-8.4**: Mail content injected into `codex-reply` MUST be wrapped in a structured envelope (sender, timestamp, message_id) — raw message text MUST NOT be injected directly as tool instructions to reduce prompt-injection risk.
- **FR-8.5**: Mail injection MUST support a `max_messages` parameter (default 10) and `max_message_length` (default 4096 chars, truncate with indicator) to prevent inbox bursts from overwhelming context.
- **FR-8.6**: Messages MUST only be marked as read AFTER the `codex-reply` containing those messages has been successfully sent to the child process (at-least-once semantics).

**Turn serialization (per-thread):**

- **FR-8.9**: Each thread MUST enforce a **single-flight rule**: only one `codex-reply` (from any source — Claude, auto-mail, resume) may be in-flight at a time per thread.
- **FR-8.10**: If a Claude-initiated `codex-reply` arrives while an auto-mail turn is in-flight, the Claude request MUST be queued (FIFO). Auto-mail injection MUST NOT start if a Claude request is queued or in-flight for that thread.
- **FR-8.11**: Turn source priority: Claude-initiated > auto-mail. If both are pending when a turn completes, Claude's request is dispatched first; auto-mail waits for the next idle window.

**Delivery acknowledgment:**

- **FR-8.12**: "Successfully sent" (FR-8.6) means: the `codex-reply` JSON-RPC request has been written to the child's stdin AND the proxy has recorded the request-id in its in-memory turn tracker. Messages are marked read only after both conditions are met.
- **FR-8.13**: On proxy crash between send and mark-read, messages remain unread (at-least-once). On restart, proxy MUST detect unacked mail (still marked unread) and re-deliver on next idle cycle. Duplicate delivery is acceptable; Codex agents MUST tolerate replayed mail (message_id enables dedup at the agent level).

**Pull model (supplementary):**

- **FR-8.7**: Proxy MUST still expose `atm_read` and `atm_pending_count` as MCP tools for Claude to explicitly check/read mail when needed (e.g., before deciding whether to start a new thread).
- **FR-8.8**: Auto-injection (FR-8.1/8.2) MUST be configurable and can be disabled per-thread or globally via `[plugins.codex-mcp].auto_mail = false`.

### FR-9: Audit Log

- **FR-9.1**: Proxy MUST log all ATM tool calls (send, read, broadcast) with timestamp, identity, recipient, and message summary.
- **FR-9.2**: Proxy MUST log all `codex`/`codex-reply` forwards with timestamp, codex_id, and prompt summary (first 200 chars).
- **FR-9.3**: Audit log written to `~/.config/atm/codex-sessions/audit.jsonl` (single proxy-wide log). Each entry includes `codex_id` and `identity` fields for per-session filtering. Per-identity views are derived, not stored separately.

### FR-10: Proxy Management MCP Tools

- **FR-10.1**: Proxy MUST expose `codex_threads` tool — returns list of all active/recent threads with their bound identity, status, last_active, tag.
- **FR-10.2**: Proxy MUST expose `codex_status` tool — returns proxy health (child process alive, team, uptime, active thread count, identity→thread mapping, aggregate pending mail count).

### FR-11: Codex Process Health

- **FR-11.1**: Proxy MUST detect child process (`codex mcp-server`) crashes and report error to Claude on next request.
- **FR-11.2**: Proxy MUST NOT auto-restart the child process. Return an error indicating the child died, with the exit code/signal.
- **FR-11.3**: Claude can decide to restart by closing and re-opening the MCP connection.

### FR-12: Configuration

- **FR-12.1**: Plugin config in `.atm.toml` under `[plugins.codex-mcp]` — codex_bin, identity, model, reasoning_effort, sandbox, approval_policy, prompt files.
- **FR-12.2**: Role presets in `[plugins.codex-mcp.roles.<name>]` — model, sandbox, approval_policy overrides.
- **FR-12.3**: Config resolution: CLI flags → env vars → repo-local `.atm.toml` → global `~/.config/atm/config.toml` → defaults.

### FR-13: CLI Interface

- **FR-13.1**: `codex-mcp serve` — start MCP server (stdio mode).
- **FR-13.2**: `codex-mcp serve --identity <name> --role <preset>` — with overrides.
- **FR-13.3**: `codex-mcp serve --resume [<codex-id>]` — resume previous session.
- **FR-13.4**: `codex-mcp config` — show resolved configuration.
- **FR-13.5**: `codex-mcp threads [--repo <name>] [--identity <name>] [--prune]` — list/manage sessions.
- **FR-13.6**: `codex-mcp summary <codex-id>` — display saved summary.

### FR-14: Request Timeouts

- **FR-14.1**: Proxy MUST support a configurable timeout per `codex`/`codex-reply` forward (default: 300s).
- **FR-14.2**: On timeout, proxy MUST cancel the downstream request if possible and return a timeout error to Claude with partial result if available.
- **FR-14.3**: Timeout is configurable via `[plugins.codex-mcp].request_timeout_secs` and CLI `--timeout`.

### FR-15: Tool Naming

- **FR-15.1**: ATM tools SHOULD use namespaced names (`atm_send`, `atm_read`, `atm_broadcast`, `atm_pending_count`) to avoid collision with future upstream Codex tools.
- **FR-15.2**: Proxy management tools SHOULD use namespaced names (`codex_threads`, `codex_status`, `codex_close`).

### FR-16: Session Initialization Modes

> **Design Decision**: A `codex` call starts a new thread. The caller MUST specify one of three initialization modes that determine how the Codex agent is prompted.

- **FR-16.1**: **Agent prompt file** — caller provides a file path (e.g., `.claude/agents/rust-dev.md`). Proxy reads the file and injects its contents as the agent's `prompt` (or `base-instructions`). This mirrors Claude Code's agent frontmatter pattern.
- **FR-16.2**: **Inline prompt** — caller provides arbitrary text as the `prompt` parameter. Proxy forwards it directly. Used when Claude constructs a task-specific prompt at runtime.
- **FR-16.3**: **Session resume** — caller provides a `codex_id` (optionally with a continuation `prompt`). This maps to a `codex-reply` under the hood. The proxy restores the session's bound identity, cwd, and context from the registry. If a saved summary exists, it is prepended to the continuation prompt.
- **FR-16.4**: The `codex` tool schema MUST include: `identity` (optional, string), `prompt` (required unless `codex_id` provided), `agent_file` (optional, file path — mutually exclusive with `prompt`), `codex_id` (optional — if present, treat as resume), `role` (optional — selects a role preset), `cwd` (optional).
- **FR-16.5**: If both `agent_file` and `prompt` are provided, proxy MUST return an error (mutually exclusive).
- **FR-16.6**: If `agent_file` is provided, proxy MUST verify the file exists and is readable before forwarding. Return a clear error if not found.

### FR-17: Thread Lifecycle and State Machine

> **Design Decision**: Each thread has a well-defined lifecycle with explicit states. The proxy enforces state transitions and exposes a `codex_close` tool for explicit shutdown.

**States:**

```
[new] ──codex──► busy ──turn ends──► idle ──codex-reply/mail──► busy ──► idle ──► ...
                                      │                                    │
                                      ├── codex_close ──► closed           ├── codex_close ──► closed
                                      │                     │              │
                                      │                     ├── resume (codex w/ thread_id) ──► busy
                                      │                     └── new agent same identity (codex w/o thread_id) ──► busy (new threadId)
                                      │
                                      └── mail arrives ──► busy (auto-injection)
```

- **FR-17.1**: Thread states are: `busy` (Codex turn in progress), `idle` (turn complete, waiting for next input), `closed` (removed from service — identity released, summary persisted if available).
- **FR-17.2**: State transitions:
  - `→ busy`: First `codex` call forwarded to child (thread created)
  - `busy → idle`: Child returns response (turn complete). Proxy checks for mail (FR-8.1).
  - `idle → busy`: New `codex-reply` from Claude, mail auto-injection, or explicit resume
  - `idle → closed`: Explicit `codex_close` call, or proxy shutdown
  - `busy → closed`: Explicit `codex_close` call (cancels in-progress turn with timeout)
- **FR-17.3**: Proxy MUST expose `codex_close` as an MCP tool. Parameters: `codex_id` or `identity` (one required). Behavior: if thread is `busy`, request graceful shutdown (summary prompt with 10s timeout), then close. If thread is `idle`, close immediately. Releases identity for reuse. Equivalent to ending a teammate session.
- **FR-17.4**: `codex_close` on a `busy` thread SHOULD attempt to get a summary before closing (same as graceful shutdown in FR-7). If timeout, close without summary and persist registry with status "interrupted".
- **FR-17.5**: Thread state MUST be tracked in the registry and reported by `codex_threads` and `codex_status` tools.
- **FR-17.6**: Auto mail injection (FR-8.1/8.2) MUST only trigger when thread is `idle`. If thread is `busy`, mail waits until the turn completes.

**Resume and replacement after close:**

- **FR-17.7**: A `closed` session MAY be resumed by calling `codex` with `codex_id`. This restores the session's identity binding, cwd, and context from the registry (same as FR-16.3). The session transitions back to `busy`.
- **FR-17.8**: After a thread is `closed`, Claude MAY start a **new** thread with the same identity by calling `codex` without `thread_id`. This creates a fresh threadId with clean context, binding the same identity. The old thread remains in the registry as `closed` for history/audit.
- **FR-17.9**: `codex_close` MUST be idempotent — closing an already-closed thread is a no-op (returns success).

**Close/cancel/queue precedence:**

- **FR-17.10**: `codex_close` takes precedence over all other operations. When close is requested:
  1. If thread is `idle`: discard any queued auto-mail, close immediately.
  2. If thread is `busy`: cancel the in-flight turn (with summary timeout per FR-17.4), discard queued requests, then close.
  3. Any queued Claude requests for the closed thread MUST return an error indicating the thread was closed.
- **FR-17.11**: Precedence order for thread operations: `close` > `cancel` (timeout) > Claude-initiated turn > auto-mail turn. This ordering is deterministic and MUST be enforced by the proxy's per-thread command queue.

---

## 4. Non-Functional Requirements

### NFR-1: Performance
- Proxy latency overhead MUST be < 10ms per request (JSON parse + forward).
- Mail polling (if implemented) MUST NOT block MCP request/response flow.

### NFR-2: Reliability
- Proxy MUST survive child process crashes without itself crashing.
- Registry persistence MUST use atomic writes (via `atm-core`) to prevent corruption.

### NFR-3: Security
- Proxy MUST NOT expose credentials or API keys in logs or audit trail.
- `atm_send` MUST NOT allow identity spoofing — always uses the calling thread's bound identity.
- Shell injection via tool parameters MUST be prevented (no shell execution for ATM tools).

### NFR-4: Compatibility
- MUST work with Codex CLI v0.103+ MCP server protocol.
- MUST work on macOS and Linux. Windows support is stretch goal.
- MUST integrate with existing `atm-core` config and IO primitives.

### NFR-5: Observability
- Structured logging via `tracing` crate.
- Log levels: ERROR (crashes, data loss), WARN (stale identity, timeout), INFO (session start/stop, mail delivery), DEBUG (MCP traffic), TRACE (raw bytes).

---

## 5. Out of Scope (Future)

- **Mail-as-turns to Claude**: Proxy pushing mail upstream to Claude via MCP. Deferred due to MCP protocol constraints (server cannot push to client). Mail-as-turns to Codex (proxy→child) IS supported (FR-8).
- **Cross-machine thread sharing**: Via `atm-daemon` bridge. Requires bridge to be production-ready.
- **Approval via mail**: `on-request` policy with human approval through ATM. Requires bidirectional proxy-Claude communication.
- **Subagent ATM identities**: Native Codex subagents registering as ATM team members. Complex lifecycle management.
- **Multi-thread mail routing**: Content-based routing of mail to specific threads. With 1:1 identity→thread binding, mail is always delivered to the thread bound to the addressed identity. No heuristics needed.

---

## 6. Open Questions

1. **Should subagents inherit the parent's ATM identity with a suffix?** (e.g., `codex-architect/worker-1`) — or should they be invisible to ATM?
2. **Should `codex-reply` calls include thread metadata in the response?** (e.g., turn count, token usage) — useful for orchestrator decisions.
3. ~~**What is the maximum number of concurrent threads per identity?**~~ — **RESOLVED**: `max_concurrent_threads` config (default: 10) per FR-3.3.
4. ~~**Should the proxy support multiple downstream Codex instances?**~~ — **RESOLVED**: Single `codex mcp-server` child, multiple concurrent threads via threadId. Different roles/models configured per thread via role presets.

---

## 7. Dependencies

| Dependency | Version | Purpose |
|------------|---------|---------|
| `atm-core` | workspace | Config, IO, schema, inbox operations |
| `tokio` | 1.x | Async runtime for stdio proxy + timers |
| `serde_json` | 1.x | JSON-RPC message parsing |
| `signal-hook` | 0.3.x | SIGTERM/SIGINT handling |
| `tracing` | 0.1.x | Structured logging |
| `clap` | 4.x | CLI argument parsing |
| `uuid` | 1.x | Request ID generation |

---

## 8. Implementation Plan

### Phase A: MVP — Proxy + ATM Tools + Registry (4-5 sprints)

| Sprint | Deliverable | Dependencies |
|--------|-------------|--------------|
| A.1 | **Crate scaffold + config** — workspace integration, CLI skeleton (`serve`, `config`, `threads`), config resolution from `.atm.toml` via `atm-core`, default identity + role preset resolution | atm-core config |
| A.2 | **MCP stdio proxy** — spawn `codex mcp-server` child, JSON-RPC pass-through (content-length framing, partial reads), `tools/list` interception to add synthetic tools, child health monitoring | A.1 |
| A.3 | **Context injection + ATM tools** — per-thread identity binding on `codex` calls, inject `developer-instructions` (with per-call context refresh), implement `atm_send`/`atm_read`/`atm_broadcast`/`atm_pending_count` via atm-core with thread-bound identity routing, mail envelope sanitization, at-least-once read semantics, auto mail injection (post-turn + idle polling) | A.2 |
| A.4 | **Session registry** — persist codex_id→identity mapping on `codex`/`codex-reply` response (codex_id maps to Codex threadId internally), single registry file with atomic writes, `codex_threads`/`codex_status`/`codex_close` MCP tools, per-session cwd tracking, `max_concurrent_threads` enforcement, lifecycle state machine | A.3 |
| A.5 | **Shutdown + resume** — graceful shutdown with bounded summary requests (10s timeout), emergency snapshot on timeout, `--resume` flag with summary prepend, fallback for missing summaries, audit log (append-only JSONL) | A.4 |

**MVP exit criteria:**
- [ ] `codex-mcp serve` starts proxy, forwards all MCP traffic correctly
- [ ] Per-thread identity binding with uniqueness enforcement
- [ ] `developer-instructions` injected with session context on every `codex` call
- [ ] `atm_send`/`atm_read`/`atm_broadcast`/`atm_pending_count` work as MCP tools
- [ ] Thread registry persists across restarts with atomic writes
- [ ] Multiple concurrent threads with different identities work correctly
- [ ] `codex_threads`/`codex_status` return accurate session info
- [ ] Graceful shutdown writes summary, `--resume` restores context
- [ ] Audit log captures all tool calls with correlation IDs
- [ ] Child process crash detected and reported
- [ ] Request timeout with configurable limit
- [ ] All tests pass on macOS + Linux
- [ ] `cargo clippy -- -D warnings` clean

### Phase B: Role Presets + Advanced Orchestration (2 sprints)

| Sprint | Deliverable | Dependencies |
|--------|-------------|--------------|
| B.1 | **Role presets** — `[plugins.codex-mcp.roles.*]` config, per-thread role selection via `codex` call parameter, role-specific model/sandbox/policy overrides, per-thread role tracking in registry | Phase A |
| B.2 | **Advanced mail orchestration** — MCP notification on new mail arrival (if client supports), mail priority/urgency hints, per-thread auto-mail enable/disable, mail delivery metrics and diagnostics | B.1 |

### Phase C: Production Hardening (2 sprints)

| Sprint | Deliverable | Dependencies |
|--------|-------------|--------------|
| C.1 | **Conformance testing** — MCP protocol conformance test suite (initialize, capabilities, notifications, cancellation, streaming), proxy latency benchmarks, registry stress tests | Phase B |
| C.2 | **Cross-platform + packaging** — Windows support (if feasible), `codex-mcp` added to release workflow, Homebrew formula update, documentation | C.1 |

---

## 9. Acceptance Test Checklist

### Proxy Core
- [ ] Start `codex-mcp serve`, verify `codex mcp-server` child spawns
- [ ] Send `codex` tool call through proxy, verify response includes `codex_id`
- [ ] Send `codex-reply` with `codex_id`, verify conversation continues
- [ ] Kill child process, verify next request returns error with exit code
- [ ] Send request exceeding timeout, verify timeout error returned

### Identity (Per-Thread)
- [ ] Start thread with explicit identity "arch-ctm", verify identity used in injected context and ATM tools
- [ ] Start second thread with identity "dev-1", verify both threads active with separate identities
- [ ] Attempt to start third thread with identity "arch-ctm" (duplicate), verify error returned with conflicting `codex_id`
- [ ] Close first thread, start new thread with "arch-ctm", verify identity reuse succeeds
- [ ] Start thread without explicit identity, verify proxy default identity used
- [ ] Verify `codex-reply` on thread automatically resolves correct bound identity

### ATM Tools
- [ ] Call `atm_send` via MCP, verify message appears in recipient's ATM inbox
- [ ] Call `atm_read`, verify unread messages returned with envelope metadata
- [ ] Call `atm_read`, verify messages only marked read after successful response
- [ ] Call `atm_broadcast`, verify all team members receive message
- [ ] Call `atm_pending_count`, verify correct count without marking read
- [ ] Verify `atm_send` uses thread's bound identity (no spoofing possible)
- [ ] Send message > max_message_length, verify truncation

### Automatic Mail Injection
- [ ] Codex turn ends, unread mail exists for thread identity → verify proxy auto-issues `codex-reply` with mail
- [ ] Codex thread idle, mail arrives → verify proxy auto-starts new turn with mail content within poll interval
- [ ] Mail addressed to identity with no bound thread → verify mail stays unread (not delivered)
- [ ] Verify mail envelope wraps content (sender, timestamp, message_id) — no raw injection
- [ ] Verify messages marked read only after `codex-reply` successfully sent to child
- [ ] Set `auto_mail = false` → verify mail NOT auto-injected (only available via `atm_read`)
- [ ] Burst of 15 messages → verify only `max_messages` (10) delivered per turn, remainder on next cycle

### Registry
- [ ] Start session, verify registry file created with correct metadata and identity→thread mapping
- [ ] Restart proxy, verify registry loaded, stale threads marked, and threads listed
- [ ] Start 3 threads with different identities, verify registry contains all 3 with correct bindings
- [ ] Exceed `max_concurrent_threads`, verify error returned
- [ ] Call `codex_threads`, verify accurate listing with per-thread identity
- [ ] Call `codex_status`, verify health info with identity→thread mapping

### Thread Lifecycle
- [ ] Start thread → verify state is `busy` during turn, transitions to `idle` when response received
- [ ] Send `codex-reply` to idle thread → verify transitions to `busy`, back to `idle` on response
- [ ] Call `codex_close` on idle thread → verify immediate close, identity released, state `closed`
- [ ] Call `codex_close` on busy thread → verify summary attempted (10s timeout), then closed
- [ ] Call `codex_close` on already-closed thread → verify idempotent success
- [ ] Verify `codex_threads` reports correct state for each thread
- [ ] Mail arrives while thread busy → verify mail waits, auto-injected only after turn completes (idle)
- [ ] Close session, then resume with `codex` + `codex_id` → verify context restored, identity rebound, busy→idle
- [ ] Close session, then start NEW session with same identity (no `codex_id`) → verify new codex_id, clean context, identity rebound

### Shutdown + Resume
- [ ] SIGTERM proxy, verify summary requested (within 10s), registry persisted
- [ ] SIGKILL proxy, verify registry persisted without summary
- [ ] Start with `--resume`, verify summary prepended to first turn
- [ ] Start with `--resume` after SIGKILL (no summary), verify graceful fallback

### Context Injection
- [ ] Verify `developer-instructions` contains identity, team, repo, branch
- [ ] Change branch between two `codex` calls, verify second call has updated branch
- [ ] Provide `developer-instructions` in caller's `codex` call, verify proxy appends (not replaces)
- [ ] Provide `base-instructions` in caller's `codex` call, verify proxy only uses `developer-instructions`

### Session Initialization
- [ ] Start thread with `agent_file: ".claude/agents/rust-dev.md"`, verify file contents used as prompt
- [ ] Start thread with `agent_file` pointing to nonexistent file, verify clear error returned
- [ ] Start thread with inline `prompt`, verify text forwarded to Codex
- [ ] Start thread with both `agent_file` and `prompt`, verify mutual-exclusion error
- [ ] Resume session with `codex_id` + continuation prompt, verify identity/cwd restored from registry
- [ ] Resume session with `codex_id` that has saved summary, verify summary prepended
- [ ] Resume session with `codex_id` that has no summary (crash), verify graceful fallback

---

## 10. Revision History

| Date | Author | Change |
|------|--------|--------|
| 2026-02-18 | team-lead + arch-ctm | Initial draft from design docs, gap analysis, and consolidated review |
| 2026-02-18 | team-lead | Single-proxy-multi-session architecture: per-thread identity, no collision handling, simplified registry |
| 2026-02-18 | team-lead | FR-16: Session initialization modes (agent file, inline prompt, thread resume) |
| 2026-02-18 | team-lead | FR-8 rewrite: auto mail-as-turns (proxy→Codex), deterministic 1:1 identity routing, idle delivery |
| 2026-02-18 | team-lead | FR-17: Thread lifecycle state machine (created→busy→idle→closed), codex_close tool |
| 2026-02-18 | team-lead + arch-ctm | Address 3 blockers: turn serialization (FR-8.9-8.11), delivery ack (FR-8.12-8.13), close/cancel precedence (FR-17.10-17.11). Fix FR-4.5, FR-9.3 per nits. |
| 2026-02-18 | team-lead | Rename MCP tool parameter from thread_id to codex_id to avoid collision with Claude Code agentId namespace |
