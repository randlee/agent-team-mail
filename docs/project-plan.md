# agent-team-mail (`atm`) — Project Plan

**Version**: 0.1
**Date**: 2026-02-11
**Status**: Draft

---

## 0. Team Lead Execution Loop (ARCH-ATM)

The project is driven by the main conversation agent acting as team lead (**ARCH-ATM**). ARCH-ATM creates a team named `atm-sprint`, spawns scrum-master teammates for sprints, and orchestrates the full lifecycle. ARCH-ATM auto-advances across phase boundaries as long as dependencies are met and CI passes.

### 0.1 Sprint Loop

```
┌─────────────────────────────────────────────────────┐
│                  ARCH-ATM Loop                      │
│                                                     │
│  for each sprint in dependency order:               │
│                                                     │
│    1. Spawn scrum-master teammate                   │
│    2. Assign sprint (deliverables, branch, refs)    │
│    3. Scrum-master runs dev-qa loop                 │
│    4. Scrum-master creates PR → develop             │
│    5. ARCH-ATM verifies:                            │
│       - PR created and CI passes                    │
│       - docs/project-plan.md updated                │
│    6. If CI passes → shutdown scrum-master           │
│       → advance to next sprint                      │
│    7. If CI fails → scrum-master addresses failures  │
│       on the same worktree (do not restart)         │
│    8. If unresolvable → escalate to user, stop      │
│                                                     │
│  Stop conditions:                                   │
│    - Architect escalation requiring user decision   │
│    - Issue that can't be resolved autonomously      │
│    - Reality doesn't match requirements             │
│    - All project sprints complete                    │
└─────────────────────────────────────────────────────┘
```

### 0.2 Scrum-Master Lifecycle

Each sprint gets a **fresh scrum-master** with clean context:

| Event | Action |
|-------|--------|
| Sprint start | ARCH-ATM spawns scrum-master with sprint assignment |
| PR created, CI green | ARCH-ATM shuts down scrum-master, advances |
| CI failure | Same scrum-master iterates on existing worktree |
| QA rejection (dev loop) | Same scrum-master continues dev-qa loop |
| Unresolvable issue | Scrum-master escalates to architect → user; ARCH-ATM stops |

**Why restart between sprints**: Fresh context prevents prompt bloat and cross-sprint confusion. Each scrum-master sees only its sprint's requirements, not the accumulated history of prior sprints.

### 0.3 PR and Merge Policy

- **PRs target `develop`** — created by the scrum-master at sprint completion
- **Only the user (randlee) merges PRs** — ARCH-ATM does not merge
- **Auto-advance**: ARCH-ATM advances to the next sprint once CI passes on the PR, without waiting for the merge — including across phase boundaries.
- **Dependent sprints**: When the next sprint depends on a previous sprint's code (e.g., 1.3 → 1.4), ARCH-ATM branches the new worktree from the predecessor's PR branch (not `develop`). This avoids waiting for merge while preserving the dependency chain.
- **Independent sprints**: When the next sprint has no code dependency on the previous one, the worktree branches from `develop` HEAD as normal.
- **PR rejection by user**: If the user requests changes on a PR, ARCH-ATM spawns a new scrum-master pointed at the existing worktree with the rejection context to address feedback.

### 0.4 Worktree Continuity

- **Independent sprint** → new worktree branched from `develop`
- **Dependent sprint** → new worktree branched from predecessor's PR branch
- **CI failure on existing PR** → same worktree, same scrum-master
- **User-requested changes on merged PR** → new worktree for follow-up sprint
- **User-requested changes on open PR** → new scrum-master, same worktree

### 0.5 Parallel Sprints

When the dependency graph allows parallel sprints (e.g., 1.2, 1.3, 1.5 after 1.1), ARCH-ATM spawns **one scrum-master per parallel sprint**:

- Each parallel sprint gets its own worktree and its own scrum-master teammate
- Parallel sprints MUST be non-intersecting — different files/modules, no shared modifications
- Each scrum-master independently runs its dev-qa loop with its own background agents
- Each sprint produces its own PR targeting `develop`
- ARCH-ATM manages multiple scrum-master teammates concurrently

