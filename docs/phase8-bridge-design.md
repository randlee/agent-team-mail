# Phase 8 Bridge Plugin Design

## Purpose
The Bridge plugin synchronizes agent inbox queues across machines. It does **not** orchestrate work; it simply keeps per-agent inbox JSON files in sync and relies on `message_id` + timestamps for deduplication.

## Decided Architecture

### 1) Naming
- **Primary format**: `agent@team.hostname`.
- `hostname` is optional and defaults to local.
- Aliases are supported (e.g., `PM-MAC` → `team-lead@my-project.mac-studio`).
- **Conflict detection**: maintain a hostname registry; warn if a team name collides with a hostname.

### 2) Topology
- **Hub-spoke**.
- All queues are duplicated across machines.
- Hub is the synchronization point (not a router).

### 3) Transport (MVP)
- **SSH/SFTP** with key-based auth.
- Persistent connections via `ControlMaster`.
- Periodic sync with backoff to handle intermittent connectivity.
- **Atomic remote writes**: SFTP writes to `<file>.bridge-tmp`, then SSH `mv` to final path. Same temp+rename pattern used locally in `atm-core::io::atomic`.

### 4) Inbox File Naming
- **Local files are UNCHANGED**: `<agent>.json` remains the canonical local inbox file, written by Claude Code and `atm send`. This is never modified by the bridge.
- **Remote origin files are additive**: `<agent>.<hostname>.json` files appear alongside local inbox files, containing messages synced from remote machines.
- Each machine only writes its own origin files on remote hosts.
- **Read path** merges `<agent>.json` + all `<agent>.<hostname>.json` files in memory, sorted by timestamp.
- **Dedup** by `message_id`. Bridge assigns a `message_id` (UUID) to any message that lacks one before syncing.
- **Eventual consistency**: sequential reads of multiple origin files may see a slightly inconsistent snapshot if a sync lands mid-read. This is acceptable for a messaging system.

### 5) Bridge Plugin Responsibilities
- Watch local inbox files for changes (via `EventListener` capability for immediate push on write).
- Push new messages to remote(s) via SSH/SFTP (atomic temp+rename).
- Pull new messages from remote(s) and write locally as `<agent>.<local-hostname>.json`.
- Deduplicate by `message_id`.
- **Self-write filtering**: maintain a `HashSet<PathBuf>` of recently-written files with TTL to ignore watcher events triggered by the bridge's own writes (prevents feedback loop).

### 6) Team Config
- **Hub is source of truth**.
- Agent list owned by local team-lead.
- No cross-team authority to manage remote agents.
- Config is synced so all machines know who exists.

### 7) Auth (MVP)
- Minimal security assumptions (secure LAN/VPN).
- SSH key-based authentication.

### 8) Non-Goals
- Routing messages.
- Managing plans/tasks.
- Spinning up remote agents.
- Coordinating git merges.

### 9) Communication Patterns
- Mostly team-lead ↔ team-lead.
- Dev/QA cross-talk only for specific debugging.

### 10) Deferred
- Per-origin → single canonical merge optimization.
- HTTP/WebSocket transport.
- Advanced auth (tokens, MFA, role policies).

---

## Implementation Notes

### File Layout Example

```
~/.claude/teams/my-project/inboxes/
├── team-lead.json                  # Local inbox (Claude Code writes here — NEVER modified by bridge)
├── team-lead.win-desktop.json      # Messages synced from win-desktop
├── team-lead.linux-ci.json         # Messages synced from linux-ci
├── dev-1.json                      # Local inbox for dev-1
└── dev-1.win-desktop.json          # Messages for dev-1 from win-desktop
```

### File Merge Strategy
- On read, glob `inboxes/<agent>.json` + `inboxes/<agent>.*.json`.
- Parse each file, collect all `InboxMessage` entries.
- Dedup by `message_id` (first occurrence wins).
- Sort by `timestamp` (stable tie-breaker: `message_id`, then origin filename).
- Messages without `message_id` are included but cannot be deduplicated — bridge ensures all synced messages have one.

### Conflict Avoidance
- Never write to a file owned by another host.
- Local `<agent>.json` is owned by the local machine (Claude Code + `atm` CLI).
- `<agent>.<hostname>.json` is owned by the originating hostname's bridge.
- All remote writes use atomic temp+rename.

### Watcher Compatibility
- The daemon's `parse_event` uses `file_stem()` to extract agent names.
- For `dev-agent.mac-studio.json`, `file_stem()` returns `dev-agent.mac-studio`.
- **Fix**: Add `origin` field to `InboxEvent`. Parse filenames as `<agent>.<hostname>.json` — split on first `.` after the agent name. Fall back to `None` origin for plain `<agent>.json`.
- The `InboxEvent.agent` field must contain the normalized agent name (e.g., `dev-agent`), not the full file stem.

### CI Testing
- SSH-dependent tests gated behind `ATM_TEST_SSH=1` environment variable + feature flag.
- Mock transport trait for unit tests (no SSH required).
- CI runs mock-transport tests on all platforms; SSH integration tests only where SSH is configured.

---

## Sprint Decomposition

### Sprint 8.1 — Bridge Config + Plugin Scaffold
- Bridge plugin scaffold implementing Plugin trait (`init`/`run`/`shutdown`).
- Bridge config structs: hostname, role (hub/spoke), remotes list, sync interval.
- Hostname registry with collision detection.
- Alias resolution.
- Config parsing from `[plugins.bridge]` in `.atm.toml`.
- Unit tests for config parsing and hostname validation.

