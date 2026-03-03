# agent-team-mail (`atm`) — Requirements Document

**Version**: 0.3
**Date**: 2026-02-25
**Status**: Draft

---

## 1. Project Summary

`atm` is a Rust workspace that provides mail-like messaging for Claude agent teams. It consists of a CLI for interactive use, a shared library for safe file I/O against the `~/.claude/teams/` file structure, and (post-MVP) an always-on daemon that hosts plugins for CI monitoring, cross-machine bridging, issue tracking, and human chat interfaces.

### Goals

- Thin, well-tested CLI over the existing agent-team file-based API
- Shared library (`atm-core`) with atomic writes, conflict detection, and schema versioning
- Plugin architecture in the daemon — complex behaviors without bloating the core
- Provider-agnostic design (GitHub, Azure DevOps, GitLab, Bitbucket)

### Non-Goals (MVP)

- Daemon / background process mode (post-MVP)
- Team or agent lifecycle management (create/delete teams, spawn agents)
- Cross-machine networking in core (plugin responsibility)
- GUI or TUI interface
- Plugin implementations (MVP delivers the trait + registry, not concrete plugins)

---

## 2. Architecture Overview

### 2.1 Workspace Structure

```
agent-team-mail/
├── Cargo.toml                  # workspace root
├── crates/
│   ├── atm-core/               # shared library
│   │   ├── Cargo.toml
│   │   └── src/
│   │       ├── lib.rs
│   │       ├── schema/         # JSON schema types with versioning
│   │       ├── io/             # atomic swap, file locking, conflict detection
│   │       ├── config/         # .atm.toml parsing, env vars, resolution
│   │       └── context/        # SystemContext, RepoContext, GitProvider
│   ├── atm/                    # CLI binary
│   │   ├── Cargo.toml
│   │   └── src/
│   │       ├── main.rs
│   │       └── commands/       # send, read, broadcast, inbox, teams, etc.
│   └── atm-daemon/             # daemon binary (post-MVP)
│       ├── Cargo.toml
│       └── src/
│           ├── main.rs
│           ├── plugin/         # Plugin trait, registry, context
│           └── plugins/        # Built-in plugin implementations
└── docs/
```

### 2.2 Crate Responsibilities

```
┌─────────────┐   ┌──────────────┐
│   atm CLI   │   │  atm-daemon  │
│  (binary)   │   │   (binary)   │
│             │   │              │
│  No plugins │   │ Plugin host  │
│  Sync I/O   │   │ Always-on    │
│  clap args  │   │ Async/tokio  │
└──────┬──────┘   └──────┬───────┘
       │                 │
       └────────┬────────┘
                │
       ┌────────▼────────┐
       │    atm-core     │
       │  (library crate)│
       │                 │
       │  Schema types   │
       │  Atomic swap    │
       │  File locking   │
       │  Conflict detect│
       │  Config parsing │
       │  SystemContext   │
       └────────┬────────┘
                │
                ▼
        ~/.claude/teams/
        ~/.claude/tasks/
```

| Crate | Role | Async? |
|-------|------|--------|
| `atm-core` | Schema types, file I/O, config, context | No (sync I/O with atomic ops) |
| `atm` | CLI binary, command dispatch, output formatting | No (calls atm-core sync functions) |
| `atm-daemon` | Plugin host, inbox watchers, event loop | Yes (tokio, async plugin trait) |

### 2.3 File-Based API

`atm` operates directly on these files (no subprocess calls to `claude`):

| Path | Purpose |
|------|---------|
| `~/.claude/teams/{team}/config.json` | Team config: name, members, metadata |
| `~/.claude/teams/{team}/inboxes/{agent}.json` | Per-agent message inbox (JSON array) |
| `~/.claude/tasks/{team}/` | Task list files |

Reference: [`docs/agent-team-api.md`](./agent-team-api.md) for full schema details.

---

## 3. Core Library (`atm-core`)

### 3.1 Schema Types with Versioning

All JSON types use **permissive deserialization with round-trip preservation**:

```rust
#[derive(Serialize, Deserialize)]
pub struct InboxMessage {
    pub from: String,
    pub text: String,
    pub timestamp: String,
    pub read: bool,
    #[serde(default)]
    pub summary: Option<String>,

    /// Unique message ID for deduplication (UUID, assigned at creation).
    /// Messages from Claude Code won't have this field — only atm-originated
    /// messages include it. Used to prevent duplicate delivery on retry.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message_id: Option<String>,

    /// Captures fields added in newer Claude Code versions.
    /// Preserved on round-trip to avoid data loss.
    #[serde(flatten)]
    pub unknown_fields: HashMap<String, serde_json::Value>,
}
```

**Schema version detection** at startup:

- Detect Claude Code version via `claude --version`
- Cache result in `~/.config/atm/claude-version.json` (re-detect periodically)
- Map version to known schema characteristics
- When unknown fields appear: log warning, continue working, preserve on write
- When required fields are missing: log error, best-effort recovery

```rust
pub enum SchemaVersion {
    /// Pre-release (2.x) — may change without notice
    PreRelease { claude_version: String },
    /// Post-release (3.x+) — stable, breaking changes unlikely
    Stable { claude_version: String },
    /// Unknown — best effort with latest known schema
    Unknown,
}
```

**Round-trip guarantee**: Reading a file and writing it back preserves all fields, including those `atm` doesn't understand. This prevents `atm` from silently stripping fields added by newer Claude Code versions.

### 3.2 Atomic Swap with Conflict Detection

**Note on Claude Code's locking behavior**: We have NOT been able to inspect Claude Code's
actual file I/O implementation (it is closed-source / bundled). We observed that no `.lock`
sidecar files appear next to inbox files, but Claude Code may use `flock()` at the fd level
which leaves no disk trace. Our strategy must work regardless of whether Claude locks or not.

**Design principle — guaranteed delivery**: Messages must never be silently lost. If a write
cannot complete immediately (file locked, I/O error, conflict), the message is queued for
retry. The CLI retries inline with backoff; the daemon retries persistently from its
outbound queue.

**Write strategy — optimistic concurrency with atomic swap:**

```
1. Try flock(inbox.lock, LOCK_EX | LOCK_NB)   — non-blocking attempt
2. If lock acquired → proceed to step 4
3. If EWOULDBLOCK (file locked by another process):
     CLI:    sleep with backoff (50ms, 100ms, 200ms), retry up to 5 times
             If all retries fail → queue to outbound spool, exit with warning
     Daemon: queue to outbound spool, retry on next cycle
4. Read inbox.json → compute content hash
5. Modify in memory (append message, update read flags)
6. Write new version to inbox.tmp, fsync
7. atomic_swap(inbox.json, inbox.tmp)  — platform-specific:
     macOS:  renamex_np(RENAME_SWAP)
     Linux:  renameat2(RENAME_EXCHANGE)
8. Read displaced file (now at inbox.tmp) → compute hash
9. If hash differs from step 4:
     → Concurrent write detected between our read and swap
     → Merge: extract messages from inbox.tmp missing in our version
     → Re-apply swap with merged content
10. flock(LOCK_UN)                     — release lock
11. Optional: watch inbox.json for ~100ms (kqueue/inotify)
     → If overwritten within window, re-insert our message
12. Delete inbox.tmp
```

**Platform-specific atomic swap:**

| OS | Syscall | Available Since |
|----|---------|-----------------|
| macOS | `renamex_np(from, to, RENAME_SWAP)` | macOS 10.12 |
| Linux | `renameat2(AT_FDCWD, from, AT_FDCWD, to, RENAME_EXCHANGE)` | Kernel 3.15 |

**Conflict outcomes:**

| Scenario | Detection | Recovery |
|----------|-----------|----------|
| atm CLI vs atm CLI | flock prevents | N/A — serialized |
| atm CLI vs atm-daemon | flock prevents | N/A — serialized |
| atm vs Claude Code | Hash mismatch after swap | Merge displaced file, re-swap |
| Claude overwrites atm | Post-swap file watch | Re-insert message |
| File locked by external process | EWOULDBLOCK on flock | Backoff + retry, then spool |
| I/O error (disk full, permissions) | Write failure | Queue to spool, report error |

### 3.3 Guaranteed Delivery and Outbound Spool

Messages that cannot be delivered immediately are written to a local spool for retry:

```
~/.config/atm/spool/
├── pending/
│   ├── 1739284800-agent-a@team-1.json    # timestamped message files
│   └── 1739284805-agent-b@team-2.json
└── failed/                                # messages that exceeded max retries
```

**CLI behavior**: On write failure, the CLI writes the message to the spool directory and
exits with a warning: `Message queued for delivery (could not write to inbox immediately)`.
The next `atm` invocation or the daemon picks it up.

**Daemon behavior**: The daemon drains the spool directory on a regular interval (e.g., every
5 seconds). Messages in `pending/` are retried with exponential backoff. After max retries
(configurable, default 10), messages move to `failed/` and a warning is logged. The daemon
is the primary retry mechanism — it is always running and will eventually deliver.

**Delivery guarantees:**

| Component | Guarantee | Mechanism |
|-----------|-----------|-----------|
| CLI | At-least-once (best effort) | Inline retry with backoff, then spool |
| Daemon | At-least-once (persistent) | Spool drain loop with exponential backoff |
| Spool | Durable | Files on disk, survive process restart |

**Duplicate detection**: Since at-least-once delivery can produce duplicates (e.g., message
written but conflict-merge re-inserts), each message gets a unique `message_id` (UUID)
assigned at creation time. The inbox append logic skips messages with IDs already present
in the inbox.

**Public API:**

```rust
/// Atomically append a message to an inbox with conflict detection.
/// On lock contention or I/O failure, returns Queued with spool path.
pub fn inbox_append(
    team: &str,
    agent: &str,
    message: &InboxMessage,
) -> Result<WriteOutcome, InboxError>;

pub enum WriteOutcome {
    /// Clean write, no conflicts
    Success,
    /// Concurrent write detected and merged automatically
    ConflictResolved { merged_messages: usize },
    /// Could not write immediately, message spooled for later delivery
    Queued { spool_path: PathBuf },
}

/// Drain the outbound spool, retrying pending messages.
/// Returns count of successfully delivered and still-pending messages.
pub fn spool_drain() -> Result<SpoolStatus, InboxError>;
```

### 3.3 Shared System Context

Resolved once at startup, shared across all consumers:

```rust
pub struct SystemContext {
    pub hostname: String,
    pub platform: Platform,               // macOS, Linux, Windows
    pub claude_root: PathBuf,             // ~/.claude/
    pub root: PathBuf,                    // current workspace root (always present)
    pub claude_version: String,           // "2.1.39"
    pub schema_version: SchemaVersion,
    pub repo: Option<RepoContext>,
    pub default_team: String,
}

pub struct RepoContext {
    pub name: String,                     // "agent-team-mail"
    pub path: PathBuf,                    // /Users/randlee/.../agent-team-mail
    pub remote_url: Option<String>,       // raw git remote URL
    pub provider: Option<GitProvider>,    // detected from remote URL
}

/// Provider-agnostic git host identification.
/// Core only parses the remote URL. Auth, API clients, and
/// provider-specific features are plugin responsibilities.
pub enum GitProvider {
    GitHub { owner: String, repo: String },
    AzureDevOps { org: String, project: String, repo: String },
    GitLab { namespace: String, repo: String },
    Bitbucket { workspace: String, repo: String },
    Unknown { host: String },
}
```

Provider detection is purely URL parsing — no network calls, no auth. Plugins consume `ctx.system.repo.provider` and handle everything provider-specific (tokens, API clients, rate limits).