**Merge sprint**: After all parallel sprints in a group complete and their PRs are merged, a small **integration sprint** follows to:
- Verify all parallel branches integrate cleanly on `develop`
- Run the full test suite across combined changes
- Resolve any unexpected interactions between parallel work
- This is a lightweight sprint — no new features, just validation and conflict resolution

```
Example: Phase 1 after Sprint 1.1

  Sprint 1.2 (worktree A) ──► PR → develop ──┐
  Sprint 1.3 (worktree B) ──► PR → develop ──┼──► Integration sprint
  Sprint 1.5 (worktree C) ──► PR → develop ──┘    (verify + resolve)
```

---

## 1. Execution Model

### 1.1 Agent Team Structure

Each sprint is executed by an agent team with the following roles:

```
┌─────────────────────────────────────────────────────┐
│                  Human (Randy)                      │
│  Approves plans, reviews escalations, merges PRs    │
└─────────────────────┬───────────────────────────────┘
                      │
         ┌────────────▼────────────┐
         │     Scrum Master        │
         │     (Sonnet/Opus)       │
         │                         │
         │  - Owns sprint quality  │
         │  - Runs dev-qa loop     │
         │  - Validates against    │
         │    arch & requirements  │
         │  - Escalates to Opus    │
         │    architect if needed  │
         └──┬──────────────┬───────┘
            │              │
   ┌────────▼──────┐  ┌───▼──────────┐
   │  Rust Dev(s)  │  │  Rust QA(s)  │
   │  (Sonnet)     │  │  (Sonnet)    │
   │               │  │              │
   │  - Implement  │  │  - Code      │
   │  - Write tests│  │    review    │
   │  - Fix issues │  │  - Corner    │
   │               │  │    cases     │
   └───────────────┘  │  - 100% pass │
                      └──────────────┘

   ┌─────────────────────────┐
   │  Opus Rust Architect    │
   │  (On-demand escalation) │
   │                         │
   │  - Architecture review  │
   │  - Complex decisions    │
   │  - Quality gate for     │
   │    human escalation     │
   └─────────────────────────┘
```

### 1.2 Dev-QA Loop

Every sprint follows this cycle:

```
                    ┌──────────┐
                    │  Sprint  │
                    │  Start   │
                    └────┬─────┘
                         │
                ┌────────▼────────┐
                │ Scrum Master    │
                │ reviews plan    │
                │ against reqs    │
                └────────┬────────┘
                         │
                ┌────────▼────────┐
                │ Dev: implement  │
                │ + write tests   │
                └────────┬────────┘
                         │
                ┌────────▼────────┐
          ┌─────│ QA: review +    │
          │     │ validate tests  │
          │     └────────┬────────┘
          │              │
          │     ┌────────▼────────┐
          │  No │ All checks pass?│
          ├─────│                 │
          │     └────────┬────────┘
          │              │ Yes
          │     ┌────────▼────────┐
          │     │ Scrum Master    │
          │     │ commit/push/PR  │
          │     └────────┬────────┘
          │              │
          │     ┌────────▼────────┐
          │     │  Sprint Done    │
          │     └─────────────────┘
          │
          │     ┌─────────────────┐
          └────►│ Dev: fix issues │
                └────────┬────────┘
                         │
                         └──► (back to QA)
```

**QA checks:**
- Code review against sprint plan and architecture
- Sufficient unit test coverage (especially corner cases)
- 100% tests pass (`cargo test`)
- Clippy clean (`cargo clippy -- -D warnings`)
- Code follows Pragmatic Rust Guidelines
- CI matrix covers macOS, Linux, and Windows

**Escalation path:**
- QA failures → Dev fixes (normal loop)
- Significant quality/architecture issues → Scrum Master escalates to Opus Rust Architect
- Opus Architect reviews and provides assessment → Scrum Master decides if human escalation needed
- Human never sees an issue that Opus Architect hasn't thoroughly assessed

### 1.3 Worktree Isolation

**All sprint work MUST use dedicated worktrees** via `sc-git-worktree` skill:

