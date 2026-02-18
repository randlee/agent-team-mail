# codex-mcp Requirements

> **Status**: APPROVED by team-lead + arch-ctm (2026-02-18)
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

**Goal:** A thin Rust MCP proxy that wraps `codex mcp-server`, automatically managing identity, team context, communication, and session lifecycle — making Codex a first-class ATM team member.

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

### FR-2: Identity and Context Injection

- **FR-2.1**: On every `codex` tool call, proxy MUST inject `developer-instructions` containing session context (identity, team, repo_root, repo_name, branch, cwd).
- **FR-2.2**: If caller already provides `developer-instructions`, proxy MUST append (not replace) its context.
- **FR-2.3**: If caller provides `base-instructions`, proxy MUST respect it and only inject via `developer-instructions`.
- **FR-2.4**: Proxy MUST set `cwd` to `repo_root` (or caller-supplied `cwd` if provided).
- **FR-2.5**: Identity resolution follows priority: CLI flag → env var → `[plugins.codex-mcp]` → `[core]` → default "codex".
- **FR-2.6**: Session context (branch, repo_root) MUST be refreshed on each `codex` call (not captured once at startup). Context injected into `developer-instructions` MUST reflect current state, or be explicitly labeled as "launch-time" values if refresh is impractical.
- **FR-2.7**: Per-thread `cwd` MUST be persisted in the registry so that `codex-reply` calls can restore the correct working directory for each thread.

### FR-3: Identity Collision Handling

- **FR-3.1**: On startup, proxy MUST check if resolved identity is already active using a PID-based liveness check (not just registry status).
- **FR-3.2**: If identity is active (live process), proxy MUST append a numeric suffix and log the resolved name.
- **FR-3.3**: If identity is stale (dead process), proxy MUST reclaim the identity and update the registry.
- **FR-3.4**: Proxy MUST write a PID file or lock file for its own identity to enable liveness checks by other instances.

### FR-4: ATM Communication Tools

- **FR-4.1**: Proxy MUST expose `atm_send`, `atm_read`, and `atm_broadcast` as MCP tools in the `tools/list` response.
- **FR-4.2**: `atm_send` MUST accept `to` (agent or agent@team format), `message`, and optional `summary`. The proxy parses `@` notation into separate recipient/team fields.
- **FR-4.3**: `atm_read` MUST return unread messages for this identity, with option to mark as read. Returns array of `{from, message, timestamp, message_id}`.
- **FR-4.4**: `atm_broadcast` MUST send to all team members via `atm-core`.
- **FR-4.5**: All ATM tools MUST use the proxy's resolved identity as sender — no impersonation.
- **FR-4.6**: All ATM tool calls MUST be logged to an audit trail (see FR-9).

### FR-5: Thread Registry and Persistence

- **FR-5.1**: Proxy MUST track all active threadIds in an in-memory registry, persisted to disk on every thread creation/update.
- **FR-5.2**: Registry entries MUST include: thread_id, identity, team, repo_root, repo_name, branch, cwd, started_at, last_active, status, tag.
- **FR-5.3**: Registry MUST use per-identity files (`registry.<identity>.json`) to avoid cross-instance write contention.
- **FR-5.4**: On `codex`/`codex-reply` response, proxy MUST extract and register the threadId.
- **FR-5.5**: Registry writes MUST use file locking + version-based CAS to prevent lost updates under concurrent access. Read-modify-write without lock is NOT acceptable.
- **FR-5.6**: Registry SHOULD support an append-only journal mode as alternative to RMW for high-contention scenarios.

### FR-6: Session Resume

- **FR-6.1**: `codex-mcp serve --resume` MUST resume the most recent session for this identity by prepending the saved summary to `developer-instructions` on the first turn.
- **FR-6.2**: `codex-mcp serve --resume <thread-id>` MUST resume a specific thread.
- **FR-6.3**: If no summary exists for the resumed thread (crash/SIGKILL), proxy MUST resume without summary context and log a warning.
- **FR-6.4**: Summary files written to `~/.config/atm/codex-sessions/<identity>/<thread-id>/summary.md`.

### FR-7: Graceful Shutdown

- **FR-7.1**: On SIGTERM/SIGINT, proxy MUST request a compacted summary from each active thread via `codex-reply` with a summary prompt.
- **FR-7.2**: Summary request MUST have a 10-second timeout. If timed out, persist registry with status "interrupted".
- **FR-7.3**: Proxy MUST persist final registry state, deregister identity from team, and terminate child process.
- **FR-7.4**: On parent disconnect (stdio EOF), proxy MUST treat as SIGTERM equivalent.

### FR-8: Incoming Mail Handling (Pull Model)