**Root vs repo distinction**:
- `root` is always present and represents the workspace root where the CLI/daemon is running (may be a non-git directory).
- `repo` is optional and only present when a git repository is detected under `root`.
- Plugins and commands must treat these as distinct concepts (e.g., CI monitor requires `repo`, but other tooling may operate on `root` without git).

---

## 4. CLI Requirements (`atm`)

### 4.1 Command Structure

```
atm <command> [options]

Commands:
  send        Send a message to an agent
  request     Send a message and wait for a response (polling)
  broadcast   Send a message to all team members
  read        Read messages from an inbox
  inbox       List inbox summary (message counts, unread)
  teams       List teams on this machine (and manage members)
  members     List agents in a team
  status      Show team status overview
  doctor      Run daemon/team health diagnostics
  config      Show/set configuration
  cleanup     Apply retention policies
  mcp         MCP server setup and management
  init        Install/check ATM hook wiring for Claude Code

Teams subcommands:
  teams add-member <team> <agent> [--agent-type <type>] [--model <model>] [--cwd <path>] [--inactive]
  teams join <agent> [--team <team>] [--agent-type <type>] [--model <model>] [--folder <path>]
  teams spawn --agent <name> --team <team> --runtime <claude|codex|gemini|opencode> [...]
  teams resume <team> [message] [--force] [--kill] [--session-id <id>]
  teams cleanup <team> [agent] [--force]
  teams backup <team> [--json]
  teams restore <team> [--from <path>] [--dry-run] [--skip-tasks] [--json]

MCP subcommands:
  mcp install <client> [scope] [--binary <path>]
  mcp uninstall <client> [scope]
  mcp status

Init command:
  init <team> [--local] [--identity <name>] [--skip-team] [--check]
```

### 4.2 Messaging Commands

#### `atm send`

Send a message to a specific agent on a team.

```
atm send <agent> <message>
atm send <agent>@<team> <message>
atm send <agent> --file <path>       # message from file (reference-only)
atm send <agent> --stdin             # message from stdin
```

**Behavior**:
- Uses `atm-core::inbox_append()` (atomic swap with conflict detection)
- Sets `read: false`, `timestamp` to current UTC, `from` to configured identity
- Generates a `summary` from the first ~100 chars of message content

**Addressing**:
- `<agent>` alone resolves to the default team
- `<agent>@<team>` specifies an explicit team (cross-team messaging)
- Namespace-qualified addresses for cross-computer/plugin routing must be
  accepted and routed when configured by transport plugins. ATM core must treat
  namespace suffixes as routable address components, not invalid identifiers.
- Agent name must exist in team's `config.json` members array

**Options**:

| Flag | Description |
|------|-------------|
| `--team <name>` | Override default team (alternative to `@team` syntax) |
| `--summary <text>` | Explicit summary instead of auto-generated |
| `--offline-action <text>` | Custom call-to-action text for offline recipients (see below) |
| `--json` | Output result as JSON |
| `--dry-run` | Show what would be written without writing |

**Offline recipient detection**:

Before writing to the inbox, `atm send` queries daemon session state (`query_session_for_team`):
- If a session record exists and `alive=false`, the recipient is considered offline.
- If session is missing (`None`) or query fails (`Err`), recipient state is unknown — **no offline warning** is shown.
- When offline, `atm` prepends a call-to-action tag to the message body: `[{action_text}] {original_message}`
- The sender receives a warning: `Warning: Agent X appears offline. Message will be queued with call-to-action.`
- The message is still delivered (written to inbox file) — the warning is informational, not a hard block.

**Agent activity tracking (daemon-managed)**:

The daemon tracks agent activity by monitoring inbox file changes and message timestamps:
- `atm send` sets the sender's `isActive: true` and `lastActive` timestamp in team `config.json` as a heartbeat.
- The daemon watches inbox file events (already part of the event loop) and tracks last-activity-per-agent from `from` fields and `timestamp` values — no extra I/O beyond existing file watching.
- After a configurable inactivity timeout (default: 5 minutes), the daemon sets `isActive: false` for the agent.
- Two activity signals: (1) messages sent by the agent (`from` field across inboxes), (2) messages read by the agent (`read: true` transitions).
- `lastActive` is stored in the member entry in `config.json` (ISO 8601 timestamp).

**Call-to-action text precedence** (highest to lowest):
1. `--offline-action "custom text"` CLI flag
2. `offline_action` property in config file (`.atm.toml` or `settings.json`)
3. Hardcoded default: empty string (`""`, no auto-tagging)

**Special case**: If the resolved action text is an empty string (property exists but value is `""`), the call-to-action is skipped entirely — no brackets prepended, message sent as-is. This allows users to explicitly opt out of auto-tagging.

**File path policy**:
- `--file <path>` is always treated as a reference (never embed file content in inbox JSON).
- The path must be inside the current repo root by default.
- Cross-repo file passing is not allowed unless explicitly permitted by repo settings.
- File access rules must be resolved from Claude Code settings with the same precedence used by Claude Code:
  managed policy → CLI args → `.claude/settings.local.json` → `.claude/settings.json` → `~/.claude/settings.json`.
- If a repo-local `.claude/settings.local.json` or `.claude/settings.json` exists, honor its file access rules.
- If the destination repo does not permit the source path, `atm` must copy the file to a local share folder and update the message to reference the new path, with an explicit note that the path was rewritten and is a copy.

**Example message (path rewritten to share copy)**:
```
[atm] File path rewritten to a local share copy for destination access.
Original: /Users/randlee/project/secrets/trace.txt
Copy: ~/.config/atm/share/backend-ci-team/trace.txt
```

#### `atm request`

Send a message from one mailbox to another and wait for a response by polling the sender inbox.
This is a temporary CLI convenience and will be replaced by a daemon-backed watcher.

```
atm request <from> <to> <message>
atm request <from> <to> <message> --timeout 30 --poll-interval 200
atm request <from> <to> <message> --from-team <team> --to-team <team>
```

**Behavior**:
- Requires explicit sender and destination mailboxes (name@team or explicit `--from-team` / `--to-team`)
- Adds a `Request-ID` marker to the message
- Polls the sender inbox for a response containing that marker
- Times out after the specified interval

#### `atm broadcast`

Send a message to all agents in a team.

```
atm broadcast <message>
atm broadcast --team <name> <message>
```

**Behavior**:
- Iterates all members in team `config.json`
- Calls `atm-core::inbox_append()` for each agent
- Reports per-agent delivery status

#### `atm read`

Read messages from an inbox.

```
atm read                         # read own inbox (unread messages)
atm read <agent>                 # read specific agent's inbox
atm read <agent>@<team>          # read inbox on specific team
atm read --all                   # read all messages (not just unread)
```

**Behavior**:
- Reads the target inbox JSON file via `atm-core`
- Default visibility uses seen-state + unread union:
  - with `since-last-seen` enabled (default), shows messages where `read == false` **or** `timestamp > last_seen`
  - with `--no-since-last-seen`, shows only unread messages (`read: false`)
- Marks displayed messages as `read: true` (atomic write back)
- `--no-mark` flag to read without marking
- Updates local seen-state to the maximum timestamp of **displayed** messages (unless disabled), never from hidden/filtered messages

**Options**:

| Flag | Description |
|------|-------------|
| `--all` | Show all messages, not just unread |
| `--since-last-seen` | Enable seen-state filtering (default) |
| `--no-since-last-seen` | Disable seen-state filtering; show unread-only behavior |
| `--no-mark` | Don't mark messages as read |
| `--no-update-seen` | Don't update local seen-state watermark after reading |
| `--limit <n>` | Show only last N messages |
| `--since <timestamp>` | Show messages after timestamp |
| `--json` | Output as JSON |
| `--from <name>` | Filter by sender |
| `--as <name>` | Reader identity override for own-inbox reads |

**Identity resolution**:
- When an explicit `<agent>` argument is provided, it is resolved through the same roles → aliases → literal pipeline as `atm send`.
- When reading your own inbox (no agent argument), identity resolution order is:
  1. `--as <name>` (explicit reader identity)
  2. `ATM_IDENTITY`
  3. `.atm.toml [core].identity` when it resolves to a concrete team member
- If identity remains unresolved, `atm read` must fail with an actionable error and must not silently default to `human`.

#### `atm inbox`

Show inbox summary without reading full messages.

```
atm inbox                        # summary for default team
atm inbox --team <name>          # summary for specific team
atm inbox --all-teams            # summary across all teams
```

**Output example**:
```
Team: backend-ci-team

  Agent              Unread  Total  Latest
  ──────────────────────────────────────────
  team-lead            3      12    2m ago
  ci-fix-agent         0       5    1h ago
  code-reviewer        1       8    15m ago
```

### 4.3 Discovery Commands

#### `atm teams`

List all teams on this machine.

```
atm teams
```

**Output**: Teams found under `~/.claude/teams/`, showing name, member count, and creation date.

#### `atm members`

List agents in a team.

```
atm members                      # default team
atm members <team>               # specific team
```

**Output**: Agent name, type, model, active status (from `config.json`).

#### `atm status`

Combined overview of a team.

```
atm status                       # default team
atm status <team>                # specific team
```

**Output**: Team info, member list with activity, unread message counts, pending tasks.

#### `atm teams add-member`

Add a member to a team roster with mailbox bootstrap guarantees.

```
atm teams add-member <team> <agent> [--agent-type <type>] [--model <model>] [--cwd <path>] [--inactive]
```

**Required behavior**:
- Add/update the member entry in `config.json`.
- Create `inboxes/<agent>.json` as part of the same add-member operation using the
  same atomic write path used for inbox creation elsewhere.
- The command must be idempotent: re-running add-member for an existing member
  must not corrupt or truncate an existing inbox.
- Command completion is not successful unless roster and mailbox converge together
  (member exists in `config.json` and inbox file exists).

**Acceptance checks**:
- Immediately after add-member returns success, `inboxes/<agent>.json` exists.
- First `atm send <agent>@<team>` succeeds without requiring a bootstrap side effect.
- `atm doctor` reports no roster/mailbox drift for a newly added member.

### 4.3.1 Lifecycle Teardown and Cleanup Semantics

Daemon-managed teammate shutdown and cleanup MUST follow one canonical flow so that
team roster (`config.json`) and mailbox (`inboxes/<agent>.json`) do not drift.

**Primary shutdown protocol**:
- Daemon sends a structured `shutdown_request` control message to the target agent mailbox.
- Daemon waits for graceful exit up to `--timeout` while monitoring PID/session liveness.
- If the process exits within timeout, daemon proceeds to teardown cleanup.
- If still alive after timeout, daemon force-kills PID using backend/platform-appropriate
  termination, then proceeds to teardown cleanup after death is confirmed.
- Mailbox deletion MUST NOT be used as a primary shutdown signal.

**Teardown cleanup invariant (REQUIRED)**:
- Roster removal and mailbox deletion are coupled operations and MUST converge together.
- For an agent in terminal state (`already terminated` or `killed after timeout`), daemon
  MUST:
  1. remove the member entry from team `config.json`, and
  2. delete `inboxes/<agent>.json`.
- A partial result (only roster removed or only mailbox deleted) is a failure state and
  MUST be retried/reconciled by daemon until converged.

**Team-lead teardown semantics (REQUIRED)**:
- `team-lead` is role-special and MUST NOT be treated as a standard removable teammate in
  automatic teardown cleanup.
- Doctor/cleanup logic MUST distinguish lead-session recovery from teammate removal:
  lead teardown drift must route to lead re-registration/recovery guidance, not generic
  member cleanup/removal guidance.
