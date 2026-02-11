# agent-team-mail (`atm`) — Project Plan

**Version**: 0.1
**Date**: 2026-02-11
**Status**: Draft

---

## 0. Team Lead Execution Loop (ARCH-ATM)

The project is driven by the main conversation agent acting as team lead (**ARCH-ATM**). ARCH-ATM creates a team named `atm-sprint` at the start of each phase, spawns scrum-master teammates per sprint, and orchestrates the full lifecycle. The team persists across sprints within a phase — only individual scrum-masters are shut down between sprints. `TeamDelete` is called only at phase end (after user review).

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
│    6. If CI passes → shutdown scrum-master            │
│       (team stays alive) → advance to next sprint   │
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

**Team lifecycle**: The `atm-sprint` team is created once per phase and persists across all sprints in that phase. Individual scrum-masters are shut down via `shutdown_request` (not `TeamDelete`), preserving the team's task list and inbox history. `TeamDelete` is called only at phase end after user review.

### 0.3 PR and Merge Policy

- **Phase integration branches**: Each phase gets an `integrate/phase-N` branch off `develop`. Sprint PRs target this integration branch, not `develop` directly.
- **Sprint PRs target `integrate/phase-N`** — created by the scrum-master at sprint completion
- **Phase completion PR targets `develop`** — one PR merging `integrate/phase-N → develop` after all phase sprints are complete
- **Only the user (randlee) merges PRs** — ARCH-ATM does not merge
- **Auto-advance**: ARCH-ATM advances to the next sprint once CI passes on the PR, without waiting for the merge — including across phase boundaries.
- **Dependent sprints**: When the next sprint depends on a previous sprint's code, ARCH-ATM branches the new worktree from the predecessor's PR branch (or the integration branch if the predecessor is already merged).
- **Independent sprints**: Worktree branches from `integrate/phase-N` HEAD.
- **After each sprint merges to integration branch**: Subsequent sprint branches must merge latest `integrate/phase-N` into their feature branch before creating their PR. This prevents merge conflicts.
- **PR rejection by user**: If the user requests changes on a PR, ARCH-ATM spawns a new scrum-master pointed at the existing worktree with the rejection context to address feedback.

### 0.4 Worktree Continuity

- **First sprint in phase** → new worktree branched from `integrate/phase-N`
- **Independent sprint** → new worktree branched from `integrate/phase-N`
- **Dependent sprint** → new worktree branched from predecessor's PR branch (or integration branch if predecessor merged)
- **CI failure on existing PR** → same worktree, same scrum-master
- **User-requested changes on merged PR** → new worktree for follow-up sprint
- **User-requested changes on open PR** → new scrum-master, same worktree

### 0.4a Worktree Cleanup Policy

**Worktrees are NOT cleaned up automatically.** The user reviews each sprint's worktree to check for design divergence before approving cleanup. ARCH-ATM only cleans up worktrees when explicitly requested by the user.

### 0.5 Parallel Sprints

When the dependency graph allows parallel sprints (e.g., 1.2, 1.3, 1.5 after 1.1), ARCH-ATM spawns **one scrum-master per parallel sprint**:

- Each parallel sprint gets its own worktree and its own scrum-master teammate
- Parallel sprints MUST be non-intersecting — different files/modules, no shared modifications
- Each scrum-master independently runs its dev-qa loop with its own background agents
- Each sprint produces its own PR targeting `integrate/phase-N`
- ARCH-ATM manages multiple scrum-master teammates concurrently
- After each sprint merges to `integrate/phase-N`, remaining sprints merge the integration branch into their feature branches before creating their PRs