```bash
# Create worktree for sprint
/sc-git-worktree --create feature/phase1-sprint1-schema-types develop

# All dev work happens in the worktree
# Main repo stays on develop at all times

# After sprint completes, PR targets develop
```

**Parallel sprints** use separate worktrees and can run concurrently as long as they don't
modify the same files. The plan identifies dependencies explicitly.

### 1.4 Model Selection

| Role | Model | Rationale |
|------|-------|-----------|
| Scrum Master | Sonnet (Opus for escalation) | Coordination, review, process |
| Rust Dev | Sonnet | Implementation, test writing |
| Rust QA | Sonnet | Code review, test validation |
| Rust Architect | Opus | Complex architecture decisions, escalation review |

---

## 2. Phase Overview

```
Phase 1: Foundation (atm-core)
  └─ Schema types, file I/O, atomic swap, config
  └─ 5 sprints, ~2 parallel tracks

Phase 2: CLI (atm)
  └─ Command structure, messaging, discovery
  └─ 4 sprints, ~2 parallel tracks

Phase 3: Integration & Hardening
  └─ End-to-end tests, conflict scenarios, polish
  └─ 3 sprints, mostly sequential

Phase 4: Daemon Foundation (atm-daemon)
  └─ Plugin trait, registry, daemon loop
  └─ 3 sprints, ~2 parallel tracks

Phase 5: First Plugin (Issues)
  └─ Provider abstraction, GitHub/Azure impl
  └─ 3 sprints, sequential

Phase 6: Additional Plugins
  └─ CI Monitor, Bridge, others
  └─ Open-ended, parallel per plugin
```

---

## 3. Phase 1: Foundation (`atm-core`)

**Goal**: Shared library with schema types, safe file I/O, config, and system context.

**Branch prefix**: `feature/p1-*`

### Sprint 1.1: Workspace Setup + Schema Types

**Branch**: `feature/p1-s1-workspace-schema`
**Depends on**: None (first sprint)
**Parallel**: Can start immediately

**Deliverables**:
- Cargo workspace with `atm-core` and `atm` crates (atm-daemon placeholder)
- Schema types: `TeamConfig`, `AgentMember`, `InboxMessage`, `TaskItem`
- Schema types for Claude Code settings (`SettingsJson`, `Permissions`, `Env`) based on documented `settings.json`
- `#[serde(flatten)]` for unknown field preservation
- `message_id: Option<String>` on `InboxMessage` for dedup
- Round-trip tests: parse → serialize → parse produces identical output
- Schema evolution tests: unknown fields preserved, missing optionals handled
- Tests cover all schemas documented in `docs/agent-team-api.md`
- GitHub CI workflow that runs unit tests on PRs (and updates) targeting `develop` or `main`

**Acceptance criteria**:
- `cargo build` succeeds for workspace
- All schema types serialize/deserialize correctly
- Unknown fields round-trip without loss
- `cargo clippy -- -D warnings` clean
- `cargo test` 100% pass
- CI triggers on PRs to `develop` and `main` and runs tests

### Sprint 1.2: Schema Version Detection

**Branch**: `feature/p1-s2-schema-version`
**Depends on**: Sprint 1.1
**Parallel**: Can run alongside Sprint 1.3 (after 1.1 completes)

**Deliverables**:
- `SchemaVersion` enum (`PreRelease`, `Stable`, `Unknown`)
- Claude Code version detection (`claude --version` with subprocess)
- Version cache at `~/.config/atm/claude-version.json`
- Mapping from Claude version → schema compatibility
- Logging: warn on unknown fields, error on missing required fields

**Acceptance criteria**:
- Detects installed Claude Code version
- Caches and reuses version (no subprocess on every call)
- Handles missing `claude` binary gracefully
- Tests cover all SchemaVersion variants

### Sprint 1.3: Atomic File I/O

**Branch**: `feature/p1-s3-atomic-io`
**Depends on**: Sprint 1.1
**Parallel**: Can run alongside Sprint 1.2 (after 1.1 completes)