- Lead mailbox absence or stale lead session MUST NOT trigger automated roster deletion of
  `team-lead`; recovery flows must preserve team ownership semantics.

**Already-terminated case**:
- If daemon verifies PID/session is already dead at operation start, daemon skips control
  delivery and runs teardown cleanup directly using the same coupled invariant above.

**Active-agent safety guard**:
- Daemon cleanup commands MUST NOT delete mailbox or remove roster entry for a
  PID/session-verified active agent unless the caller explicitly requested kill semantics.

**Command expectations**:
- `atm cleanup --agent <name>`: non-destructive for active agents; applies teardown cleanup
  only when daemon verifies dead state (or explicit kill mode is requested). In kill mode,
  it MUST deliver `shutdown_request` first, then enforce timeout/kill fallback.
- `atm daemon --kill <agent> [--timeout <seconds>]`: executes shutdown protocol above,
  then teardown cleanup invariant.

### 4.3.2 `atm teams spawn` (Claude Runtime Baseline)

`atm teams spawn` must provide a first-class CLI path equivalent to the current
`scripts/spawn-teammate.sh` behavior for Claude teammates.

Required baseline behavior:
- Resolve agent runtime metadata from `.claude/agents/<agent>.md` frontmatter
  (at minimum `model`, `color`; prompt body used for initial instruction delivery).
- Resolve team/identity using explicit args first, then `ATM_TEAM` / `ATM_IDENTITY`.
- Resolve working directory from explicit `--folder <path>` (preferred) or legacy
  `--cwd <path>` compatibility alias, else current project root; launch command MUST
  `cd` into that root before starting Claude.
- If both `--folder` and `--cwd` are provided, they must resolve to the same canonical
  path or the command must fail with an actionable mismatch error.
- Register teammate in team config before launch, then update pane/session metadata
  after successful spawn.
- Support resume-aware launch by passing parent session when available (for example,
  `leadSessionId`-derived handoff).
- Deliver initial prompt/body content after launch using ATM messaging path.

Hook/path compatibility requirements:
- Generated hook commands must use `"$CLAUDE_PROJECT_DIR"` for project scripts and guard
  missing files with `test -f` before execution.
- Spawn semantics must not rely on fragile relative paths.

Non-goal:
- Runtime-agnostic spawn (`codex|gemini|opencode`) is tracked separately; Claude
  baseline parity is the immediate requirement.

### 4.3.2a `/team-join` Slash Command + `atm teams join` Contract

ATM must provide a first-class teammate onboarding flow for joining an existing
team from Claude Code slash-command UX.

Required command surfaces:
- Slash command entrypoint: `/team-join` (implemented via skill/frontmatter so it
  is discoverable as a slash command in Claude Code).
- CLI execution contract: `atm teams join <agent> [--team <team>] [--agent-type <type>] [--model <model>] [--folder <path>]`.

Required behavior:
1. **Caller context check first**:
   - Determine whether the caller is already a member of a current team using ATM
     commands and ATM identity resolution.
   - If caller is already on a team, treat invocation as **team-lead initiated**.
2. **`--team` semantics**:
   - In team-lead initiated mode, `--team` is optional verification only.
   - If provided in this mode, it must match the caller's resolved current team or
     the command fails with explicit mismatch guidance.
   - If caller is not already on a team, `--team` is required and identifies the
     existing target team to join.
3. **Join operation**:
   - Verify target team exists before mutation.
   - Add the teammate to roster (`config.json`) using the same persistence and
     validation guarantees as `teams add-member`.
4. **Post-join launch guidance (required output)**:
   - Return a precise Claude command line for launching/resuming the teammate from
     the chosen folder using `--resume`.
   - Human output must include a copy-pastable command and explicit folder context.
   - JSON output must include structured fields:
     - `team`
     - `agent`
     - `folder`
     - `launch_command`
     - `mode` (`team_lead_initiated` or `self_join`)
5. **Cross-runtime launch compatibility note**:
   - The join flow may optionally invoke `atm teams spawn` in another tmux window.
   - For this path, runtime spawn must honor `--folder` for `claude`, `codex`,
     and `gemini` launch contexts.

### 4.3.3 `atm doctor`

`atm doctor` provides a single operational triage report for daemon-backed ATM health.

```
atm doctor
atm doctor --team <name>
atm doctor --json
atm doctor --since <iso8601|duration>
atm doctor --errors-only
atm doctor --full
```

**Checks performed**:
- Daemon health: lock/socket/PID coherence and daemon availability.
- PID/session reconciliation: live process verification for registered team members.
- Roster/session integrity: detect mismatches between `config.json` members and daemon session registry.
- Mailbox/teardown integrity: detect terminal-agent partial teardown states
  (roster removed xor mailbox present).
- Config/runtime drift: detect path/env mismatches relevant to daemon/team operation.
- Unified log diagnostics: summarize warning/error events in the configured time window.

**Default warning/error log window**:
- `since = max(team-lead session start, last doctor call time)`.
- First call (no prior doctor state) uses team-lead session start.
- Repeated calls are incremental by default (new events since prior doctor call).
- `--since` overrides default window.
- `--full` forces full window from team-lead session start.

**`--errors-only` behavior**:
- Scope: affects only the unified log diagnostics check.
- With `--errors-only`, log scanning includes only `error` level events.
- With `--errors-only`, doctor must suppress non-error log findings (for example,
  warning-count summaries and "no events in window" informational findings).
- `--errors-only` does not suppress non-log findings from daemon/session/roster/mailbox/config checks.

**`--since` duration format**:
- Accepted duration grammar: `<positive-integer><unit>`.
- Accepted units: `s` = seconds, `m` = minutes, `h` = hours, `d` = days.
- Examples: `30m`, `2h`, `1d`, `45s`.
- Invalid examples: `0m`, `1w`, `1.5h`, `-5m`, `m30`.
- Zero/negative duration inputs MUST fail validation with actionable error text and
  must not be coerced into a valid range.

**Output requirements**:
- Human-readable output MUST start with a concise team member snapshot table (equivalent
  core fields to `atm members`: name/type/model/status), followed by ordered findings by
  severity, then recommended remediation commands.
- JSON output (`--json`): stable schema with `summary`, `findings[]`, `recommendations[]`, `log_window`.
- Recommendations must include directly runnable commands when applicable and MUST be
  context-aware/actionable for the reported finding class (for example, avoid suggesting
  commands that require unavailable session context without explicit fallback guidance).
- `atm doctor` must be diagnostics-first and report-producing by default:
  daemon probe/autostart failures must be captured as findings in the report,
  not treated as fatal preconditions that suppress report generation.

**JSON output schema (`--json`)**:
- `summary`: `team`, `generated_at`, `has_critical`, `counts` (`critical`, `warn`, `info`)
- `findings[]`: `severity`, `check`, `code`, `message`
- `recommendations[]`: `command`, `reason`
- `log_window`: `mode`, `start`, `end`

#### `DoctorReport` Schema Contract and Compatibility

`atm doctor --json` must return a stable top-level object (`DoctorReport`) with:
- `summary`
- `findings`
- `recommendations`
- `log_window`

Current required `DoctorReport` shape:
- `summary`: `team`, `generated_at`, `has_critical`, `counts`
- `findings[]`: `severity`, `check`, `code`, `message`
- `recommendations[]`: `command`, `reason`
- `log_window`: `mode`, `start`, `end`

Logging-health expansion contract:
- Target shape adds `logging` object with at least:
  - `health_state` (`healthy|degraded_spooling|degraded_dropping|unavailable`)
  - `log_path`
  - `spool_path`
  - `dropped_count`
  - `spool_file_count`
  - `oldest_spool_age_secs`
  - `last_error` (nullable)
- Until this object is implemented, diagnostics may infer logging state from
  findings/recommendations. This is temporary and must be replaced by explicit
  `logging` fields once available.
- Field additions must be backward-compatible (additive-only); existing fields
  above are required and must not be removed or repurposed.

**Last-doctor-call persistence**:
- Path: `~/.config/atm/doctor-state.json`.
- Format: `{"last_call_by_team": {"<team>": "<rfc3339-timestamp>"}}`
- Update timing: on successful `atm doctor` completion.
- Missing/unreadable/invalid state file treated as empty (first-call semantics).

**Exit codes**: `0` = no critical findings, `2` = critical findings, `1` = execution error.

Doctor non-failing requirement:
- Failure to contact or auto-start daemon must not cause immediate process error
  if team/config inputs are otherwise readable; doctor must still emit a report
  with daemon health findings and return severity-based exit (`0` or `2`).
- Exit `1` is reserved for true execution failures that prevent report creation
  (for example unreadable/malformed required team config or unrecoverable output
  serialization/write failure).

### 4.3.3a Operational Health Monitor (`atm-monitor`)

ATM must support a continuous health monitor mode that detects and reports
daemon/team regressions without manual polling.

Required monitor behavior:
- `atm-monitor` must operate as an ATM teammate agent (background-capable), not
  only as an internal function call path.
- As a teammate agent, it must be able to send ATM mail notifications to other
  agents (for example `team-lead`) when actionable findings are detected.
- Poll daemon/team health on a configurable interval (default: `60s`).
- Consume the same checks as `atm doctor` and report only new findings by default.
- Emit alerts via ATM messaging with severity, finding code, and remediation hint.
- Deduplicate repeated alerts for the same finding within a configurable cooldown.
- Preserve enough context in alerts to correlate back to unified logs.
- It may reuse shared evaluator/software components, but agent behavior remains
  the primary operational interface.

Required monitor outputs:
- Human-readable alert form for team operators.
- Stable JSON payload for machine-readable consumers.

Acceptance checks:
- Injecting a controlled daemon/session fault must produce a monitor alert within
  two poll intervals.
- Repeating the same fault within cooldown must not spam duplicate alerts.
- Clearing and re-introducing the fault must emit a new alert.
- Monitor can be launched as a background teammate and continues polling/sending
  alerts without interactive prompting.

### 4.3.3b TUI Baseline Correctness Requirements

TUI behavior must remain consistent with daemon-backed state and inbox data.

Required behavior:
- Left and right status panels must derive from one normalized state source.
  Contradictory panel state for the same agent/session is invalid.
- When daemon state is unavailable, TUI must render explicit degraded/unavailable
  state guidance instead of silent empty or contradictory status.
- TUI must provide inbox message viewing:
  - message list view for selected agent/team context,
  - message detail view for full payload content,
  - mark-as-read persistence using the same atomic write guarantees as CLI reads.
- TUI header must display ATM version from build metadata (`CARGO_PKG_VERSION`).

Acceptance checks:
- Panel-consistency tests fail on divergent left/right state and pass on unified state.
- Message list/detail and mark-read persistence tests pass for representative inbox fixtures.
- Header-render tests assert visible version string in normal TUI startup.

### 4.3.4 Runtime-Agnostic Teammate Spawn Contract

`atm` must support runtime-aware teammate spawn semantics that keep ATM identity
stable across runtimes (Claude/Codex/Gemini/OpenCode) while allowing runtime-
specific session handles.

Required baseline:
- `atm teams spawn` accepts an explicit runtime selector (initially `claude`,
  `codex`, `gemini` where supported).
- Proposed baseline command surface:
  - `atm teams spawn --agent <name> --team <team> --runtime <claude|codex|gemini|opencode> [--model <model>] [--folder <path>|--cwd <path>] [--system-prompt <path>] [--sandbox <on|off>] [--approval-mode <mode>] [--include-directories <paths>] [--env KEY=VALUE ...] [--resume] [--resume-session-id <runtime_session_id>]`