```
Example: Phase 3 with integration branch

  integrate/phase-3 ◄── created from develop at phase start
    │
    ├── Sprint 3.1 (worktree A) ──► PR → integrate/phase-3 ──► merge
    │     (after merge, remaining sprints pull integrate/phase-3)
    ├── Sprint 3.2 (worktree B) ──► PR → integrate/phase-3 ──► merge
    └── Sprint 3.3 (worktree C) ──► PR → integrate/phase-3 ──► merge

  integrate/phase-3 ──► PR → develop (phase completion)
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

**Status**: ✅ Complete
**PR**: [#3](https://github.com/randlee/agent-team-mail/pull/3)
**Commit**: `95110c5`
**Completed**: 2026-02-10
**Dev-QA iterations**: 1 (TaskItem timestamp field naming fix)

---

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

**Status**: ✅ Complete
**PR**: [#7](https://github.com/randlee/agent-team-mail/pull/7)
**Commit**: `eefeeda`
**Completed**: 2026-02-11
**Dev-QA iterations**: 3 (analysis paralysis → tool access issue → successful implementation)
**Implementation**:
- 6 new modules in src/io/ (error, hash, atomic, lock, inbox, mod)
- BLAKE3 content hashing (not xxhash as originally specified)
- 49 tests pass, 0 failures
- Clippy clean, 0 warnings
- ~80-85% test coverage

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

**Status**: ✅ Complete
**PR**: [#8](https://github.com/randlee/agent-team-mail/pull/8)
**Commit**: `e169b1d`
**Completed**: 2026-02-11
**Dev-QA iterations**: 1 (clean implementation, QA passed on first review)
**Implementation**:
- New spool.rs module (551 lines) with SpooledMessage, spool_drain(), 7 tests
- Updated inbox.rs with team/agent parameters for inbox_append()
- 56 tests pass, 0 failures
- Clippy clean, 0 warnings
- Comprehensive test coverage covering all critical paths

---

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

**Status**: ✅ Complete
**PR**: [#10](https://github.com/randlee/agent-team-mail/pull/10)
**Commit**: `6850207`
**Completed**: 2026-02-11
**Dev-QA iterations**: 2 (initial implementation + clippy error fixes)
**Implementation**:
- 8 new source files (1295 insertions: main, commands, util modules)
- 1 test file (12 integration test cases)
- 26 total tests pass (14 unit + 12 integration)
- Clippy clean, 0 warnings
- Full integration with atm-core Phase 1 APIs

---

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
**Status**: ✅ Complete (2026-02-11)

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

**Status**: ✅ Complete
**PR**: [#13](https://github.com/randlee/agent-team-mail/pull/13)
**Commit**: `64a54f5`
**Completed**: 2026-02-11
**Dev-QA iterations**: 1 (clippy formatting fixes on first pass)
**Implementation**:
- 4 new command modules (teams, members, status, config_cmd)
- 14 integration tests (integration_discovery.rs)
- 41 total tests pass (15 unit + 14 discovery + 12 send)
- Clippy clean, 0 warnings
- Parallel sprint compliance: Perfect (no modifications to send.rs or atm-core)

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

**Goal**: End-to-end validation, conflict scenarios, fix design review findings, polish.

**Branch prefix**: `feature/p3-*`
**Depends on**: Phase 2 complete
**Integration branch**: `integrate/phase-3` (created from `develop` at phase start)

### Sprint 3.0: ARCH-CTM Design Review Fixes (Hotfix)

**Branch**: `feature/p3-s0-design-fixes`
**Depends on**: Phase 2 complete
**Priority**: Must complete before Sprint 3.1

**Background**: External architecture review (ARCH-CTM) identified correctness issues in Phase 1-2 code. These fixes address data integrity bugs that would undermine Phase 3 testing.

**Deliverables**:
- **[CRITICAL] Fix non-atomic read marking** (`crates/atm/src/commands/read.rs`):
  - Create `inbox_update()` function in `atm-core/src/io/inbox.rs` for atomic read-modify-write
  - Refactor lock/hash/swap logic from `inbox_append` into shared helper
  - Replace bare `std::fs::write` in `read.rs` with `inbox_update()` call
  - Add concurrent read/write test
- **[HIGH] Fix spool drain message loss** (`crates/atm-core/src/io/spool.rs`):
  - Capture `WriteOutcome` from `inbox_append` in `process_spooled_message`
  - Treat `WriteOutcome::Queued` as NOT delivered — keep original spool file pending, update retry metadata (increment `retry_count`, update `last_attempt`)
  - Do NOT delete spool file on `Queued` (current behavior permanently drops the message)
  - Add test: force `inbox_append` to return `Queued`, verify spool file remains in `pending/` with incremented retry count
- **[LOW] Fix task status counting** (`crates/atm/src/commands/status.rs`):
  - Replace string matching with `serde_json::from_str::<TaskItem>()` parsing
  - Handle `TaskStatus::Completed`, `Deleted`, and pending states properly

**Acceptance criteria**:
- `inbox_update()` uses full atomic write infrastructure (lock, hash, swap, conflict detection)
- Spool drain keeps spool file pending on `Queued` outcome (no message loss)
- Task status counts match actual task file schema
- All existing tests pass, new tests cover the fixed scenarios

**Status**: ✅ Complete (PR pending)
**Dev-QA iterations**: 1 (passed first QA review)
**Implementation notes**:
- Created `inbox_update()` with atomic write infrastructure, extracted shared helper `atomic_write_with_conflict_check()`
- Fixed spool drain to handle `WriteOutcome::Queued` properly - keeps spool file pending, no message loss
- Replaced task status string matching with proper `TaskItem` JSON parsing
- Added 3 new tests: concurrent inbox updates, spool drain queued handling, proper coverage
- 168 tests passing, clippy clean, cross-platform compliant

### Sprint 3.1: End-to-End Integration Tests

**Branch**: `feature/p3-s1-e2e-tests`
**Depends on**: Sprint 3.0

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

**Status**: ✅ Complete
**PR**: [#16](https://github.com/randlee/agent-team-mail/pull/16)
**Commit**: `f2b2005`
**Completed**: 2026-02-11
**Dev-QA iterations**: 1 (passed first QA review)
**Implementation**:
- New test file: `crates/atm/tests/integration_e2e_workflows.rs` (20 E2E workflow tests)
- Send → Read → Mark-as-read → Verify workflows (7 tests)
- Broadcast → Read all inboxes workflows (4 tests)
- Config resolution integration tests (5 tests)
- Complex multi-step workflows (4 tests: conversation, team discussion, cross-team relay, inbox summary)
- 188 tests passing (168 baseline + 20 new), 0 failures
- Clippy clean, 0 warnings
- Cross-platform compliant (ATM_HOME pattern, no HOME/USERPROFILE)
- All tests run in isolation with temp directories

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
- **[ARCH-CTM Fix] File policy repo root + destination fallback** (`crates/atm/src/util/file_policy.rs`):
  - Use repo root (walk up to `.git`) instead of CWD for `is_file_in_repo` checks
  - Resolve file policy against **destination repo context** when available
  - If destination repo is unknown, fall back to **deny + copy with notice** (safest behavior)
  - Add test for subdirectory file reference validation
  - Add test for unknown-destination fallback (deny + copy)
- **[NEW] Offline recipient detection on `atm send`** (`crates/atm/src/commands/send.rs`):
  - Before sending, check `config.json` for recipient: if member not found or `isActive == false`, recipient is offline
  - Warn sender: "Agent X appears offline. Message will be queued with call-to-action."
  - Prepend `[{action_text}] ` to message body. Action text resolved from (highest priority first):
    1. `--offline-action "custom text"` CLI flag
    2. `offline_action` property in config (`.atm.toml` `[messaging]` section)
    3. Hardcoded default: `PENDING ACTION - execute when online`
  - If resolved action text is empty string (`""`): skip prepend entirely (explicit opt-out)
  - Still deliver the message (write to inbox file) — warning is informational, not a hard block
  - Add tests: offline detection, auto-tagging, custom flag, config override, empty-string opt-out
- **[NEW] Fix `members`/`status` active label** (`crates/atm/src/commands/members.rs`, `status.rs`):
  - Rename display from "Active"/"Idle" and "Yes"/"No" to "Online"/"Offline"
  - `isActive: false` means shut down, not idle — current labels are misleading

**Acceptance criteria**:
- No data loss in any concurrent scenario
- Spool delivery works end-to-end
- Performance acceptable for large inboxes
- All edge cases handled gracefully
- File policy correctly resolves from subdirectories
- Offline recipients are detected and messages auto-tagged on send
- File policy denies + copies when destination repo is unknown

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
- **[ARCH-CTM Fix] Settings repo-root traversal** (`crates/atm-core/src/config/discovery.rs`):
  - `resolve_settings` walks from CWD up to git root to find `.claude/settings*.json`
  - Use **override semantics** (not merge): highest-precedence file wins, matching Claude Code behavior
  - Add test for settings discovery from subdirectory
- **[ARCH-CTM Note] Config command source reporting** (`crates/atm/src/commands/config_cmd.rs`):
  - Document that source is heuristic (ignores env/CLI overrides) or fix if straightforward

**Acceptance criteria**:
- All `--help` text is clear and complete
- Error messages are actionable
- `cargo doc` produces no warnings
- Settings resolution works from any subdirectory within a repo

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
    └── Sprint 3.0 (Design Review Fixes - HOTFIX)
            │
            └── Sprint 3.1 (E2E Tests)
                    │
                    ├── Sprint 3.2 (Conflict Tests + file policy fix) ──┐
                    │                                                    │
                    ├── Sprint 3.3 (Docs + Polish + settings fix) ──────┤
                    │                                                    │
                    └── Sprint 3.4 (Retention) ─────────────────────────┘
                                                                         │
                                                         MVP Complete ───┘
```

### Deferred to Phase 4+

- **Managed settings policy paths** (Finding 3b): Platform-specific managed policy directories (`/Library/Application Support/ClaudeCode/`, `/etc/claude-code/`, `%PROGRAMDATA%\ClaudeCode\`). Uncommon in practice; defer until daemon or enterprise features.
- **Destination repo file policy — full resolution** (Finding 4b): Full resolution of destination team's repo context requires schema extension to `TeamConfig` for repo path. Sprint 3.2 adds safe fallback (deny + copy when destination unknown). Full schema extension deferred until cross-repo use cases are implemented.
- **Windows atomic swap fsync** (ARCH-CTM note): Current best-effort behavior is documented. Full fsync would require `FlushFileBuffers` on Windows. Low priority.

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