**Deliverables**:
- Platform-specific atomic swap (`renamex_np` on macOS, `renameat2` on Linux)
- Windows fallback strategy (best-effort replace + fsync where supported)
- `flock` advisory locking (non-blocking with `LOCK_NB`)
- Content hashing for conflict detection (xxhash or similar)
- Conflict detection: hash before/after swap, merge if mismatch
- `inbox_append()` public API with `WriteOutcome` return
- Retry logic with exponential backoff (50ms, 100ms, 200ms, 400ms, 800ms)
- Graceful handling: missing files created, empty files initialized, malformed JSON recovered

**Acceptance criteria**:
- Atomic swap works on macOS (primary platform)
- Windows build passes and uses the documented fallback path
- Lock contention detected and handled (EWOULDBLOCK)
- Conflict detection catches simulated concurrent writes
- Tests simulate: clean write, lock contention, conflict merge, malformed JSON
- No data loss in any scenario
- `cargo clippy` clean, `cargo test` 100% pass

**Windows fallback guidance (for devs)**:
- No `RENAME_SWAP` equivalent; use temp-file write + fsync + atomic replace on same volume.
- Keep conflict detection and merge logic identical to macOS/Linux.
- Use a platform-appropriate file lock implementation on Windows (do not rely on `flock`).

### Sprint 1.4: Outbound Spool + Guaranteed Delivery

**Branch**: `feature/p1-s4-spool`
**Depends on**: Sprint 1.3
**Parallel**: Can run alongside Sprint 1.5 (after respective dependencies)

**Deliverables**:
- Spool directory management (`~/.config/atm/spool/pending/`, `failed/`)
- `inbox_append()` integration: on write failure → spool message
- `spool_drain()` function: retry pending messages with backoff
- Duplicate detection via `message_id` in inbox
- Failed message handling: move to `failed/` after max retries
- `SpoolStatus` return type with delivery/pending/failed counts

**Acceptance criteria**:
- Messages survive write failure (spooled to disk)
- `spool_drain()` delivers spooled messages
- Duplicates detected and skipped
- Failed messages moved after max retries
- Tests simulate: write failure → spool → drain → delivery

### Sprint 1.5: System Context + Config

**Branch**: `feature/p1-s5-context-config`
**Depends on**: Sprint 1.1
**Parallel**: Can run alongside Sprints 1.2, 1.3 (after 1.1 completes)

**Deliverables**:
- `SystemContext` struct with all fields
- `RepoContext` with git remote parsing
- `GitProvider` enum detection from remote URL (GitHub, Azure DevOps, GitLab, Bitbucket, Unknown)
- Config resolution: flags → env → repo `.atm.toml` → global `~/.config/atm/config.toml` → defaults
- Config file parsing with serde + toml
- Environment variable support (`ATM_TEAM`, `ATM_IDENTITY`, `ATM_CONFIG`, `ATM_NO_COLOR`)
- Claude Code settings discovery for file access policy:
  managed policy → CLI args → `.claude/settings.local.json` → `.claude/settings.json` → `~/.claude/settings.json`

**Acceptance criteria**:
- All git providers detected correctly from URLs
- Config resolution follows priority order
- Missing config files handled gracefully (use defaults)
- Tests cover all providers, config priority, missing files
- `.claude/settings.local.json` and `.claude/settings.json` handled gracefully when absent or malformed
- Settings precedence is enforced with tests for each layer

### Phase 1 Dependency Graph

```
Sprint 1.1 (Workspace + Schema)
    │
    ├── Sprint 1.2 (Schema Version) ──────────────────────┐
    │                                                      │
    ├── Sprint 1.3 (Atomic I/O) ── Sprint 1.4 (Spool) ───┤
    │                                                      │
    └── Sprint 1.5 (Context + Config) ────────────────────┘
                                                           │
                                              Phase 1 Complete
```

**Parallel tracks after Sprint 1.1**:
- Track A: Sprint 1.2 (schema version)
- Track B: Sprint 1.3 → Sprint 1.4 (I/O → spool)
- Track C: Sprint 1.5 (context + config)

---

## 4. Phase 2: CLI (`atm`)

**Goal**: Functional CLI binary with all messaging and discovery commands.

**Branch prefix**: `feature/p2-*`
**Depends on**: Phase 1 complete

### Sprint 2.1: CLI Skeleton + Send Command