- Spawn supports two modes:
  - **fresh**: start a new runtime session with a system prompt/bootstrap prompt.
  - **resume**: continue an existing runtime session bound to the ATM agent.
- User-facing control remains agent-centric (`team`, `agent`) rather than runtime
  session-centric for normal usage.

### 4.3.4a Codex/Gemini Startup Guidance Prompt Injection

When teammates are launched via `atm teams spawn` for `codex` or `gemini`,
ATM must inject a short operational guidance block into the startup prompt path.

Required behavior:
- If caller supplies `--prompt`, ATM must prepend the guidance block before
  caller text and append a short completion line after caller text.
- If caller omits `--prompt`, ATM must still send the guidance block as the
  initial prompt payload.
- Injection is runtime-scoped to `codex` and `gemini` launch paths.

Canonical guidance content (semantically equivalent text required):
- `Agent-teams-mail is configured for this session.`
- `<team-lead> is orchestrating this session.`
- `atm read --timeout 60`
- `atm send <team-member> "<message>"`

Notes:
- Team/member placeholders may be concretized using resolved team context.
- Command examples must match actual ATM CLI syntax.

### 4.3.5 Runtime Session and Identity Mapping

Daemon/session registry must store both ATM identity and runtime identity:
- canonical ATM identity: `team`, `agent`
- runtime metadata:
  - `runtime` (e.g., `gemini`)
  - `runtime_session_id` (runtime-native session/thread identifier)
  - `process_id`
  - `pane_id` (for tmux-based workers)
  - `runtime_home` (runtime state root when isolated per agent)
  - `state`, `updated_at`

Invariants:
- ATM identity (`team`, `agent`) is stable and is the authoritative routing key
  for ATM mail semantics.
- Runtime session identifiers are adapter-specific and may change between fresh
  and resumed launches.
- Resume-by-agent is the default UX. Runtime session IDs are resolved from ATM
  registry/state in normal flow.

### 4.3.6 Teardown and Liveness Escalation Contract

Teammate teardown must follow request-first semantics:
1. Send polite shutdown request to the target agent.
2. Wait a bounded grace window (default: `15s`, configurable).
3. If unresponsive, escalate with runtime/process signals.

For process-backed runtimes (including Gemini tmux workers), minimum escalation:
- `SIGINT` (`10s` wait, configurable) -> `SIGTERM` (`10s` wait, configurable) -> `SIGKILL`.

Safety requirements:
- Teardown escalation must never target agents outside the current team scope.
- Every escalation stage must emit a structured event to unified logging (section 4.6).

### 4.3.7 Steering Contract (Interactive and Headless)

Steering must support both:
- interactive tmux-pane workers (stdin prompt/control injection), and
- headless/structured transports for MCP-style orchestration.

For runtimes without in-turn prompt injection APIs, ATM must enforce and
document `cancel-then-steer` semantics (no silent assumptions of live turn
mutation).

### 4.3.8 Gemini Baseline Adapter Requirements

Gemini is the first non-Claude runtime baseline for this contract.

Required Gemini behavior:
- Launch options must support:
  - `--model`
  - `--sandbox` / approval mode where configured
  - fresh prompt mode and resume mode (`--resume`)
- Structured headless output must use `--output-format stream-json` for event
  transport where applicable.
- System prompt override support must be available through Gemini-supported
  mechanism (`GEMINI_SYSTEM_MD`).
- Per-agent state isolation must be supported via `GEMINI_CLI_HOME`.
- Lifecycle mapping should use one ATM envelope (`hook-event`) with
  `source.kind = "agent_hook"` for Gemini-origin events (`session_start`,
  `teammate_idle`, `session_end`).
- `teammate_idle` above refers to the existing canonical lifecycle event already
  defined in section 4.5 (not a new event type).

Gemini-specific acceptance checks:
- Fresh spawn persists `runtime=gemini` and a non-empty `runtime_session_id`
  when the runtime provides one.
- Resume spawn binds to the previously persisted `runtime_session_id` for the
  same `(team, agent)` unless an explicit override is provided.
- Registry/query surfaces must return consistent runtime metadata before and
  after resume operations.

### 4.3.9 OpenCode Baseline Adapter Requirements (Discovery Draft)

OpenCode is the next runtime baseline after Gemini for this contract.

Required OpenCode behavior:
- Launch options must support OpenCode-native resume controls:
  - latest-root resume (`--continue`),
  - explicit session resume (`--session <runtime_session_id>`),
  - optional `--fork` on resume flows where requested.
- Runtime identity mapping must persist OpenCode session IDs (`ses_*`) as
  `runtime_session_id` in ATM registry/state.
- System prompt integration must use OpenCode-supported instruction surfaces
  (instruction files/config), since no single CLI `--system-prompt` flag exists
  in the current runtime.
- Per-agent runtime isolation must be provided by agent-scoped XDG roots for
  OpenCode processes.
- Runtime-aware interrupt must prefer API/session cancellation (`session.abort`)
  before process signal escalation.
- Lifecycle and observability events must continue to flow through existing ATM
  unified envelope and logging requirements (sections 4.5 and 4.6), including
  runtime adapter fields (`runtime=opencode`, `runtime_session_id`,
  teardown stage, spawn/resume mode).

### 4.3.10 Availability Signaling Contract

Agent availability signaling must be consistent across hook events and transport layers.

#### T.5c Canonical Payload

Sprint T.5c standardized the availability event payload. The canonical contract
fields are:

| Field | Type | Description |
|-------|------|-------------|
| `team` | string | Team name the agent belongs to |
| `agent` | string | Agent identity (matches config.json member name) |
| `state` | string | Availability state: `"idle"` or `"busy"` |
| `timestamp` | ISO 8601 string | Event time (UTC). Replaces the legacy `ts` short-hand. |
| `idempotency_key` | string | Stable deduplication key per logical event. Must survive replay. |

**Field notes**:

- `timestamp` (not `ts`): T.5c standardizes on the full `timestamp` field name.
  Legacy relays that emit `ts` are accepted during a backward-compat window; the
  daemon normalizes `ts → timestamp` internally. New producers must emit
  `timestamp`.
- `idempotency_key`: Stable per logical event so that replaying the same hook
  event (e.g., after a crash or file-rotation reset) does not produce a duplicate
  state transition. The key must NOT include wall-clock receipt time — it must be
  derived from content-stable fields such as `team`, `agent`, and `turn-id` only.
  Format: `"<team>:<agent>:<turn-id>"`.
- `source` field: **intentionally absent** from the T.5c canonical contract.
  Daemon state is the authoritative source of truth; the originating hook relay
  or adapter is implicit context, not a required field. Emitting `source` is
  permitted but the daemon does not consume or persist it.

Required contract:
- Availability state source of truth is daemon-maintained agent state.
- Idle/busy transitions may be produced by hooks/adapters, but must be normalized
  through one daemon lifecycle/event pipeline.
- Ephemeral pub/sub may distribute availability changes, but must not become the
  canonical persistence source.
- Availability events must include: `team`, `agent`, `state`, `timestamp`,
  and `idempotency_key` (stable per logical event replay).
- Hook relays and adapter emitters may provide these fields directly; daemon
  normalization may derive backward-compatible defaults for legacy payloads, but
  durable behavior and tests must target the canonical contract fields above.

Role boundaries:
- Hook/adapters are signal producers only (emit lifecycle/availability events).
- Daemon lifecycle pipeline validates, normalizes, deduplicates, and mutates
  authoritative availability state.
- Pub/sub is fanout-only notification transport and must not be used as
  persistent state.

Reliability requirements:
- Duplicate/out-of-order availability events must not permanently corrupt state.
- On daemon restart, availability state must recover from durable sources and/or
  liveness checks, not transient pub/sub buffers.

Acceptance checks:
- Hook-derived idle event transitions agent to idle within one update window.
- Replayed duplicate event does not produce duplicate state transition.
- Lost pub/sub message does not prevent eventual correct state via daemon reconciliation.

### 4.4 Configuration

#### Resolution Order (highest priority first)

1. Command-line flags (`--team`, `--identity`)
2. Environment variables (`ATM_TEAM`, `ATM_IDENTITY`)
3. Repo-local config (`.atm.toml` in current directory or git root)
4. Global config (`~/.config/atm/config.toml`)
5. Defaults

#### Configuration File (`.atm.toml`)

```toml
[core]
default_team = "backend-ci-team"    # default team for commands
identity = "team-lead"              # from field on sent messages

[messaging]
offline_action = ""  # default: no call-to-action prefix when recipient appears offline

[display]
format = "text"                     # text | json
color = true
timestamps = "relative"             # relative | absolute | iso8601

[aliases]
arch-atm = "team-lead"   # alias-name → inbox-identity mapping
                         # used as shorthand when the actual identity name is long or changes

[roles]
team-lead = "arch-atm"   # role-name → inbox-identity mapping
                         # roles take precedence over aliases in resolution order
                         # resolution order: roles → aliases → literal fallback
```

**Identity resolution**: The `[aliases]` and `[roles]` tables allow symbolic names to route to actual inbox identities. Resolution order: `[roles]` first (for semantic role names), then `[aliases]` (for stable shorthand), then literal fallback. Resolution is non-recursive and case-sensitive.

#### Environment Variables

| Variable | Description |
|----------|-------------|
| `ATM_TEAM` | Default team name |
| `ATM_IDENTITY` | Sender identity |
| `ATM_CONFIG` | Path to config file override |
| `ATM_NO_COLOR` | Disable colored output |

### 4.5 Recommended Hooks (Agent Teams)

Use Claude Code hooks to enforce safe team behavior and to publish lifecycle
events for daemon state tracking.

**Hook team source of truth**:
- For hook policy decisions, use repo `.atm.toml` `[core].default_team` as the required team.
- Do not rely on `ATM_TEAM` for enforcement, because env state can be stale or missing.

#### `PreToolUse` (`matcher: "Task"`)

Required policy:
- Block Task spawns when the target agent prompt (`.claude/agents/<subagent_type>.md`) declares frontmatter `metadata.spawn_policy = named_teammate_required` unless they are named teammates (`name` provided).
- Block any explicit `team_name` that does not match `.atm.toml` `[core].default_team`.
- Return exit code `2` with actionable feedback when blocked.

Rationale:
- Prevents accidental teammate creation in the wrong team, which causes inbox/context divergence.

#### `TeammateIdle`

Recommended policy:
- Emit a lightweight JSON event for daemon consumption (for example:
  `${ATM_HOME:-$HOME}/.claude/daemon/hooks/events.jsonl`).
- Include at least: `type`, `agent`, `team`, `session_id`, `received_at`.
- Keep this hook non-blocking and fail-open (`exit 0` on relay errors).

Rationale:
- Provides low-latency state transitions (`Busy` → `Idle`) without expensive polling.

#### Unified Lifecycle Event Envelope (Claude + MCP + Future Adapters)

Lifecycle tracking must use one daemon command path (`hook-event`) with a single
extensible payload shape, not separate packet types per integration.

Required baseline fields:
- `event`: `session_start` | `teammate_idle` | `session_end`
- `team`
- `agent` (or canonical `agent_id` where available)
- `source`: source-kind enum

`source` should be expandable and include at least:
- `claude_hook` — Claude Code lifecycle hooks
- `atm_mcp` — lifecycle events emitted by `atm-agent-mcp`
- `agent_hook` — future external agent hooks/adapters (e.g. Codex/Gemini when supported)
- `unknown` — reserved fallback

Expected producer coverage:
- Claude hooks emit `session_start`, `teammate_idle`, `session_end`
- `atm-agent-mcp` should emit equivalent lifecycle events for MCP-managed agents
- Future adapters should map provider lifecycle callbacks into the same envelope
  and daemon command path