**Files**
- `crates/atm-daemon/src/plugins/bridge/mod.rs`
- `crates/atm-daemon/src/plugins/bridge/config.rs`
- `crates/atm-core/src/config/` (bridge config types)

### Sprint 8.2 — Per-Origin Read Path + Watcher Fix
- New `inbox_read_merged(team_dir, agent_name) -> Vec<InboxMessage>` in `atm-core::io::inbox`.
  - Globs `<agent>.json` + `<agent>.*.json`.
  - Merges, deduplicates by `message_id`, sorts by timestamp.
  - Backward-compatible: works with or without origin files.
- Update CLI `read.rs` to call merged reader.
- Update CLI `inbox.rs` to call merged reader.
- Update daemon watcher `parse_event` to handle `<agent>.<hostname>.json` filenames:
  - Add `origin: Option<String>` field to `InboxEvent`.
  - Normalize `agent` field to strip hostname suffix.
- Update daemon `event_loop.rs` cursor tracking (per-file cursors already work, just verify routing metadata).
- Unit + integration tests for merge, dedup, and watcher parsing.

**Files**
- `crates/atm-core/src/io/inbox.rs` (new `inbox_read_merged`)
- `crates/atm/src/commands/read.rs`
- `crates/atm/src/commands/inbox.rs`
- `crates/atm-daemon/src/daemon/watcher.rs`
- `crates/atm-daemon/src/daemon/event_loop.rs`

### Sprint 8.3 — SSH/SFTP Transport
- Transport trait: `connect`, `upload`, `download`, `list`, `rename`.
- SSH/SFTP implementation using `russh`/`ssh2` crate (evaluate at sprint start).
- `ControlMaster` connection pooling and lifecycle.
- Mock transport implementation for tests.
- Connection health check, retry with exponential backoff.
- Unit tests with mock transport.

**Files**
- `crates/atm-daemon/src/plugins/bridge/transport.rs`
- `crates/atm-daemon/src/plugins/bridge/ssh.rs`
- `crates/atm-daemon/src/plugins/bridge/mock_transport.rs`

### Sprint 8.4 — Sync Engine + Dedup
- Push cycle: watch local inbox files → SFTP new messages to remote `<agent>.<local-hostname>.json`.
- Pull cycle: download remote `<agent>.<local-hostname>.json` files → write locally.
- Atomic remote writes (temp+rename via transport trait).
- Cursor/watermark tracking to avoid re-transferring old messages.
- `message_id` assignment for messages that lack one.
- Self-write filtering (HashSet with TTL to prevent feedback loop).
- Integration tests with mock transport simulating 2-node sync.

**Files**
- `crates/atm-daemon/src/plugins/bridge/sync.rs`
- `crates/atm-daemon/src/plugins/bridge/dedup.rs`
- `crates/atm-daemon/tests/bridge_sync.rs`

### Sprint 8.5 — Team Config Sync + Hardening
- Sync team config from hub to spokes.
- Hostname registry warnings on config sync.
- Logging and operational metrics.
- Failure handling and retry policy for partial syncs.
- Retention extension: `RetentionConfig` handles per-origin files.
- `atm bridge status` / `atm bridge sync` CLI commands.
- End-to-end integration test: 3-node simulated topology with mock transport.
- Documentation and ops checklist.

**Files**
- `crates/atm-daemon/src/plugins/bridge/`
- `crates/atm/src/commands/bridge.rs`
- `crates/atm-daemon/tests/bridge_e2e.rs`
- `docs/`

---

## Dependency Graph

```
Sprint 8.1 (Config + Scaffold)
    │
    ├──→ Sprint 8.2 (Read Path + Watcher)  ──→ Sprint 8.4 (Sync Engine)
    │                                              │
    └──→ Sprint 8.3 (SSH Transport)  ─────────────┘
                                                   │
                                              Sprint 8.5 (Config Sync + Hardening)
```

- 8.2 and 8.3 can run **in parallel** after 8.1 completes.
- 8.4 depends on both 8.2 (read path) and 8.3 (transport).
- 8.5 depends on 8.4.

---

## Open Questions / Risks

1. **Timestamp consistency** across hosts (clock drift).
   - Mitigation: `message_id` is primary dedup key. Bridge assigns `message_id` to any message lacking one. Timestamp is secondary sort key only.

2. **Large inboxes**: merge across many origin files can be expensive.
   - Mitigation: periodic compaction or capped retention. Sprint 8.5 extends `RetentionConfig` to per-origin files.

3. **SSH availability on Windows**.
   - Mitigation: document requirement; Win32 OpenSSH. SSH tests gated behind feature flag.

4. **Security beyond LAN/VPN**.
   - Deferred to future phase.

5. **Alias collisions**.
   - Mitigation: require unique alias names and warn on duplicates.

6. **Event storm from bridge writes** (ARCH-CTM finding).
   - Bridge writes origin file → watcher fires → bridge's `handle_message` triggers.
   - Mitigation: self-write filtering with `HashSet<PathBuf>` + TTL. Bridge ignores events for paths it recently wrote.

7. **SFTP non-atomic writes** (ARCH-CTM finding).
   - Partial SFTP writes cause truncated JSON on remote.
   - Mitigation: write to `.bridge-tmp`, then `mv` on remote. Same pattern as local atomic writes.

8. **`message_id` is Optional** in `InboxMessage` (ARCH-CTM finding).
   - Claude Code messages may lack `message_id`.
   - Mitigation: bridge assigns UUID `message_id` to any message without one before syncing. Messages without `message_id` that are never synced are unaffected.