> **Design Decision**: Mail-as-turns (proxy autonomously injecting `codex-reply`) is rejected due to MCP protocol direction constraints. The proxy MUST NOT issue `codex`/`codex-reply` calls on its own — only the Claude client may initiate Codex turns. Instead, use a **pull model**.

- **FR-8.1**: `atm_read` tool returns pending mail. Claude/Codex decides when and how to process it.
- **FR-8.2**: Proxy MUST expose an `atm_pending_count` tool that returns the count of unread messages without reading/marking them — useful for Claude to check before deciding whether to inject mail into a Codex turn.
- **FR-8.3**: If the proxy is registered as an MCP server with notification support, it MAY send an MCP notification when new mail arrives. This is optional and depends on client support.
- **FR-8.4**: Mail content returned by `atm_read` MUST be wrapped in a structured envelope (sender, timestamp, message_id) — raw message text MUST NOT be injected directly as tool instructions to reduce prompt-injection risk.
- **FR-8.5**: `atm_read` MUST support a `max_messages` parameter (default 10) and `max_message_length` (default 4096 chars, truncate with indicator) to prevent inbox bursts from overwhelming context.
- **FR-8.6**: Messages MUST only be marked as read AFTER the MCP response containing those messages has been fully written and flushed to the client (at-least-once semantics). Mark-before-deliver risks message loss on proxy crash or client disconnect.

### FR-9: Audit Log

- **FR-9.1**: Proxy MUST log all ATM tool calls (send, read, broadcast) with timestamp, identity, recipient, and message summary.
- **FR-9.2**: Proxy MUST log all `codex`/`codex-reply` forwards with timestamp, threadId, and prompt summary (first 200 chars).
- **FR-9.3**: Audit log written to `~/.config/atm/codex-sessions/<identity>/audit.jsonl`.

### FR-10: Proxy Management MCP Tools

- **FR-10.1**: Proxy MUST expose `codex_threads` tool — returns list of active/recent threads for this identity with status, last_active, tag.
- **FR-10.2**: Proxy MUST expose `codex_status` tool — returns proxy health (child process alive, identity, team, uptime, active thread count, pending mail count).

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
- **FR-13.3**: `codex-mcp serve --resume [<thread-id>]` — resume previous session.
- **FR-13.4**: `codex-mcp config` — show resolved configuration.
- **FR-13.5**: `codex-mcp threads [--repo <name>] [--identity <name>] [--prune]` — list/manage sessions.
- **FR-13.6**: `codex-mcp summary <thread-id>` — display saved summary.

### FR-14: Request Timeouts

- **FR-14.1**: Proxy MUST support a configurable timeout per `codex`/`codex-reply` forward (default: 300s).
- **FR-14.2**: On timeout, proxy MUST cancel the downstream request if possible and return a timeout error to Claude with partial result if available.
- **FR-14.3**: Timeout is configurable via `[plugins.codex-mcp].request_timeout_secs` and CLI `--timeout`.

### FR-15: Tool Naming

- **FR-15.1**: ATM tools SHOULD use namespaced names (`atm_send`, `atm_read`, `atm_broadcast`, `atm_pending_count`) to avoid collision with future upstream Codex tools.
- **FR-15.2**: Proxy management tools SHOULD use namespaced names (`codex_threads`, `codex_status`).

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
- `atm_send` MUST NOT allow identity spoofing — always uses proxy's resolved identity.
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

- **Mail-as-turns (push model)**: Proxy autonomously injecting mail into Codex sessions. Deferred due to MCP protocol constraints. May revisit if MCP adds server→client push.
- **Cross-machine thread sharing**: Via `atm-daemon` bridge. Requires bridge to be production-ready.
- **Approval via mail**: `on-request` policy with human approval through ATM. Requires bidirectional proxy-Claude communication.
- **Subagent ATM identities**: Native Codex subagents registering as ATM team members. Complex lifecycle management.
- **Heuristic mail-to-thread routing**: Content-based routing of mail to specific threads. MVP uses most-recent-active only.

---

## 6. Open Questions