AuthZ and validation should be source-aware in one handler, not split across
multiple transport packet types.

#### `TaskCompleted`

Recommended policy:
- Run completion gates (for example: required tests, PR linkage, required status updates).
- Return exit code `2` to prevent completion when policy checks fail.

Rationale:
- Stops tasks from being marked complete before required quality gates pass.

---

### 4.6 Unified Event Logging

`atm` must provide one structured event stream across `atm`, `atm-daemon`, `atm-tui`,
and `atm-agent-mcp` so operators can reconstruct causality and filter by team/session.

Unified event logging uses a single daemon-owned write path with producer fan-in
and spool fallback.

#### Goals

- One common sink across all binaries
- Deterministic, schema-validated JSONL records
- Team/session/request correlation by default
- Fail-open behavior (logging must never block or fail core workflows)
- Safe multi-process operation (no cross-process file append races)

#### Canonical Architecture

- Producers (`atm`, `atm-tui`, `atm-agent-mcp`) emit `log-event` messages to daemon over
  the existing socket envelope.
- `atm-daemon` is the only writer to canonical log files and the only component that
  performs validation, redaction, queueing, and rotation.
- If daemon is unavailable, producers spool locally and daemon merges spool on startup.

#### Socket Contract (`command = "log-event"`)

- Request envelope: existing `SocketRequest` with `version`, `request_id`, `command`,
  and `payload`.
- Command: `log-event`
- Payload: `LogEventV1`
- Success response: `status = "ok"` with payload `{ "accepted": true }`
- Error response: `status = "error"` and code:
  - `VERSION_MISMATCH`
  - `INVALID_PAYLOAD`
  - `INTERNAL_ERROR`

#### Canonical Event Schema (`LogEventV1`)

Required fields:
- `v` (schema version)
- `ts` (RFC3339 UTC)
- `level` (`trace|debug|info|warn|error`)
- `source_binary` (`atm|atm-daemon|atm-tui|atm-agent-mcp`)
- `hostname`
- `pid`
- `target`
- `action`

Optional correlation fields:
- `team`, `agent`, `session_id`
- `request_id`, `correlation_id`
- `outcome`, `error`
- `fields` (structured map), `spans` (span refs)

Validation rules:
- Reject payloads missing required fields
- Enforce serialized-size guard (`64 KiB` max per line, initial default)
- Apply built-in redaction before enqueue/write
- `action` MUST be stable snake_case. Canonical baseline action vocabulary is
  defined in `docs/logging-l1a-spec.md` and is the source of truth for
  dashboard/alert naming.

#### Sink Paths and Files

Canonical log file (daemon-writer mode):
- `${home_dir}/.config/atm/atm.log.jsonl` where `home_dir` is resolved via `get_home_dir()`
  (`ATM_HOME` when set, otherwise platform home directory)

Producer fallback spool directory:
- `${home_dir}/.config/atm/log-spool` where `home_dir` is resolved via `get_home_dir()`

Spool filename convention:
- `{source_binary}-{pid}-{unix_millis}.jsonl`

#### Queue, Redaction, Rotation Defaults

- Daemon in-memory queue capacity: `4096`
- Overflow policy: `drop-new`
- Overflow observability: increment dropped counter + rate-limited warning
- Redaction v1 denylist keys (case-insensitive): `password`, `secret`, `token`,
  `api_key`, `auth`; plus bearer-token value pattern
- Rotation: size-based at `50 MiB`, retain `5` rotated files

#### Failure and Merge Semantics

- Logging failures never fail CLI command execution or daemon progress.
- Producer path is non-blocking best-effort; if socket send fails, write to spool.
- Daemon startup merges spool files via claim/rename then append; delete source file
  only after successful merge.
- Merge ordering: timestamp then file order, append-only.
- Daemon startup spool merge and daemon runtime writer MUST target the same canonical
  path resolved from `ATM_LOG_FILE`/`ATM_LOG_PATH` (or default `atm.log.jsonl`).
  Divergent startup/runtime sink paths are forbidden.

#### Default-On and Health State Requirements

- Unified structured logging is enabled by default for all ATM binaries.
- Logging health must be explicit and queryable with these states:
  - `healthy` — events reaching canonical log sink
  - `degraded_spooling` — daemon/sink unavailable, events spooled
  - `degraded_dropping` — queue overflow or unrecoverable emit failures
  - `unavailable` — no active sink and no successful spool fallback
- Silent degradation is not allowed. State transitions into degraded/unavailable
  must emit structured warning/error events.

#### Logging Diagnostics Surface Requirements

- `atm doctor --json` must include logging health summary with:
  - current health state
  - canonical log path
  - spool directory path
  - dropped-event counter
  - spool-file count and oldest spool age
  - last logging error (if any)
- Human-readable `atm doctor` output must report degraded/unavailable logging as
  actionable findings with remediation commands.
- `atm status --json` must expose logging health state for operator visibility.
- A runbook mapping each health state to remediation commands must be maintained
  in `docs/logging-troubleshooting.md`.

#### Shared Logging Health Evaluator Requirements

- Logging health evaluation must be implemented once in a shared module used by
  both `atm doctor` and `atm status` outputs.
- Health state computation must not be duplicated across command handlers.
- The shared evaluator must consume canonical inputs:
  - daemon reachability
  - canonical log/spool path resolution
  - spool inventory/age
  - dropped-event counters and last logging error metadata where available

#### JSON Schema and Compatibility Requirements

- Logging health JSON object shape must be stable and versioned.
- `atm doctor --json` and `atm status --json` must use the same logging-health
  schema fields for overlapping data.
- Additive fields are allowed; field removal or semantic redefinition requires
  an explicit compatibility note in release docs.
- For one minor release after schema expansion, newly added fields should be
  documented as optional for external consumers.

#### Path Resolution Consistency Requirements

- CLI producers and daemon writer must resolve the same canonical home/log/spool
  paths under identical environment configuration.
- Diagnostics must print resolved paths used by the current process to support
  troubleshooting of path/env mismatches.

#### Migration Bridge (Legacy `events.jsonl`) — REMOVED

The `emit_event_best_effort` dual-write path and `ATM_LOG_BRIDGE` env var were removed.
`emit_event_best_effort` now routes exclusively through the unified producer channel.
No legacy `events.jsonl` sink code remains in any crate.

#### Minimum Event Coverage

- `atm`: `send`, `broadcast`, `request`, `read` outcomes, watermark updates, teams ops
- `atm-daemon`: lifecycle, session registry transitions, plugin lifecycle/errors
- `atm-agent-mcp`: tool-call audit + lifecycle context
- `atm-tui`: startup/shutdown, stream attach/detach, control-send/ack summaries

#### Runtime Controls

- `ATM_LOG=trace|debug|info|warn|error` controls stderr tracing verbosity.
- `ATM_LOG_MSG=none|truncated|full` controls message text inclusion policy.
- `ATM_LOG_FILE` may override file path for tests/ops; `ATM_LOG_PATH` remains alias.

### 4.7 Daemon Auto-Start and Single-Instance Guarantees

Daemon-backed features must work without manual `atm-daemon` bootstrapping while
guaranteeing at most one live daemon per machine/user scope.

#### Start Conditions

CLI must ensure daemon availability before executing daemon-backed commands, including:
- session/lifecycle updates (`hook-event`, session registry reads/writes)
- TUI and control protocol commands
- unified logging producer fan-in (`log-event`)
- plugin-backed operations

If daemon is unreachable, CLI attempts auto-start once per command invocation.

#### Single-Instance Contract

- Daemon startup acquires an exclusive process lock in
  `${home_dir}/.config/atm/daemon.lock`, where `home_dir` is resolved via
  `get_home_dir()` (`ATM_HOME` when set, otherwise platform home directory).
- If lock acquisition fails, new daemon process exits immediately (existing daemon is authoritative).
- Socket path is fixed per user scope:
  - Unix/macOS: `${ATM_HOME:-$HOME/.claude}/daemon/atm-daemon.sock` (existing convention)
  - Windows: named-pipe equivalent (canonical path documented in daemon crate)
- CLI must never spawn a second daemon when lock/socket indicate an existing healthy instance.
- Daemon startup MUST acquire `daemon.lock` before mutating socket or PID files.
- Daemon MUST NOT remove an existing socket file unless lock ownership has already
  been acquired by the current process.

#### Team/Repo Isolation Contract

Single daemon process does not imply shared team behavior. Runtime behavior must
remain isolated per team/repo scope.

Required isolation rules:
- Team state is namespace-isolated by team identifier for:
  - roster/session queries
  - lifecycle state transitions
  - inbox/mailbox integrity checks
  - diagnostics findings and recommendations
- Command scope defaults are single-team:
  - `atm broadcast` targets one team only (resolved team scope), never all teams.
  - `atm doctor` analyzes one team by default.
- Cross-team/global operations must be explicit opt-in flags and must not be
  implicit side effects.
- Cross-team messaging remains explicitly supported by address form
  (`<agent>@<team>`) and must continue working under multi-team scale.
- Namespace-qualified cross-computer addresses must remain supported where
  bridge/transport plugins are enabled; isolation guarantees still apply to the
  resolved team scope.
- Repo-scoped plugin/state data must remain isolated by repo/root context.
- No cross-team data bleed in outputs (`status`, `doctor`, `logs` filters) when
  command scope is a single team.

Scalability expectation:
- Behavior for one team and many teams is semantically identical from the team
  perspective (same correctness/isolation guarantees), independent of total
  number of active teams.
- Multi-team validation should use representative concurrency (multiple active
  teams), not a fixed hardcoded team-count threshold.

#### Required Acceptance Checks

- Starting a second daemon while one is healthy must fail immediately with an
  actionable single-instance error.
- Existing healthy daemon must retain lock ownership; socket/PID files must not
  be overwritten by a second process.
- `atm logs` default view and daemon startup spool merge must observe the same
  canonical `atm.log.jsonl` sink path.

#### Daemon Session Registry Contract

`teams resume` handoff logic depends on daemon truth for active lead session
identity and liveness.

- **Storage path**: `${ATM_HOME:-$HOME}/.claude/daemon/session-registry.json`
- **Ownership**: daemon is sole writer; CLI reads via daemon socket API only.
- **Update sources**:
  - `hook-event` `session_start`: upsert record (`session_id`, `process_id`, `state=active`, `updated_at`)
  - `hook-event` `session_end`: mark record dead (`state=dead`, `updated_at`)
  - daemon liveness sweeps may mark stale PIDs dead when process no longer exists
- **Lookup semantics**:
  - Team-scoped lead check must resolve by `(team, agent=team-lead)`
  - CLI `teams resume` refusal logic must use this team-scoped daemon result, not bare-name process lookup

Minimum record shape:

```json
{
  "team": "atm-dev",
  "agent": "team-lead",
  "session_id": "uuid",
  "process_id": 12345,
  "state": "active",
  "updated_at": "2026-02-27T00:00:00Z"
}
```

#### CLI Spawn/Readiness Flow

1. Probe daemon socket/pipe.
2. If healthy, continue.
3. If unavailable, spawn daemon detached with platform-native process creation.
4. Wait for readiness with bounded retry/backoff (default total wait `5s`).
5. If ready, continue command.
6. If not ready, fail daemon-backed command with actionable error; non-daemon commands continue.

#### Mid-Session Daemon Death

- Producers (logging, lifecycle, control) fail-open where possible:
  - lifecycle/logging events use spool fallback or best-effort warning
  - control commands return explicit daemon-unavailable error
