# agent-team-mail (`atm`) — Requirements Document

**Version**: 0.2
**Date**: 2026-02-11
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
  config      Show/set configuration
  cleanup     Apply retention policies

Teams subcommands:
  teams add-member <team> <agent> [--agent-type <type>] [--model <model>] [--cwd <path>] [--inactive]
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

Before writing to the inbox, `atm send` checks the recipient's status in `config.json`:
- If the recipient is **not in the members array** or has **`isActive: false`**, the recipient is considered offline.
- When offline, `atm` prepends a call-to-action tag to the message body: `[{action_text}] {original_message}`
- The sender receives a warning: `Warning: Agent X appears offline. Message will be queued with call-to-action.`
- The message is still delivered (written to inbox file) — the warning is informational, not a hard block.

**Call-to-action text precedence** (highest to lowest):
1. `--offline-action "custom text"` CLI flag
2. `offline_action` property in config file (`.atm.toml` or `settings.json`)
3. Hardcoded default: `PENDING ACTION - execute when online`

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
- By default shows only unread messages (`read: false`)
- Marks displayed messages as `read: true` (atomic write back)
- `--no-mark` flag to read without marking

**Options**:

| Flag | Description |
|------|-------------|
| `--all` | Show all messages, not just unread |
| `--no-mark` | Don't mark messages as read |
| `--limit <n>` | Show only last N messages |
| `--since <timestamp>` | Show messages after timestamp |
| `--json` | Output as JSON |
| `--from <name>` | Filter by sender |

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
identity = "human"                  # from field on sent messages

[messaging]
offline_action = "PENDING ACTION - execute when online"  # call-to-action prepended for offline recipients
                                                         # set to "" to disable auto-tagging

[display]
format = "text"                     # text | json
color = true
timestamps = "relative"             # relative | absolute | iso8601
```

#### Environment Variables

| Variable | Description |
|----------|-------------|
| `ATM_TEAM` | Default team name |
| `ATM_IDENTITY` | Sender identity |
| `ATM_CONFIG` | Path to config file override |
| `ATM_NO_COLOR` | Disable colored output |

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

**Proposed direction (from Phase 6 review)**:
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
| Logging | `tracing` | Structured logging, compatible with daemon mode |
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

---

## 11. Open Questions

1. **CLI inbox identity**: When reading "own" messages via `atm read` (no agent specified), which inbox does it read? The configured `identity` name? `team-lead`? Or show a picker?

2. **Inbox file creation**: If an agent doesn't have an inbox file yet, should `atm send` create it? Or error?

3. **Plugin trait in MVP?**: Should the plugin trait definition live in `atm-core` (available to MVP) even though no plugins exist until the daemon? This would let third parties develop against the trait early.

4. **Config file name**: `.atm.toml` (hidden, conventional) vs `atm.toml` (visible)?

5. **Large inbox strategy**: For inboxes with 10K+ messages, should `atm-core` support streaming JSON parsing, or is read-all-into-memory acceptable for MVP?

---

**Document Version**: 0.2
**Last Updated**: 2026-02-11
**Maintained By**: Claude