1. **Should subagents inherit the parent's ATM identity with a suffix?** (e.g., `codex-architect/worker-1`) — or should they be invisible to ATM?
2. **Should `codex-reply` calls include thread metadata in the response?** (e.g., turn count, token usage) — useful for orchestrator decisions.
3. **What is the maximum number of concurrent threads per identity?** — unbounded risks resource exhaustion.
4. **Should the proxy support multiple downstream Codex instances?** (e.g., different models) — or strictly 1:1?

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
| A.1 | **Crate scaffold + config** — workspace integration, CLI skeleton (`serve`, `config`, `threads`), config resolution from `.atm.toml` via `atm-core`, identity resolution with PID-based liveness check | atm-core config |
| A.2 | **MCP stdio proxy** — spawn `codex mcp-server` child, JSON-RPC pass-through (content-length framing, partial reads), `tools/list` interception to add synthetic tools, child health monitoring | A.1 |
| A.3 | **Context injection + ATM tools** — inject `developer-instructions` on `codex` calls (with per-call context refresh), implement `atm_send`/`atm_read`/`atm_broadcast`/`atm_pending_count` via atm-core, mail envelope sanitization, at-least-once read semantics | A.2 |
| A.4 | **Thread registry** — persist threadId on `codex`/`codex-reply` response, per-identity registry files with file lock + CAS, `codex_threads`/`codex_status` MCP tools, per-thread cwd tracking | A.3 |
| A.5 | **Shutdown + resume** — graceful shutdown with bounded summary requests (10s timeout), emergency snapshot on timeout, `--resume` flag with summary prepend, fallback for missing summaries, audit log (append-only JSONL) | A.4 |

**MVP exit criteria:**
- [ ] `codex-mcp serve` starts proxy, forwards all MCP traffic correctly
- [ ] Identity auto-resolved from `.atm.toml` with collision handling
- [ ] `developer-instructions` injected with session context on every `codex` call
- [ ] `atm_send`/`atm_read`/`atm_broadcast`/`atm_pending_count` work as MCP tools
- [ ] Thread registry persists across restarts, no lost updates under concurrent access
- [ ] `codex_threads`/`codex_status` return accurate session info
- [ ] Graceful shutdown writes summary, `--resume` restores context
- [ ] Audit log captures all tool calls with correlation IDs
- [ ] Child process crash detected and reported
- [ ] Request timeout with configurable limit
- [ ] All tests pass on macOS + Linux
- [ ] `cargo clippy -- -D warnings` clean

### Phase B: Assisted Mail Processing (2 sprints)

| Sprint | Deliverable | Dependencies |
|--------|-------------|--------------|
| B.1 | **Mail-aware orchestration helpers** — `atm_pending_count` returns unread count + sender list, optional MCP notification on new mail arrival (if client supports), mail batch size/truncation controls | Phase A |
| B.2 | **Role presets + multi-identity** — `[plugins.codex-mcp.roles.*]` config, `--role` CLI flag, role-specific model/sandbox/policy overrides, per-thread role tracking in registry | B.1 |

### Phase C: Production Hardening (2 sprints)

| Sprint | Deliverable | Dependencies |
|--------|-------------|--------------|
| C.1 | **Conformance testing** — MCP protocol conformance test suite (initialize, capabilities, notifications, cancellation, streaming), proxy latency benchmarks, registry stress tests | Phase B |
| C.2 | **Cross-platform + packaging** — Windows support (if feasible), `codex-mcp` added to release workflow, Homebrew formula update, documentation | C.1 |

---

## 9. Acceptance Test Checklist

### Proxy Core
- [ ] Start `codex-mcp serve`, verify `codex mcp-server` child spawns
- [ ] Send `codex` tool call through proxy, verify response includes threadId
- [ ] Send `codex-reply` with threadId, verify conversation continues
- [ ] Kill child process, verify next request returns error with exit code
- [ ] Send request exceeding timeout, verify timeout error returned

### Identity
- [ ] Start with `--identity foo`, verify foo used in all injected context
- [ ] Start two instances with same identity, verify second gets suffix
- [ ] Kill first instance, start third with same identity, verify it reclaims (no suffix)
- [ ] Verify PID file written and cleaned up on graceful shutdown

### ATM Tools
- [ ] Call `atm_send` via MCP, verify message appears in recipient's ATM inbox
- [ ] Call `atm_read`, verify unread messages returned with envelope metadata
- [ ] Call `atm_read`, verify messages only marked read after successful response
- [ ] Call `atm_broadcast`, verify all team members receive message
- [ ] Call `atm_pending_count`, verify correct count without marking read
- [ ] Verify `atm_send` uses proxy identity (no spoofing possible)
- [ ] Send message > max_message_length, verify truncation

### Registry
- [ ] Start session, verify registry file created with correct metadata
- [ ] Restart proxy, verify registry loaded and threads listed
- [ ] Run two instances concurrently writing to registry, verify no lost updates
- [ ] Call `codex_threads`, verify accurate listing
- [ ] Call `codex_status`, verify health info returned

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

---

## 10. Revision History

| Date | Author | Change |
|------|--------|--------|
| 2026-02-18 | team-lead + arch-ctm | Initial draft from design docs, gap analysis, and consolidated review |