- CLI retries one auto-restart attempt on next daemon-backed operation.
- Daemon startup must recover durable state needed for safety:
  - replay pending spool files
  - restore dedupe/session metadata from durable stores where implemented

#### Cross-Platform Requirements

- Windows CI coverage must validate spawn/readiness/lock behavior.
- Use `std::process::Command`/Tokio process APIs only; no shell-specific assumptions.
- Path handling must use `Path`/`PathBuf`; avoid hardcoded separators.
- Readiness timeout/backoff defaults must be shared across platforms.

#### Roster Seeding and Config Watcher Requirements

- On daemon startup, roster state must be seeded from each team `config.json`.
- Daemon must watch `config.json` changes and reconcile member adds/removes/updates.
- Roster reconciliation must preserve mailbox/roster coupling invariants from
  section 4.3.1.
- Drift conditions (roster without mailbox, mailbox without roster) must be
  surfaced to diagnostics (`atm doctor`) as actionable findings.

Acceptance checks:
- Starting daemon with pre-populated team config yields matching in-memory roster.
- Editing `config.json` to add/remove a member updates daemon roster within one
  watch cycle.
- Drift injection is detected and reported by diagnostics.

#### Agent State Transition Requirements

- Agent state must transition based on lifecycle events plus PID liveness checks.
- Supported baseline states: `unknown`, `active`, `idle`, `offline`.
- State transitions must record `reason` and `source` for troubleshooting.
- Team/status outputs must reflect reconciled state within one poll window.

Acceptance checks:
- `session_start` drives `unknown/offline -> active`.
- `teammate_idle` drives `active -> idle`.
- PID death drives `active/idle -> offline` when lifecycle end is missing.
- Conflicting signals resolve deterministically (latest valid event with liveness guard).

### 4.8 MCP Server Setup (`atm mcp`)

The `atm mcp` command group provides user-facing setup and status tooling for
configuring `atm-agent-mcp` as an MCP server across supported AI coding clients.
It is distinct from the `atm-agent-mcp` crate (section 6.6, `docs/atm-agent-mcp/requirements.md`)
— `atm mcp install` configures `atm-agent-mcp` as an MCP server, but `atm mcp`
itself is part of the `atm` CLI binary, not the proxy crate.

> **Note**: The `atm-agent-mcp serve` subcommand referenced by install entries is
> defined in `docs/atm-agent-mcp/requirements.md` FR-1 (MCP stdio proxy mode).

#### 4.8.1 Supported Clients