**Branch**: `feature/p2-s1-cli-send`
**Depends on**: Phase 1 complete
**Parallel**: Can start immediately after Phase 1

**Deliverables**:
- `atm` binary crate with clap derive
- `atm send <agent> <message>` command
- `atm send <agent>@<team> <message>` cross-team addressing
- `--team`, `--summary`, `--json`, `--dry-run` flags
- `--file` and `--stdin` message input
- `--file` is reference-only; never embeds file content in inbox JSON
- Enforce repo-root and `.claude/settings.json` file access policy
- If access not permitted for destination repo, copy to share folder and rewrite message with explicit “copy” notice
- Integration with `atm-core::inbox_append()`
- Error reporting (agent not found, team not found, write failure)

**Acceptance criteria**:
- `atm send` writes message to correct inbox file
- Cross-team addressing works
- All flags functional
- File reference policy enforced (including copy + rewrite notice)
- Integration tests with temp fixtures

**Implementation checklist (file references + settings)**:
- Resolve settings precedence in this order: managed → CLI args → `.claude/settings.local.json` → `.claude/settings.json` → `~/.claude/settings.json`
- Enforce repo-root path constraint by default
- If path not permitted for destination repo, copy to `~/.config/atm/share/<team>/` and rewrite the message with an explicit copy notice
- Never embed file contents in inbox JSON

### Sprint 2.2: Read + Inbox Commands

**Branch**: `feature/p2-s2-read-inbox`
**Depends on**: Sprint 2.1
**Parallel**: Can run alongside Sprint 2.3 (after 2.1 completes)

**Deliverables**:
- `atm read` command with all flags (`--all`, `--no-mark`, `--limit`, `--since`, `--from`, `--json`)
- Mark-as-read behavior (atomic write back)
- `atm inbox` command with summary table
- `--team`, `--all-teams` flags
- Human-readable output formatting (relative timestamps, aligned table)

**Acceptance criteria**:
- Read displays unread messages by default
- Mark-as-read updates file atomically
- Inbox summary shows correct counts
- All filtering flags work
- Integration tests

### Sprint 2.3: Broadcast Command

**Branch**: `feature/p2-s3-broadcast`
**Depends on**: Sprint 2.1
**Parallel**: Can run alongside Sprint 2.2 (after 2.1 completes)

**Deliverables**:
- `atm broadcast <message>` command
- `--team` flag
- Per-agent delivery status reporting
- Handles partial delivery failure (some agents succeed, some fail)

**Acceptance criteria**:
- Message delivered to all team members
- Partial failure reported clearly
- Integration tests

### Sprint 2.4: Discovery Commands

**Branch**: `feature/p2-s4-discovery`
**Depends on**: Sprint 2.1
**Parallel**: Can run alongside Sprints 2.2, 2.3

**Deliverables**:
- `atm teams` — list all teams
- `atm members [team]` — list agents in team
- `atm status [team]` — combined overview
- `atm config` — show effective configuration
- Human-readable and `--json` output for all commands

**Acceptance criteria**:
- All commands produce correct output from fixture data
- Handles empty teams, missing teams, no teams gracefully
- Integration tests

### Phase 2 Dependency Graph

```
Phase 1 Complete
    │
    └── Sprint 2.1 (CLI Skeleton + Send)
            │
            ├── Sprint 2.2 (Read + Inbox) ─────────┐
            │                                       │
            ├── Sprint 2.3 (Broadcast) ─────────────┤
            │                                       │
            └── Sprint 2.4 (Discovery) ─────────────┘
                                                    │
                                       Phase 2 Complete
```

**Parallel tracks after Sprint 2.1**:
- Track A: Sprint 2.2 (read/inbox)
- Track B: Sprint 2.3 (broadcast)
- Track C: Sprint 2.4 (discovery)

---

## 5. Phase 3: Integration & Hardening

**Goal**: End-to-end validation, conflict scenarios, polish.

**Branch prefix**: `feature/p3-*`
**Depends on**: Phase 2 complete

### Sprint 3.1: End-to-End Integration Tests

**Branch**: `feature/p3-s1-e2e-tests`
**Depends on**: Phase 2 complete

**Deliverables**:
- Full CLI workflow tests (send → read → mark-as-read → verify)
- Cross-team messaging tests
- Broadcast → read all inboxes tests
- Config resolution integration tests
- CI matrix for macOS, Linux, and Windows
- Tests against real `~/.claude/teams/` structure (with temp directory)

**Acceptance criteria**:
- All workflows pass end-to-end
- Tests run in isolation (temp directories, no side effects)

### Sprint 3.2: Conflict & Edge Case Testing

**Branch**: `feature/p3-s2-conflict-tests`
**Depends on**: Sprint 3.1
**Parallel**: Can run alongside Sprint 3.3

**Deliverables**:
- Simulated concurrent write tests (multi-threaded)
- Lock contention scenarios
- Spool → drain → delivery cycle tests
- Malformed JSON recovery tests
- Large inbox performance tests (10K+ messages)
- Missing file / empty file / permission denied scenarios
- Settings schema parse/round-trip tests for `.claude/settings.json`

**Acceptance criteria**:
- No data loss in any concurrent scenario
- Spool delivery works end-to-end
- Performance acceptable for large inboxes
- All edge cases handled gracefully

### Sprint 3.3: Documentation & Polish

**Branch**: `feature/p3-s3-docs-polish`
**Depends on**: Sprint 3.1
**Parallel**: Can run alongside Sprint 3.2

**Deliverables**:
- `--help` text polished for all commands
- Error messages reviewed for clarity
- README.md with quickstart
- `cargo doc` generates clean documentation
- Version info in `atm --version`

**Acceptance criteria**:
- All `--help` text is clear and complete
- Error messages are actionable
- `cargo doc` produces no warnings

### Sprint 3.4: Inbox Retention and Cleanup

**Branch**: `feature/p3-s4-retention`
**Depends on**: Sprint 3.1
**Parallel**: Can run alongside Sprint 3.2 or 3.3

**Deliverables**:
- Configurable retention policy (max age and/or max message count)
- Default cleanup for non-Claude-managed inboxes
- Optional cleanup for Claude-managed inboxes (configurable)
- Archive or delete strategy with tests

**Acceptance criteria**:
- Inboxes are bounded by configured policy
- Non-Claude inbox cleanup runs without data loss outside policy
- Tests cover retention by age and by count

### Phase 3 Dependency Graph

```
Phase 2 Complete
    │
    └── Sprint 3.1 (E2E Tests)
            │
            ├── Sprint 3.2 (Conflict Tests) ───────┐
            │                                      │
            ├── Sprint 3.3 (Docs + Polish) ────────┤
            │                                      │
            └── Sprint 3.4 (Retention) ────────────┘
                                                    │
                                    MVP Complete ────┘
```

---

## 6. Phase 4: Daemon Foundation (`atm-daemon`)

**Goal**: Daemon binary with plugin infrastructure, no concrete plugins yet.

**Branch prefix**: `feature/p4-*`
**Depends on**: Phase 3 complete (MVP)

### Sprint 4.1: Plugin Trait + Registry

**Branch**: `feature/p4-s1-plugin-trait`
**Depends on**: Phase 3 complete

**Deliverables**:
- `Plugin` async trait definition
- `PluginMetadata`, `Capability`, `PluginError` types
- `PluginContext` with `SystemContext`, `MailService`, `RosterService`
- `inventory`-based plugin registration
- Plugin factory and lifecycle management

**Acceptance criteria**:
- Trait compiles and is implementable
- Mock plugin can be registered and discovered
- Plugin context provides access to atm-core services

### Sprint 4.2: Daemon Event Loop

**Branch**: `feature/p4-s2-daemon-loop`
**Depends on**: Sprint 4.1
**Parallel**: Can run alongside Sprint 4.3

**Deliverables**:
- `atm-daemon` binary crate with tokio runtime
- Plugin init → run → shutdown lifecycle
- Cancellation token propagation (SIGINT/SIGTERM → cancel)
- Spool drain loop (periodic retry of pending messages)
- Inbox file watching (kqueue/inotify for event-driven plugins)
- Graceful shutdown with timeout