| Client | User/Global Config | Project/Local Config | Format | Source |
|--------|-------------------|---------------------|--------|--------|
| Claude Code | `~/.claude.json` (`mcpServers` field, user scope) | `.mcp.json` (project scope, committed) | JSON (`mcpServers` key, `"type": "stdio"`) | [Claude Code MCP docs](https://code.claude.com/docs/en/mcp) |
| Codex CLI | `~/.codex/config.toml` | N/A (global only) | TOML (`[mcp_servers.*]` section) | [Codex CLI docs](https://github.com/openai/codex) |
| Gemini CLI | `~/.gemini/settings.json` | `.gemini/settings.json` | JSON (`mcpServers` key) | [Gemini CLI docs](https://github.com/google-gemini/gemini-cli) |

**Claude Code scope mapping**: Claude Code has three MCP scopes:
1. **"user"** (cross-project): top-level `mcpServers` object in `~/.claude.json`
2. **"local"** (per-project, private): per-project entries inside `~/.claude.json` keyed by project path — NOT the same as the top-level `mcpServers`
3. **"project"** (shared, committed): `.mcp.json` at the project root

ATM's `global` scope = Claude Code "user" scope (`~/.claude.json` top-level `mcpServers`).
ATM's `local` scope = Claude Code "project" scope (`.mcp.json`).
ATM deliberately does **not** target Claude Code's "local" scope (the per-project entries
inside `~/.claude.json`) because those are private to the user and harder to manage externally.

#### 4.8.2 `atm mcp install`

```
atm mcp install <client> [scope] [--binary <path>]
```

**Arguments**:
- `<client>` — target client: `claude`, `codex`, or `gemini`
- `[scope]` — `global` (user-level, default) or `local` (project-level)
- `--binary` — override auto-detected `atm-agent-mcp` path (must be a regular
  file with executable permission; directories and non-executable files are rejected)

**Behavior**:
- Auto-detects `atm-agent-mcp` binary via `std::env::split_paths` + PATH lookup
  in-process (no shell subprocess dependency). Shell commands like `which`/`where`
  must never be used for resolution; they may only appear in user-facing diagnostic
  messages suggesting what the user can run manually.
- Reads existing config file, preserving all existing content (read-modify-write)
- Adds or updates the `atm` MCP server entry with `command` and `args: ["serve"]`
- Creates parent directories if needed
- For Claude Code global: reads `~/.claude.json`, adds/updates `mcpServers.atm`
  entry with `"type": "stdio"`, `"command"`, and `"args": ["serve"]`
- For Claude Code local: writes `.mcp.json` with `mcpServers.atm` entry
  including `"type": "stdio"`, `"command"`, and `"args": ["serve"]` (same fields as global)
- For Codex: parse-and-merge semantics — parse the existing TOML, update/add the
  `[mcp_servers.atm]` table, and re-serialize. If an existing `[mcp_servers.atm]`
  entry exists, update it in place (idempotent). If not, add the new table.
- Codex local scope is rejected with an error (not supported by Codex)
- If already configured with identical settings, reports existing configuration
  without modifying (exit code 0)

**Cross-scope deduplication**: When `local` scope is requested, check if `atm`
is already configured at `global` scope for the same client first. If global is
already configured, skip the local install and report:
`"Project scope install skipped. atm MCP already installed globally."`
This prevents redundant configuration. The reverse (global install when local
exists) proceeds normally since global takes broader precedence.

**Codex TOML entry format** (merged into `~/.codex/config.toml`):
```toml
[mcp_servers.atm]
command = "/opt/homebrew/bin/atm-agent-mcp"
args = ["serve"]
```

**Idempotency detection**: For JSON clients (Claude, Gemini), check if
`mcpServers.atm` exists with matching `command` and `args`. For Codex TOML,
parse and check `mcp_servers.atm.command` and `mcp_servers.atm.args`.

**Install outcome states**:
- `installed` — new configuration written
- `updated` — existing `mcpServers.atm` entry found with different `command` path;
  overwritten with new path and reported as "Updating" with both old and new paths shown
- `already-configured` — identical configuration exists, no changes
- `skipped` — cross-scope deduplication (global already configured)
- `error` — binary not found, invalid config, unsupported scope

**Exit codes**:
- `0` — success (installed, updated, already-configured, or skipped)
- `1` — error (binary not found, invalid config file, unsupported scope)

> **Note**: Unlike the general exit code policy in section 8.2 (where exit code 2 = partial
> failure), `atm mcp install` uses only 0/1. The `skipped` and `already-configured`
> outcomes are not errors — they indicate the system is in the desired state — so they
> return exit code 0. There is no partial-failure scenario for single-client install.

**Error conditions**:
- Binary not found in PATH and no `--binary` override → error with install instructions
- Config file exists but is not valid JSON/TOML → error
- Codex + local scope → error (unsupported)

#### 4.8.2a `atm mcp uninstall`

```
atm mcp uninstall <client> [scope]
```

**Arguments**:
- `<client>` — target client: `claude`, `codex`, or `gemini`
- `[scope]` — `global` (default) or `local`

**Behavior**:
- Removes the `atm` MCP server entry from the specified client configuration
- For JSON clients (Claude, Gemini): removes `mcpServers.atm` key from config,
  preserving all other content (read-modify-write)
- For Codex TOML: parse, remove `[mcp_servers.atm]` table, re-serialize
- If `atm` is not configured, reports "not present" without error (exit code 0)
- Codex local scope is rejected with an error (not supported)

**Uninstall outcome states**:
- `removed` — configuration entry deleted
- `not-present` — no `atm` entry found, nothing to remove
- `error` — invalid config file, unsupported scope

**Exit codes**:
- `0` — success (removed or not-present)
- `1` — error (invalid config file, unsupported scope)

> **Note**: Same as install — `not-present` returns exit code 0 (desired state achieved).
> See install exit code note for rationale on deviation from section 8.2.

#### 4.8.3 `atm mcp status`

```
atm mcp status
```

**Behavior**:
- Reports `atm-agent-mcp` binary availability and path
- For each supported client, checks applicable config files per the scope matrix:
  - Claude Code: user scope (`~/.claude.json`) + project scope (`.mcp.json`)
  - Codex: global only (`~/.codex/config.toml`)
  - Gemini: user scope (`~/.gemini/settings.json`) + project scope (`.gemini/settings.json`)
- Reports whether `atm` is configured as an MCP server in each location
- System-level config paths (e.g., Gemini `/Library/Application Support/`) are
  intentionally not checked; status covers user and project scopes only.
- **Status labels**: Claude Code and Gemini use "User"/"Project" to match their
  scope terminology. Codex uses "Global" because it supports only a single
  global scope and does not use "user"/"project" terminology.

**Output format** (text only, no `--json` in initial version):

When binary is found:
```
ATM MCP Server Status
=====================

Binary: /opt/homebrew/bin/atm-agent-mcp
Available: yes

Claude Code:
  User    configured       ~/.claude.json
  Project not configured   .mcp.json

Codex:
  Global  configured       ~/.codex/config.toml

Gemini CLI:
  User    not configured   ~/.gemini/settings.json
  Project not configured   .gemini/settings.json
```

When binary is NOT found:
```
ATM MCP Server Status
=====================

Binary: (not found)
Available: no

Claude Code:
  User    not configured   ~/.claude.json
  Project not configured   .mcp.json

Codex:
  Global  not configured   ~/.codex/config.toml

Gemini CLI:
  User    not configured   ~/.gemini/settings.json
  Project not configured   .gemini/settings.json

Install atm-agent-mcp with:
  brew install randlee/tap/agent-team-mail
  cargo install agent-team-mail  (includes atm-agent-mcp binary)
```

#### 4.8.4 Cross-Platform Requirements

- Binary detection uses in-process PATH resolution (`std::env::split_paths` +
  file existence + executable permission check) exclusively. Shell `which`/`where`
  subprocess calls must never be used for resolution — they may only appear in
  user-facing diagnostic text. On Unix, verify the executable bit (`mode & 0o111`);
  on Windows, file existence with known extension is sufficient.
- Config file paths use `home_dir()` with `ATM_HOME` override for testing
- File writes preserve existing content (read-modify-write for JSON; parse-and-merge for TOML)
- Windows config paths: all clients use standard home-dir conventions
  (`%USERPROFILE%`). Claude Code: `%USERPROFILE%\.claude.json`. Codex:
  `%USERPROFILE%\.codex\config.toml`. Gemini: `%USERPROFILE%\.gemini\settings.json`.
  If a client documents different Windows paths in the future, follow their docs.
  `ATM_HOME` override enables test isolation on all platforms.

#### 4.8.5 Future Extensions (Not in Initial Scope)

- `--json` output mode for `atm mcp status`
- Validation that `atm-agent-mcp serve` actually starts successfully
- `atm mcp test` — run a quick connectivity check against configured servers

#### 4.8.6 CLI Crate Publishability Requirements

`agent-team-mail` CLI crate must be publishable and installable via crates.io
without relying on repository-external paths.

Required constraints:
- Crate code must not use compile-time file includes (`include_str!`,
  `include_bytes!`, or equivalent) that reference files outside the crate
  publish boundary.
- Release workflows must fail hard on publish failures for required artifacts.
  Failure masking through shell fallbacks is not allowed.
- Publish validation must run before release completion and must include:
  - package manifest validation,
  - build from packaged sources,
  - version installability check (`cargo install` path for released version).
- Every release must produce a machine-readable artifact inventory that includes,
  at minimum, artifact identifier, version, source reference, publish target,
  and verification command(s).
- Post-publish verification must run for every required inventory item and record
  pass/fail evidence for each item.
- Release completion is permitted only when all required inventory items verify
  successfully, or when explicit waivers are recorded with approver and reason.
- The publishing process above is the default release procedure for all future
  releases, not a one-off phase policy.

Acceptance checks:
- `cargo package` and `cargo publish --dry-run` succeed for CLI crate in CI.
- Simulated publish failure causes workflow failure (non-zero overall status).
- Post-release install validation resolves the expected CLI version.
- Inventory validation fails when required fields are missing, artifact entries
  are duplicated, or ordering is non-deterministic.
- Post-publish verification failure for any required item fails the release gate
  unless a documented waiver is present.

### 4.9 Team Hook Setup (`atm init`)

The `atm init` command provides one-command ATM setup and validates Claude Code
hook wiring for session coordination. Hook script bodies are embedded in the
ATM binary and materialized at install time.

**Claude hook path reference**:
- Canonical docs: https://docs.anthropic.com/en/docs/claude-code/hooks (redirects to https://code.claude.com/docs/en/hooks)
- Follow "Reference scripts by path": use `"$CLAUDE_PROJECT_DIR"/...` for project-scoped scripts and `"${HOME}/.claude/scripts/"...` for globally-installed scripts.

#### 4.9.1 Command Forms

```bash
atm init <team>
atm init <team> --local
atm init <team> --identity <name>
atm init <team> --skip-team
atm init --check
atm init <team> --check
```

**Arguments and flags**:
- `<team>`: target/default team name for generated `.atm.toml` and optional team creation.
- `--local`: install hooks in project scope (`.claude/settings.json`) instead of default global scope.
- `--identity <name>`: identity value written to `.atm.toml` (`team-lead` default).
- `--skip-team`: skip team creation step (join-existing-team workflows).
- `--check`: read-only validation; report missing/misaligned wiring without modifying files.

#### 4.9.2 Behavior

- One-command setup order (idempotent at each step):
  1. Create `.atm.toml` in cwd when missing (writes `identity`, `default_team`).
  2. Create team (`~/.claude/teams/<team>/`) when missing, unless `--skip-team`.
  3. Install hooks (global by default, or local with `--local`).
- Default install writes/merges hook entries in `~/.claude/settings.json` (global scope).
- `--local` install writes/merges hook entries in project `.claude/settings.json`.
- Installs are idempotent: reruns preserve unrelated settings and avoid duplicate entries.
- Existing `.atm.toml` is preserved (no silent overwrite); command reports that existing config was found.
- Existing team is preserved (no duplicate recreation); command reports "team already exists".
- Global-installed hooks must remain passive in non-ATM repositories; `.atm.toml` guard is the first hook operation.
- Embedded hook scripts are the runtime source of truth.

**Required test scenarios** (each must be independently tested):

| Scenario | Pre-state | Expected outcome |
|----------|-----------|-----------------|
| Fresh setup | No `.atm.toml`, no hooks, no team | Creates all three; reports each as "created" |
| Has `.atm.toml`, no hooks | `.atm.toml` present, hooks absent | Installs hooks; does not overwrite `.atm.toml` |
| Has hooks, no `.atm.toml` | Hooks present, `.atm.toml` absent | Creates `.atm.toml` and team; does not duplicate hooks |
| Fully initialized | `.atm.toml`, hooks, and team all present | No changes; all three reported as "already configured" |

#### 4.9.3 File and Write Requirements

- Use read-modify-write semantics; never wholesale rewrite settings files.
- `.atm.toml` creation must also use read/merge-safe behavior (create-only by default; explicit mutation paths must be additive and transparent).
- Preserve unknown fields and non-ATM hook entries.
- Use atomic writes (temp + rename) and create parent directories as needed.
- Report exact file path(s) modified in command output.
- Generated hook command paths should use `"$CLAUDE_PROJECT_DIR"` (project scope) for project-local hook scripts and `"${HOME}/.claude/scripts/"` (global scope) for globally-installed hook scripts, not repo-absolute paths.
- `atm init` success output must include whether hooks were installed globally or locally.

#### 4.9.4 Exit and Result Semantics

- Exit `0` for `installed`, `updated`, `already-configured`, and `check-ok`.
- Exit `1` for malformed config, unsupported environment, or write/permission failures.
- `--check` output must include actionable guidance for each missing/misaligned hook entry.
- Idempotent no-op cases (`.atm.toml` exists, team exists, hooks already configured)
  are success states and must be explicitly reported in human output.

---

## 5. Plugin System (Daemon Only)

Plugins live exclusively in `atm-daemon`. The CLI has no plugin awareness — it operates directly on files via `atm-core`.

### 5.1 Design Principles

Informed by analysis of the `coding_agent_session_search` connector system (14-plugin Rust codebase):

**Adopted from that system:**
- Simple trait contract (low barrier to implement)
- Per-plugin error isolation (one bad plugin can't crash the system)
- Parallel execution model

**Improved upon:**
- Bidirectional (send + receive, not read-only)
- Async with cancellation tokens (daemon can't block on slow I/O)
- Macro-based registration (not hardcoded in 5 places)
- Structured error reporting (not silent swallowing)
- Stateful plugins (daemon plugins maintain connections, sync cursors, watch handles)
- Plugin metadata with versioning

### 5.2 Plugin Trait

```rust
pub struct PluginMetadata {
    pub name: &'static str,
    pub version: &'static str,
    pub description: &'static str,
    pub capabilities: Vec<Capability>,
}

pub enum Capability {
    /// Plugin can add synthetic members to teams
    AdvertiseMembers,
    /// Plugin can intercept outbound messages
    InterceptSend,
    /// Plugin can inject inbound messages
    InjectMessages,
    /// Plugin reacts to events (new message, team change)
    EventListener,
}

#[async_trait]
pub trait Plugin: Send + Sync {
    /// Plugin identity and capabilities.
    fn metadata(&self) -> PluginMetadata;

    /// One-time setup. Read config, establish connections.
    async fn init(&mut self, ctx: &PluginContext) -> Result<(), PluginError>;

    /// Long-running event loop. Watch for events, inject/intercept messages.
    /// Must respect the cancellation token for graceful shutdown.
    async fn run(&self, ctx: &PluginContext, cancel: CancellationToken) -> Result<(), PluginError>;

    /// Graceful shutdown. Flush caches, close connections, clean up members.
    async fn shutdown(&self) -> Result<(), PluginError>;
}
```

### 5.3 Plugin Context

```rust
pub struct PluginContext {
    /// Shared system context (repo, provider, claude version)
    pub system: Arc<SystemContext>,

    /// Read/write inbox messages (uses atm-core atomic swap)
    pub mail: Arc<MailService>,

    /// Add/remove synthetic team members
    pub roster: Arc<RosterService>,

    /// Plugin-specific config section from .atm.toml
    pub config: toml::Value,

    /// Plugin temp storage: temp/atm/<plugin-name>/
    pub temp_dir: PathBuf,
}
```

Plugins access shared system info (repo name, git provider, claude version) via `ctx.system`. Provider-specific concerns (auth tokens, API clients, rate limiting) are the plugin's responsibility.

**Multi-repo daemon model (design gap to address)**:
- Current implementation assumes one daemon per repo (paths and plugin state are repo-scoped).
- Future design must support a single daemon hosting multiple repos/roots.
- Plugin state, caches, and report outputs must be scoped by repo/root context.
- When `repo` is missing, plugins should fall back to `root` for storage and either disable or degrade gracefully if git context is required.

**Proposed direction**:
- Single daemon per machine, started on first plugin activation.
- Plugins maintain repo registries and agent subscriptions (per repo).
- CI Monitor supports multiple agents per repo, potentially branch-scoped subscriptions.
- Notifications should include co-recipient hints when multiple agents are subscribed.

**Configuration tiers (agreed)**:
- **Machine/daemon**: machine-scoped config listing repos to monitor.
- **Repo**: repo-scoped CI settings (single source of truth for agents).
- **Team**: collaboration/transport settings only (no CI settings).

### 5.4 Plugin Registration

Compile-time registration via `inventory` crate (avoids hardcoded registration):

```rust
// In each plugin module — single line to register
inventory::submit! {
    PluginFactory::new("gh-issues", || Box::new(GhIssuesPlugin::new()))
}

// In daemon startup — auto-discovers all registered plugins
for factory in inventory::iter::<PluginFactory> {
    if config.is_plugin_enabled(factory.name) {
        let plugin = (factory.create)();
        daemon.register(plugin);
    }
}
```

Adding a new plugin = one file with `inventory::submit!`. Zero edits to central code.

### 5.5 Plugin-Managed Members

Plugins declaring `AdvertiseMembers` can add synthetic members to a team's `config.json`. These members look identical to local agents — other agents message them normally via inbox files.

The plugin is responsible for:
- Adding/removing the member entry in `config.json` (via `ctx.roster`)
- Syncing the agent's inbox file (via `ctx.mail`)
- Transporting messages to/from the external system
- Cleaning up on shutdown

No synthetic members exist without a plugin to manage them.

### 5.6 Plugin Configuration

Each plugin gets a section in `.atm.toml`:

```toml
[plugins.issues]
enabled = true
poll_interval = "5m"
labels = ["bug", "agent-task"]

[plugins.ci-monitor]
enabled = true

[plugins.bridge]
enabled = true
remote_host = "192.168.1.100"
remote_port = 9876
```

### 5.7 Temporary File Storage

Plugins that cache data use a conventional pattern:

```
temp/atm/<plugin-name>/
```

- Gitignored (covered by `temp/` in `.gitignore`)
- Plugin's responsibility to manage (create, rotate, clean up)
- No guaranteed persistence across reboots
- Recommended for offline caching, report storage, sync state

---

## 6. Planned Plugins

All plugins are **provider-agnostic** where applicable. They read `ctx.system.repo.provider` to determine the git host and handle provider-specific API details internally.

### 6.1 Issues Plugin (First Plugin)

**Purpose**: Bridge between git provider issues and agent team messaging.

**Providers**: GitHub, Azure DevOps, GitLab, Bitbucket

**Capabilities**: `AdvertiseMembers`, `EventListener`, `InjectMessages`

**Planned features**:
- Watch a repository for new/updated issues matching filters (labels, assignees)
- Create messages to agents when issues are created or updated
- Allow agents to respond on issues via inbox messages
- Provider-specific auth via environment variables (plugin-managed)

### 6.2 CI Monitor Plugin

**Purpose**: Monitor CI/CD workflows and notify agents of failures.

**Providers**: GitHub Actions, Azure Pipelines, GitLab CI, etc.

**Reference**: [`ci-monitor-design.md`](../../agent-teams-test/docs/ci-monitor-design.md)

**Capabilities**: `InjectMessages`, `EventListener`

**Planned features**:
- Watch CI workflow runs for failures
- Generate failure reports (JSON + Markdown) in `temp/atm/ci-monitor/`
- Post concise notification to designated agent's inbox
- Deduplicate per-commit
- Requires git repo context; if no repo is detected, the plugin should disable itself with a clear warning.

**Multi-repo + agent subscription model (planned)**:
- Single daemon per machine; CI Monitor registers multiple repos from machine-level config.
- Each repo can have one or more subscribed agents (team-lead or dedicated CI agent).
- Branch filters support exact branch, branch + derived branches (worktree ancestry), and “all branches.”
  - Proposed syntax: `develop:*` (develop + all branches derived from develop), `develop:feature/*` (derived + pattern). `:` indicates derived-branch matching.
- If multiple agents are subscribed to the same event, include a notification warning such as:
  `Warning: <agent>@<team> is also receiving this notification`
- Distinguish **plugin settings** (repo registry, provider config, poll interval) from **agent settings** (response behavior, routing preferences, scratch-pad state).

**Multi-repo config file layout (agreed)**:
- Mono-repo: single `config.atm.toml` at repo root.
- Multi-repo: machine-level config lists repo paths, and each repo has its own `<repo>.config.atm.toml`.
  - Machine-level daemon config path: `~/.config/atm/daemon.toml`
  - Repo-level config path: `<repo>/.atm/config.toml` (for mono-repo, `config.atm.toml` at repo root is acceptable)

**Daemon lifecycle (planned)**:
- CLI starts the daemon on first use of any daemon-backed feature if not already running.
- Daemon should support hot-reload for config changes without restart.

**CI Monitor without repo**:
- CI Monitor is only valid for repo contexts.
- If repo is missing, CI Monitor should disable with a clear warning and prompt the CI agent to ask the team-lead/user for repo info.
- Agents may intentionally subscribe to repos outside their local root for dashboards or testing; co-recipient warnings help disambiguate.

### 6.3 Cross-Computer Bridge Plugin

**Purpose**: Enable agent teams that span multiple machines.

**Capabilities**: `AdvertiseMembers`, `InterceptSend`, `InjectMessages`

**Planned features**:
- Advertise remote agents as local team members (via `ctx.roster`)
- Sync inbox files between machines (transport TBD: TCP, SSH, HTTP)
- Handle offline scenarios with temp file caching
- Bidirectional — both machines can initiate communication

### 6.7 Async Agent Worker Adapter (Generic, Codex First)

**Purpose**: Allow async teammates without requiring a foreground terminal. Codex is the first backend.

**Planned features**:
- Daemon plugin that routes inbox messages to a tmux-backed worker session
- Worker launches/attaches per agent and uses `tmux send-keys` for input
- Responses are captured (prefer log file tailing over capture-pane) and written back to inbox
- Designed to avoid stdin injection into the user's active terminal
- Backend-agnostic adapter interface (Codex implementation first, others later)

### 6.4 Human Chat Interface Plugin

**Purpose**: Connect human users via chat applications.

**Capabilities**: `AdvertiseMembers`, `InterceptSend`, `InjectMessages`

**Planned features**:
- Bridge between a chat app (Slack, Discord, etc.) and agent inboxes
- Support individual and team/channel message routing
- Multiple human users, each as a synthetic team member

### 6.5 Beads Mail Plugin

**Purpose**: Bridge between the Beads protocol and agent team messaging.

**Reference**: [https://github.com/steveyegge/beads](https://github.com/steveyegge/beads)

**Context**: Beads are the mail primitive used in Gastown. This plugin enables agent teams
to send/receive beads, allowing integration with Gastown-based workflows.

**Status**: Planned — research and design TBD.

### 6.6 MCP Agent Mail Plugin

**Purpose**: Bridge between MCP-based agent mail and agent team messaging.

**Reference**: [https://github.com/Dicklesworthstone/mcp_agent_mail](https://github.com/Dicklesworthstone/mcp_agent_mail)

**Context**: MCP Agent Mail is an alternative agent messaging system. This plugin enables
interoperability between `atm` teams and agents using the MCP agent mail protocol.

**Status**: Planned — research and design TBD.

> **Note**: This plugin (Section 6.6) is an `atm-daemon` plugin for interoperability with the external [mcp_agent_mail](https://github.com/Dicklesworthstone/mcp_agent_mail) project. It is unrelated to `atm-agent-mcp` (the Codex MCP proxy crate defined in `docs/atm-agent-mcp/`). Despite both having "MCP" in their names, they serve different purposes: this plugin bridges a third-party messaging protocol, while `atm-agent-mcp` wraps Codex sessions with ATM identity and communication.

---

## 7. Cross-Team Messaging

### 7.1 Same-Machine Cross-Team

The core supports messaging between different teams on the same machine:

```
atm send agent-b@other-team "message from this team"
```

This writes directly to `~/.claude/teams/other-team/inboxes/agent-b.json`.

### 7.2 Cross-Machine (Plugin)

Cross-machine messaging is entirely a plugin responsibility. The bridge plugin:

1. On Machine A: Watches inboxes for messages to remote agents
2. Transports message to Machine B over network
3. On Machine B: Writes message to the target agent's local inbox file
4. Return path works the same way in reverse

The core has no awareness of whether a team member is local or remote.

---

## 8. Non-Functional Requirements

### 8.1 File I/O Safety

- **Atomic swap**: `renamex_np` (macOS) / `renameat2` (Linux) for conflict-safe writes
- **File locking**: `flock` advisory locks between atm processes
- **Conflict detection**: Hash comparison after swap to detect Claude concurrent writes
- **Round-trip preservation**: Unknown JSON fields preserved on read-modify-write
- **No data loss**: Never truncate or silently drop messages
- **Graceful degradation**: Missing files, empty files, malformed JSON — log warning, don't crash

### 8.2 Error Handling

- **Structured errors**: `thiserror` for typed error variants in `atm-core`
- **Application errors**: `anyhow` in binary crates (`atm`, `atm-daemon`)
- **User-facing errors**: Clear, actionable messages (not raw stack traces)
- **Per-plugin isolation**: A failing plugin does not crash the daemon or affect other plugins
- **Exit codes**: 0 = success, 1 = error, 2 = partial failure

### 8.3 Testing

- **Unit tests**: Schema parsing, config resolution, atomic I/O, conflict detection
- **Integration tests**: End-to-end CLI commands against temp `~/.claude/` fixtures
- **Plugin trait tests**: Default test harness for plugin implementations
- **No external dependencies in tests**: Mock file system, no network calls
- **Schema evolution tests**: Verify round-trip with unknown fields, missing optional fields
- **Global env mutation safety**: Tests that read/write process-global env vars
  (for example `ATM_HOME`) MUST be serialized to avoid cross-test races.
- **Parallel stability gate**: CI/local suites must include a parallel run baseline
  (`--test-threads=8` or equivalent) for env-sensitive integration tests.

### 8.4 Performance

- **CLI startup**: < 100ms for simple commands (send, read)
- **Large inboxes**: Handle inbox files with 10,000+ messages without degradation
- **Minimal allocations**: Streaming JSON read/write for large files

### 8.5 Platform Support

| Platform | Tier | Atomic Swap | Notes |
|----------|------|-------------|-------|
| macOS | Primary | `renamex_np(RENAME_SWAP)` | Development machine |
| Linux | Secondary | `renameat2(RENAME_EXCHANGE)` | CI, servers |
| Windows | Secondary | Best-effort | CI coverage required |

**CI requirement**: Tests must run on macOS, Linux, and Windows.

### 8.6 Inbox Retention and Cleanup

- `atm` should prevent unbounded inbox growth by applying a configurable retention policy.
- Default behavior for non-Claude-managed members: archive or delete old messages automatically.
- If Claude does not perform cleanup for its own agents, `atm` should optionally apply retention there as well.
- Retention policies must be configurable by max message count and/or max age.
- For daemon-managed teammate teardown, inbox deletion and roster removal from
  `config.json` MUST occur together for terminal agents (already-dead or killed after
  timeout). Partial cleanup states are invalid and must be reconciled.
- For active agents, retention/cleanup MUST NOT remove mailbox or roster entry unless
  explicit kill semantics are invoked.
- For active-agent termination intent, cleanup tooling MUST send `shutdown_request` and
  wait for termination/timeout before performing mailbox deletion and roster removal.

### 8.7 Large Payloads and File References

- File paths are always treated as references; inbox JSON must never embed file contents.
- File references are allowed only when the path is permitted by the destination repo settings.
- If a path is not permitted for the destination repo, `atm` must copy the file to a local share folder and rewrite the message to reference that copy, explicitly noting the rewrite.
- Default share folder: `~/.config/atm/share/<team>/` (configurable).
- Cross-computer transfer remains a plugin responsibility; the core only guarantees safe local references.

---

## 9. Technology Stack

| Component | Choice | Rationale |
|-----------|--------|-----------|
| Language | Rust (Edition 2024) | Type safety, performance, existing agent infrastructure |
| CLI framework | `clap` (derive) | Standard Rust CLI framework |
| Async runtime | `tokio` (daemon only) | Plugin async trait, inbox watchers |
| Serialization | `serde` + `serde_json` | JSON file I/O with `#[serde(flatten)]` for round-trip |
| Error handling | `thiserror` (lib) / `anyhow` (bin) | Per Pragmatic Rust Guidelines |
| Config | `toml` + `serde` | `.atm.toml` parsing |
| Logging | JSONL event sink + `tracing` | Unified structured events across binaries, plus operational diagnostics |
| Plugin registry | `inventory` | Compile-time auto-registration |
| File locking | `flock` (libc) | Advisory locks for atm-to-atm coordination |
| Testing | Built-in + `tempfile` + `assert_cmd` | Standard Rust test ecosystem |

### Guidelines

Follow [Pragmatic Rust Guidelines](../.claude/skills/rust-development/guidelines.txt) for all implementation decisions.

---

## 10. MVP Scope

### In Scope (MVP)

- [ ] Workspace setup (`atm-core`, `atm` crates)
- [ ] `atm-core`: Schema types with `#[serde(flatten)]` round-trip preservation
- [ ] `atm-core`: Schema version detection (Claude Code version → schema compat)
- [ ] `atm-core`: Atomic swap with conflict detection (`renamex_np` / `renameat2`)
- [ ] `atm-core`: File locking (`flock`) for atm-to-atm coordination
- [ ] `atm-core`: SystemContext with RepoContext and GitProvider detection
- [ ] `atm-core`: Config resolution (flags → env → repo → global → defaults)
- [ ] `atm` CLI: All commands from Section 4 (send, read, broadcast, inbox, teams, members, status, config)
- [ ] Cross-team messaging (same machine)
- [ ] Comprehensive test suite

### Out of Scope (Post-MVP)

- [ ] `atm-daemon` crate and daemon mode
- [ ] Plugin trait, registry, and PluginContext
- [ ] Issues plugin (first plugin, post-MVP)
- [ ] CI Monitor plugin
- [ ] Cross-computer bridge plugin
- [ ] Human chat interface plugin
- [ ] Dynamic plugin loading (`.so` / `.dylib`)
- [ ] Task management commands
- [ ] `atm mcp` command group (MCP server setup — section 4.8)

---

## 11. Open Questions

1. **Concurrent team-lead session policy**: For `atm register <team>`, should conflicts always block by default with `--force` takeover, and should optional `--kill` require explicit user confirmation every time?

2. **Inbox file creation**: If an agent doesn't have an inbox file yet, should `atm send` create it? Or error?

3. **Plugin trait in MVP?**: Should the plugin trait definition live in `atm-core` (available to MVP) even though no plugins exist until the daemon? This would let third parties develop against the trait early.

4. **Config file name**: `.atm.toml` (hidden, conventional) vs `atm.toml` (visible)?

5. **Large inbox strategy**: For inboxes with 10K+ messages, should `atm-core` support streaming JSON parsing, or is read-all-into-memory acceptable for MVP?

---

**Document Version**: 0.3
**Last Updated**: 2026-02-25
**Maintained By**: Claude