**Acceptance criteria**:
- Daemon starts, loads plugins, runs event loop
- SIGINT triggers graceful shutdown
- Spool is drained on interval
- Mock plugin receives init/run/shutdown calls

### Sprint 4.3: Roster Service

**Branch**: `feature/p4-s3-roster`
**Depends on**: Sprint 4.1
**Parallel**: Can run alongside Sprint 4.2

**Deliverables**:
- `RosterService`: add/remove synthetic members in team config
- Atomic config.json updates (same swap pattern)
- Plugin membership tracking (which plugin owns which members)
- Cleanup on plugin shutdown (remove plugin's members)

**Acceptance criteria**:
- Synthetic members appear in config.json
- Other agents can message synthetic members
- Members cleaned up on plugin shutdown
- Tests cover add/remove/cleanup

### Phase 4 Dependency Graph

```
MVP Complete (Phase 3)
    │
    └── Sprint 4.1 (Plugin Trait + Registry)
            │
            ├── Sprint 4.2 (Daemon Loop) ──────────┐
            │                                       │
            └── Sprint 4.3 (Roster Service) ────────┘
                                                    │
                                     Phase 4 Complete
```

---

## 7. Phase 5: First Plugin (Issues)

**Goal**: Working Issues plugin with at least one provider (GitHub or Azure DevOps).

**Branch prefix**: `feature/p5-*`
**Depends on**: Phase 4 complete

### Sprint 5.1: Provider Abstraction

**Branch**: `feature/p5-s1-provider-abstraction`
**Depends on**: Phase 4 complete

**Deliverables**:
- Provider trait for issue operations (list, get, comment)
- GitHub provider implementation (using `gh` CLI or API)
- Azure DevOps provider stub (or implementation if straightforward)
- Provider selection from `ctx.system.repo.provider`

### Sprint 5.2: Issues Plugin Core

**Branch**: `feature/p5-s2-issues-plugin`
**Depends on**: Sprint 5.1

**Deliverables**:
- Issues plugin implementing `Plugin` trait
- Poll loop watching for new/updated issues
- Issue → inbox message transformation
- Inbox reply → issue comment flow
- Configurable filters (labels, assignees)

### Sprint 5.3: Issues Plugin Testing

**Branch**: `feature/p5-s3-issues-tests`
**Depends on**: Sprint 5.2

**Deliverables**:
- Mock provider for testing (no real API calls)
- End-to-end: issue created → message delivered → reply → comment posted
- Error scenarios: API failure, auth failure, rate limit
- Configuration validation

---

## 8. Phase 6: Additional Plugins

**Goal**: Expand plugin ecosystem. Sprints are independent and parallel per plugin.

**Branch prefix**: `feature/p6-*`
**Depends on**: Phase 4 complete (daemon infrastructure)

Planned plugins (each is a self-contained sprint series):

| Plugin | Priority | Depends On | Notes |
|--------|----------|------------|-------|
| CI Monitor | High | Phase 4 | Existing design doc available |
| Cross-Computer Bridge | High | Phase 4 | Enables multi-machine teams |
| Human Chat Interface | Medium | Phase 4 | Slack/Discord integration |
| Beads Mail | Medium | Phase 4 | [steveyegge/beads](https://github.com/steveyegge/beads) — Gastown integration |
| MCP Agent Mail | Medium | Phase 4 | [Dicklesworthstone/mcp_agent_mail](https://github.com/Dicklesworthstone/mcp_agent_mail) — MCP interop |

Sprint planning for Phase 6 plugins will be done when Phase 4 is complete and the plugin
infrastructure is proven.

---

## 9. Sprint Summary

| Phase | Sprint | Name | Depends On | Parallel With |
|-------|--------|------|------------|---------------|
| **1** | 1.1 | Workspace + Schema Types | — | — |
| **1** | 1.2 | Schema Version Detection | 1.1 | 1.3, 1.5 |
| **1** | 1.3 | Atomic File I/O | 1.1 | 1.2, 1.5 |
| **1** | 1.4 | Outbound Spool | 1.3 | 1.5 |
| **1** | 1.5 | System Context + Config | 1.1 | 1.2, 1.3 |
| **2** | 2.1 | CLI Skeleton + Send | Phase 1 | — |
| **2** | 2.2 | Read + Inbox | 2.1 | 2.3, 2.4 |
| **2** | 2.3 | Broadcast | 2.1 | 2.2, 2.4 |
| **2** | 2.4 | Discovery Commands | 2.1 | 2.2, 2.3 |
| **3** | 3.1 | E2E Integration Tests | Phase 2 | — |
| **3** | 3.2 | Conflict & Edge Cases | 3.1 | 3.3 |
| **3** | 3.3 | Docs & Polish | 3.1 | 3.2 |
| **3** | 3.4 | Inbox Retention & Cleanup | 3.1 | 3.2, 3.3 |
| **4** | 4.1 | Plugin Trait + Registry | Phase 3 | — |
| **4** | 4.2 | Daemon Event Loop | 4.1 | 4.3 |
| **4** | 4.3 | Roster Service | 4.1 | 4.2 |
| **5** | 5.1 | Provider Abstraction | Phase 4 | — |
| **5** | 5.2 | Issues Plugin Core | 5.1 | — |
| **5** | 5.3 | Issues Plugin Testing | 5.2 | — |

**Total**: 18 sprints across 5 planned phases (Phase 6 is open-ended)

**Critical path**: 1.1 → 1.3 → 1.4 → 2.1 → 2.2 → 3.1 → 3.2 → 3.4 → MVP

**Maximum parallelism**:
- Phase 1: 3 concurrent sprints (1.2, 1.3, 1.5 after 1.1)
- Phase 2: 3 concurrent sprints (2.2, 2.3, 2.4 after 2.1)
- Phase 3: 2 concurrent sprints (3.2, 3.3 after 3.1)
- Phase 4: 2 concurrent sprints (4.2, 4.3 after 4.1)

---

## 10. Scrum Master Agent Prompt

The following prompt is used when spawning the scrum master agent for a sprint:

```
You are the Scrum Master for the agent-team-mail (atm) project.

## Your Responsibilities

1. **Sprint Planning**: Before dev work begins, review the sprint deliverables against
   docs/requirements.md and docs/project-plan.md. Verify the sprint scope is clear,
   achievable, and consistent with the architecture.

2. **Dev-QA Loop**: Coordinate the development and quality assurance cycle:
   - Assign implementation tasks to rust-dev agent(s)
   - After dev completes, assign review to rust-qa agent(s)
   - If QA finds issues, send them back to dev with specific feedback
   - Repeat until all QA checks pass
   - You own sprint quality — do not approve work that doesn't meet standards

3. **QA Standards** (non-negotiable):
   - Code review against sprint plan and architecture
   - Sufficient unit test coverage, especially corner cases
   - 100% tests pass: `cargo test`
   - Clippy clean: `cargo clippy -- -D warnings`
   - Code follows Pragmatic Rust Guidelines
   - Round-trip preservation of unknown JSON fields where applicable

4. **Worktree Discipline**: ALL work happens on a dedicated worktree created via
   sc-git-worktree skill. The main repo must remain on the develop branch at all times.
   Create the worktree FROM develop. PRs target develop.

5. **Escalation**: If you encounter significant quality issues, architecture concerns,
   or blocking problems that dev cannot resolve:
   - First: consult the Opus Rust Architect agent for a thorough assessment
   - Only after the architect has reviewed: escalate to the human with the architect's
     assessment and your recommendation
   - Never escalate to human without architect review first

6. **Commit/PR**: When QA passes, commit the work with a clear message, push, and
   create a PR targeting develop. Include sprint ID in the PR title.

## Project References

- Requirements: docs/requirements.md
- Project Plan: docs/project-plan.md
- Agent Team API: docs/agent-team-api.md
- Rust Guidelines: .claude/skills/rust-development/guidelines.txt

## Communication

- Use TaskCreate/TaskUpdate to track sprint tasks
- Send clear, specific feedback to dev and qa agents via SendMessage
- Report sprint status to human when complete or when escalation is needed
```

---

**Document Version**: 0.1
**Last Updated**: 2026-02-11
**Maintained By**: Claude
