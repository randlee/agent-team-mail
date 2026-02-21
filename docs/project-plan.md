# agent-team-mail (`atm`) — Project Plan

**Version**: 0.3
**Date**: 2026-02-19
**Status**: Phase A (atm-agent-mcp) IN PROGRESS — 7/8 sprints merged, A.8 PR pending

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

Phase 5: First Plugin (Issues) ✅
  └─ Provider abstraction, pluggable architecture, testing
  └─ 5 sprints (3 core + pluggable providers + ARCH-CTM review)

Phase 6: Additional Plugins
  └─ CI Monitor (GitHub built-in + Azure external), Bridge
  └─ Sprint planning below
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

**Status**: ✅ Complete
**Completed**: 2026-02-11
**Dev-QA iterations**: 1 (passed first QA review)
**Implementation**:
- New test file: `crates/atm/tests/integration_conflict_tests.rs` (19 conflict/edge case tests)
- File policy repo root fix: walks up to `.git` from subdirectories
- Offline recipient detection: auto-tags with configurable action text (CLI flag > config > default)
- Config schema extension: `MessagingConfig` with `offline_action` field
- Label fixes: "Online"/"Offline" in members and status commands
- 223 tests passing (188 baseline + 35 new), 0 failures
- Clippy clean, 0 warnings
- Cross-platform compliant (ATM_HOME pattern, no violations)
- Parallel sprint compliance: no modifications to Sprint 3.3/3.4 files

### Sprint 3.3: Documentation & Polish

**Branch**: `feature/p3-s3-docs-polish`
**Depends on**: Sprint 3.1
**Parallel**: Can run alongside Sprint 3.2
**Status**: ✅ Complete

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

**Implementation**:
- README.md created with quickstart, command reference, configuration, architecture sections
- Settings traversal fix: `find_repo_local_settings()` walks from CWD to git root, checks settings.local.json then settings.json at each level
- Config command: Added doc comment noting source reporting limitation (heuristic, doesn't reflect env/CLI overrides)
- Help text: Polished doc comments on ReadArgs and ConfigArgs
- Tests: Added `test_settings_resolution_from_subdirectory` and `test_settings_local_takes_precedence`
- Validation: 94 tests pass, clippy clean, cargo doc clean (no warnings)

**PR**: TBD
**Completed**: 2026-02-11
**Dev-QA iterations**: 0 (implemented directly by scrum master)

### Sprint 3.4: Inbox Retention and Cleanup

**Status**: ✅ Complete
**Branch**: `feature/p3-s4-retention`
**Depends on**: Sprint 3.1
**Parallel**: Can run alongside Sprint 3.2 or 3.3

**Deliverables**:
- Configurable retention policy (max age and/or max message count) — ✅ DONE
- Default cleanup for non-Claude-managed inboxes — ✅ DONE
- Optional cleanup for Claude-managed inboxes (configurable) — ✅ DONE
- Archive or delete strategy with tests — ✅ DONE

**Acceptance criteria**:
- Inboxes are bounded by configured policy — ✅ VERIFIED
- Non-Claude inbox cleanup runs without data loss outside policy — ✅ VERIFIED
- Tests cover retention by age and by count — ✅ VERIFIED (11 integration tests)

**Implementation**:
- `crates/atm-core/src/retention.rs` — Core retention logic (318 lines)
- `crates/atm/src/commands/cleanup.rs` — CLI command (161 lines)
- `crates/atm-core/tests/retention_tests.rs` — 11 integration tests (443 lines)
- `crates/atm-core/src/config/types.rs` — RetentionConfig and CleanupStrategy types
- All 205 tests pass, clippy clean, cross-platform compliant

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

## 6. Phase 4: Daemon Foundation (`atm-daemon`) ✅

**Goal**: Daemon binary with plugin infrastructure, no concrete plugins yet.

**Branch prefix**: `feature/p4-*`
**Depends on**: Phase 3 complete (MVP)
**Status**: ✅ Complete — All sprints merged. PR [#25](https://github.com/randlee/agent-team-mail/pull/25) merged `integrate/phase-4 → develop`.
**Completed**: 2026-02-12

### Sprint 4.1: Plugin Trait + Registry ✅

**Branch**: `feature/p4-s1-plugin-trait`
**Depends on**: Phase 3 complete
**Status**: ✅ Complete
**PR**: [#22](https://github.com/randlee/agent-team-mail/pull/22)
**Completed**: 2026-02-12

**Deliverables**:
- ✅ `Plugin` async trait definition (edition 2024 native async, ErasedPlugin type-erasure for object safety)
- ✅ `PluginMetadata`, `Capability`, `PluginError`, `PluginState` types
- ✅ `PluginContext` with `SystemContext`, `MailService`, `Config` (RosterService deferred to Sprint 4.3)
- ✅ Vec-based `PluginRegistry` with register, init_all, get_by_name, get_by_capability
- ✅ `MailService` wrapping atm-core inbox_append/read
- ✅ 11 integration tests (MockPlugin + EchoPlugin proving trait implementability)

**Deviations from original plan**:
- Used Vec-based registry instead of `inventory` crate (simpler, sufficient for current needs)
- RosterService not included in PluginContext (Sprint 4.3 will add it)

**Acceptance criteria**:
- ✅ Trait compiles and is implementable
- ✅ Mock plugin can be registered and discovered
- ✅ Plugin context provides access to atm-core services
- ✅ 253 total workspace tests, all passing
- ✅ Clippy clean, cross-platform compliant

### Sprint 4.2: Daemon Event Loop ✅

**Branch**: `feature/p4-s2-daemon-loop`
**Depends on**: Sprint 4.1
**Parallel**: Can run alongside Sprint 4.3
**PR**: [#24](https://github.com/randlee/agent-team-mail/pull/24)

**Deliverables**:
- `atm-daemon` binary crate with tokio runtime
- Plugin init → run → shutdown lifecycle
- Cancellation token propagation (SIGINT/SIGTERM → cancel)
- Spool drain loop (periodic retry of pending messages)
- Inbox file watching (kqueue/inotify for event-driven plugins)
- Graceful shutdown with timeout

**Acceptance criteria**:
- ✅ Daemon starts, loads plugins, runs event loop
- ✅ SIGINT triggers graceful shutdown
- ✅ Spool is drained on interval
- ✅ Mock plugin receives init/run/shutdown calls
- ✅ 260 total workspace tests, all passing
- ✅ Clippy clean (including tests), cross-platform compliant

**Status**: ✅ Complete
**Completed**: 2026-02-12
**Dev-QA iterations**: 1 (passed first QA review)
**Implementation**:
- 5 new daemon modules: event_loop.rs, shutdown.rs, spool_task.rs, watcher.rs, mod.rs
- Full daemon binary with clap CLI (--config, --team, --verbose, --daemon)
- Plugin lifecycle: init → run (per-task) → shutdown with timeout
- CancellationToken propagation (SIGINT/SIGTERM on Unix, Ctrl-C on Windows)
- Spool drain loop (10s interval), file system watcher (notify crate)
- Graceful shutdown with per-plugin timeout enforcement
- 7 new daemon integration tests, 18 total daemon crate tests
- ATM_HOME compliance throughout

### Sprint 4.3: Roster Service ✅

**Branch**: `feature/p4-s3-roster`
**Depends on**: Sprint 4.1
**Parallel**: Can run alongside Sprint 4.2
**PR**: [#23](https://github.com/randlee/agent-team-mail/pull/23)

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

**Status**: ✅ Complete
**Completed**: 2026-02-12
**Dev-QA iterations**: 1 (passed first QA review with 1 minor clippy fix)
**Implementation notes**:
- New module `crates/atm-daemon/src/roster/` with `RosterService`, `MembershipTracker`, `RosterError`, `CleanupMode`
- Atomic config.json updates via lock → read → modify → write → rename pattern (reuses `atm_core::io::lock::acquire_lock`)
- `MembershipTracker` maps plugin_name → Vec<(team, agent_name)> for ownership tracking
- `CleanupMode::Soft` sets isActive=false, `CleanupMode::Hard` removes members entirely (both idempotent)
- Synthetic members use `agentType: "plugin:<plugin-name>"` convention
- `PluginContext` updated with `roster: Arc<RosterService>` field
- 22 new tests (8 unit + 14 integration): add/remove, cleanup modes, concurrent access (4 threads), plugin isolation, unknown field preservation
- 33 total atm-daemon tests passing, clippy clean

### Sprint 4.4: Architecture Gap Hotfix (ARCH-CTM Review)

**Branch**: `feature/p4-hotfix-arch-gaps`
**Depends on**: Sprints 4.1, 4.2, 4.3
**Worktree**: `/Users/randlee/Documents/github/agent-team-mail-worktrees/feature/p4-hotfix-arch-gaps/` (already created from `integrate/phase-4`)
**PR target**: `integrate/phase-4`
**Design prompt**: `.tmp/sprint-4.4-design.md`

**Background**: External architecture review (ARCH-CTM) identified gaps between requirements and Phase 4 implementation. This hotfix addresses findings that would block Phase 5 (Issues plugin).

**Deliverables**:

1. **[HIGH] Add behavioral Capability variants** (`crates/atm-daemon/src/plugin/types.rs`):
   - Add `AdvertiseMembers`, `InterceptSend`, `InjectMessages`, `EventListener` to `Capability` enum
   - Keep existing domain variants (`IssueTracking`, `CiMonitor`, `Bridge`, `Chat`, `Retention`, `Custom`)
   - Behavioral variants describe what a plugin *does* (routing); domain variants describe what it *is about* (metadata)
   - Update any tests that enumerate capabilities

2. **[HIGH] Add plugin config sections** (`crates/atm-core/src/config/types.rs`):
   - Add `plugins: HashMap<String, toml::Table>` field to `Config` struct
   - Each plugin gets `[plugins.<name>]` section in `.atm.toml`
   - Add helper `Config::plugin_config(&self, name: &str) -> Option<&toml::Table>`
   - Add round-trip serialization test for plugin config sections
   - Add `PluginContext::plugin_config(&self, name: &str) -> Option<&toml::Table>` convenience method

3. **[MEDIUM] Fix SystemContext default_team** (`crates/atm-daemon/src/main.rs`):
   - Replace hardcoded `"default-team"` (line 98) with `config.core.default_team.clone()`
   - One-line fix

4. **[MEDIUM] Wire watcher event dispatch to plugins** (`crates/atm-daemon/src/daemon/watcher.rs`, `event_loop.rs`):
   - Change watcher to accept a channel sender for dispatching events
   - In event loop, receive watcher events and route to plugins with `EventListener` capability
   - Call `plugin.handle_message()` for inbox file changes (new/modified files in team inbox dirs)
   - Use `tokio::sync::mpsc` instead of `std::sync::mpsc` for async-native channel

**Deferred (documented, not blocking Phase 5)**:

- **Managed settings policy** (Finding 5): Platform-specific managed policy dirs. Uncommon in practice; deferred to Phase 6+.
- **Destination-repo file policy** (Finding 6): Sprint 3.2 added safe fallback (deny + copy). Full schema extension deferred.
- **SchemaVersion wiring** (Finding 7): `SchemaVersion::detect()` exists but `SystemContext.schema_version` is `Option<()>`. Low priority — no consumer needs it yet. Wire when a consumer exists.
- **Inventory-based registration** (Finding 9): Manual registration is fine for <5 plugins. Defer to Phase 6.
- **Plugin temp_dir** (Finding 8): Add when Issues plugin needs cache storage (Phase 5).
- **Roster atomic swap** (Finding 10): Lock-protected rename is sufficient for config.json contention levels.

**Acceptance criteria**:
- Capability enum includes all 4 behavioral variants from requirements
- `.atm.toml` with `[plugins.issues]` section parses correctly
- `Config::plugin_config("issues")` returns the section
- SystemContext uses config-derived default_team
- Watcher dispatches file events to EventListener plugins
- All existing tests pass + new tests for added functionality
- `cargo clippy -- -D warnings` clean
- `cargo test` 100% pass
- Cross-platform compliant (ATM_HOME pattern)

**Status**: ✅ Complete
**PR**: [#26](https://github.com/randlee/agent-team-mail/pull/26)
**Completed**: 2026-02-12
**Dev-QA iterations**: 1 + 2 review fix rounds
**Implementation notes**:
- Added 4 behavioral Capability variants (`AdvertiseMembers`, `InterceptSend`, `InjectMessages`, `EventListener`) to `Capability` enum
- Added `plugins: HashMap<String, toml::Table>` field to `Config` with `plugin_config()` helper method
- Added `PluginContext::plugin_config()` convenience accessor
- Fixed SystemContext `default_team` to use `config.core.default_team` instead of hardcoded string
- Refactored watcher to use `tokio::sync::mpsc` with `InboxEvent`/`InboxEventKind` types for async dispatch
- Added event dispatch loop in `event_loop.rs` that routes to `EventListener` plugins via `handle_message()`
- 10 new tests in watcher (path parsing, event types, filtering), 5 new tests for plugin config (round-trip, accessor, missing, empty)
- All atm-core and atm-daemon tests pass (104 passed), clippy clean
- Re-exported `toml` from atm-core for plugin config type access in daemon
- **Review fix 1**: Added `plugins` HashMap merge to `merge_config()` (was silently dropped)
- **Review fix 2**: Replaced `try_recv + sleep` with `recv_timeout` in watcher (eliminated busy-wait)
- **Review fix 3 (ARCH-CTM)**: Fixed watcher path from `inbox` to `inboxes` matching actual Claude teams layout
- **Review fix 4 (ARCH-CTM)**: Fixed event dispatch to parse inbox as `Vec<InboxMessage>` (JSON array), dispatch newest

### Phase 4 Dependency Graph

```
MVP Complete (Phase 3)
    │
    └── Sprint 4.1 (Plugin Trait + Registry)
            │
            ├── Sprint 4.2 (Daemon Loop) ──────────┐
            │                                       │
            └── Sprint 4.3 (Roster Service) ────────┤
                                                    │
            Sprint 4.4 (Arch Gap Hotfix) ───────────┘
                                                    │
                                     Phase 4 Complete
```

### Deferred to Phase 5+

- **Managed settings policy** (ARCH-CTM Finding 5): Platform-specific managed policy directories. Uncommon in practice.
- **Destination-repo file policy** (ARCH-CTM Finding 6): Full resolution requires `TeamConfig` schema extension. Sprint 3.2 fallback is safe.
- **SchemaVersion wiring** (ARCH-CTM Finding 7): Detection exists, no consumer yet.
- **Inventory-based registration** (ARCH-CTM Finding 9): Manual registration is fine for current plugin count.
- **Plugin temp_dir** (ARCH-CTM Finding 8): Add when first plugin needs cache.

---

## 7. Phase 5: First Plugin (Issues) ✅

**Goal**: Working Issues plugin with pluggable provider architecture.

**Branch prefix**: `feature/p5-*`
**Depends on**: Phase 4 complete
**Status**: ✅ Complete — All sprints merged. PRs #27-#29, #31, #32 merged to `integrate/phase-5`. PR #30 merged `integrate/phase-5 → develop`. PR #33 merges remaining ARCH-CTM fixes to develop.
**Completed**: 2026-02-13

### Sprint 5.1: Provider Abstraction ✅

**Branch**: `feature/p5-s1-provider-abstraction`
**Depends on**: Phase 4 complete
**Status**: ✅ Complete
**PR**: [#27](https://github.com/randlee/agent-team-mail/pull/27)
**Completed**: 2026-02-12

**Deliverables**:
- ✅ Provider trait for issue operations (list, get, comment) — `provider.rs` with RPITIT + ErasedIssueProvider
- ✅ GitHub provider implementation using `gh` CLI subprocess — `github.rs`
- ✅ Azure DevOps provider stub — `azure_devops.rs` (later removed in Sprint 5.4)
- ✅ Provider factory function from GitProvider — `create_provider()` in `mod.rs`
- ✅ Issue types (Issue, IssueComment, IssueLabel, IssueFilter, IssueState) — `types.rs`
- ✅ Module structure: `crates/atm-daemon/src/plugins/issues/`
- ✅ All tests pass (293 total), clippy clean with `-D warnings`

### Sprint 5.2: Issues Plugin Core ✅

**Branch**: `feature/p5-s2-issues-plugin`
**Depends on**: Sprint 5.1
**Status**: ✅ Complete
**PR**: [#28](https://github.com/randlee/agent-team-mail/pull/28)
**Completed**: 2026-02-12
**Dev-QA iterations**: 1 + CI fix (collapsible_if clippy lint on rust-1.93.0)

**Deliverables**:
- ✅ IssuesPlugin struct implementing `Plugin` trait — `plugin.rs`
- ✅ IssuesConfig parsing from `[plugins.issues]` — `config.rs`
- ✅ Poll loop with configurable interval, respects cancellation
- ✅ Issue → InboxMessage transformation with `[issue:NUMBER]` prefix
- ✅ Inbox reply → issue comment flow (parses `[issue:NUMBER]` prefix)
- ✅ Configurable filters (labels, assignees, poll_interval, team, agent)
- ✅ Synthetic member registration via RosterService
- ✅ Graceful init error handling (missing provider/repo)
- ✅ All tests pass (317 total, +24 new), clippy clean with `-D warnings`

### Sprint 5.3: Issues Plugin Testing ✅

**Branch**: `feature/p5-s3-issues-tests`
**Depends on**: Sprint 5.2
**Status**: ✅ Complete
**PR**: [#29](https://github.com/randlee/agent-team-mail/pull/29)
**Completed**: 2026-02-12
**Dev-QA iterations**: 1 (Scrum Master handled both dev and QA directly)

**Deliverables**:
- ✅ New module `mock_provider.rs` with configurable MockProvider (issues, comments, error injection, call tracking)
- ✅ `IssueFilter` derive for PartialEq (required for MockCall equality checks)
- ✅ Plugin test helpers: `with_provider()` and `with_config()` for dependency injection
- ✅ Modified plugin init() to skip provider creation if already injected (enables mock testing)
- ✅ 16 new integration tests in `tests/issues_integration.rs` and `tests/issues_error_tests.rs`
- ✅ Test coverage: inbox delivery, reply handling, label filtering, synthetic member lifecycle, disabled plugin, error scenarios, config validation
- ✅ All tests pass (342 total workspace tests), clippy clean, cross-platform compliant

### Sprint 5.4: Pluggable Provider Architecture ✅

**Branch**: `feature/p5-s4-pluggable-providers`
**Depends on**: Sprint 5.3
**Status**: ✅ Complete
**PR**: [#31](https://github.com/randlee/agent-team-mail/pull/31)
**Completed**: 2026-02-13
**Dev-QA iterations**: 1

**Background**: User review of Sprint 5.1 identified that providers were hard-coded in the daemon crate. External providers must be registerable without modifying daemon source code.

**Deliverables**:
- ✅ `ProviderRegistry` with runtime registration — `registry.rs` (HashMap<String, ProviderFactory>)
- ✅ `ProviderLoader` using `libloading` for dynamic `.dylib`/`.so`/`.dll` loading — `loader.rs`
- ✅ C-ABI convention: libraries export `atm_create_provider_factory() -> *mut ProviderFactory`
- ✅ Config-based provider override via `[plugins.issues] provider = "name"` and `provider_libraries = ["/path/to/lib"]`
- ✅ Provider directory scanning for auto-discovery
- ✅ Removed hard-coded Azure DevOps stub from daemon crate
- ✅ Example external provider crate: `examples/provider-stub/` (cdylib with README)
- ✅ Integration test: build stub → load dynamically → verify factory and provider methods
- ✅ All tests pass (351 total), clippy clean, cross-platform compliant

### Sprint 5.5: ARCH-CTM Review Fixes ✅

**Branch**: `review/arch-ctm-phase-5`
**Depends on**: Sprint 5.4
**Status**: ✅ Complete
**PR**: [#32](https://github.com/randlee/agent-team-mail/pull/32), [#33](https://github.com/randlee/agent-team-mail/pull/33)
**Completed**: 2026-02-13

**Background**: External architecture review (ARCH-CTM) of Phase 5 code identified 3 correctness bugs and test coverage gaps. Fixes and tests were applied, followed by a Windows CI fix.

**Fixes**:
- ✅ **Self-loop guard**: Plugin commenting on its own notifications — added `if msg.from == self.config.agent { return; }` in `handle_message()`
- ✅ **Library unload prevention**: `ProviderLoader` stored in plugin struct to keep dynamic libraries alive (was being dropped after `build_registry()`)
- ✅ **Message dedup improvement**: `message_id` now includes `updated_at` — `format!("issue-{}-{}", issue.number, issue.updated_at)` — so issue updates aren't suppressed
- ✅ **Windows CI fix**: Added `ATM_HOME` env var support to `get_spool_dir_with_base()` — `dirs::config_dir()` ignores HOME/USERPROFILE on Windows

**Test additions**:
- ✅ Provider loader integration test (build stub, load, verify)
- ✅ Issue update delivery test (updated_at in message_id)
- ✅ Event loop `read_latest_inbox_message` tests
- ✅ Self-loop guard test
- ✅ Spool test isolation fix (ATM_HOME instead of platform-specific env vars)
- ✅ All tests pass (363 total), CI green on all 3 platforms (Ubuntu, macOS, Windows)

### Phase 5 Dependency Graph

```
Phase 4 Complete
    │
    └── Sprint 5.1 (Provider Abstraction)
            │
            └── Sprint 5.2 (Issues Plugin Core)
                    │
                    └── Sprint 5.3 (Issues Plugin Testing)
                            │
                            └── Sprint 5.4 (Pluggable Provider Architecture)
                                    │
                                    └── Sprint 5.5 (ARCH-CTM Review Fixes)
                                            │
                                         Phase 5 Complete
```

---

## 8. Phase 6: CI Monitor Plugin

**Goal**: CI Monitor plugin with GitHub Actions built-in provider and Azure DevOps as external provider (demonstrating the pluggable provider architecture from Phase 5).

**Branch prefix**: `feature/p6-*`
**Depends on**: Phase 5 complete
**Integration branch**: `integrate/phase-6` (created from `develop` at phase start)
**Reference**: Existing CI Monitor design doc at `agent-teams-test/docs/ci-monitor-design.md` (Go-based; adapted to ATM's Rust plugin system)
**Status**: Not started

### Sprint 6.1: CI Provider Abstraction

**Branch**: `feature/p6-s1-ci-provider`
**Depends on**: Phase 5 complete
**Parallel**: None (foundation sprint)

**Deliverables**:
- `CiProvider` async trait for CI operations (list runs, get run details, get job logs)
- CI types: `CiRun`, `CiJob`, `CiStep`, `CiRunStatus`, `CiRunConclusion`, `CiFilter`
- `CiProviderRegistry` (same pattern as Issues `ProviderRegistry`)
- GitHub Actions built-in provider using `gh` CLI
- Module structure: `crates/atm-daemon/src/plugins/ci_monitor/`
- Unit tests for types and registry

**Acceptance criteria**:
- CiProvider trait compiles and is implementable
- GitHub provider can list/get workflow runs via `gh` CLI
- Registry supports registration and lookup
- All tests pass, clippy clean

### Sprint 6.2: CI Monitor Plugin Core

**Branch**: `feature/p6-s2-ci-monitor-plugin`
**Depends on**: Sprint 6.1

**Deliverables**:
- `CiMonitorPlugin` struct implementing `Plugin` trait
- `CiMonitorConfig` parsing from `[plugins.ci_monitor]` TOML section
- Poll loop: detect new workflow runs, monitor status, detect failures
- Failure → InboxMessage transformation with `[ci:RUN_ID]` prefix
- Deduplication: per-commit or per-run (configurable)
- Configurable: poll interval, repo, team, agent, watched branches
- Synthetic member registration via RosterService

**Acceptance criteria**:
- Plugin polls GitHub Actions and detects failures
- Failure reports delivered as inbox messages
- No duplicate notifications for same failure
- Configurable via `.atm.toml` `[plugins.ci_monitor]` section
- All tests pass, clippy clean

### Sprint 6.3: CI Monitor Testing + Azure External Provider

**Branch**: `feature/p6-s3-ci-monitor-tests`
**Depends on**: Sprint 6.2

**Deliverables**:
- Mock CI provider for testing (no real API calls)
- End-to-end: CI failure → message delivered → acknowledgment
- Error scenarios: API failure, auth failure, timeout
- Azure DevOps external provider example crate (`examples/ci-provider-azdo/`)
  - cdylib using same C-ABI convention as provider-stub
  - Demonstrates external CI provider registration
  - Uses Azure DevOps REST API via `az` CLI or direct HTTP
- Integration test: build Azure provider stub → load dynamically → verify

**Acceptance criteria**:
- Mock tests cover full failure detection lifecycle
- Azure DevOps provider builds as external cdylib
- Dynamic loading works (same pattern as issues provider-stub)
- All tests pass on Ubuntu, macOS, Windows

### Phase 6 Dependency Graph

```
Phase 5 Complete
    │
    └── Sprint 6.1 (CI Provider Abstraction)
            │
            └── Sprint 6.2 (CI Monitor Plugin Core)
                    │
                    └── Sprint 6.3 (CI Monitor Testing + Azure External)
                                    │
                              Phase 6 Complete
```

---

## 8.5 Phase 6.4: Design Reconciliation (Post-Phase 6) ✅

**Goal**: Update requirements and plan to support multi-repo daemon model and clarify root vs repo semantics.

**Branch prefix**: `planning/p6-4-*`
**Depends on**: Phase 6 complete
**Status**: ✅ Complete (incorporated into planning/phase-7 branch, PR #40)
**Completed**: 2026-02-14

**Deliverables**:
- ✅ Requirements update: explicit `root` vs `repo` distinction and behavior in non-repo contexts
- ✅ Multi-repo daemon model: per-repo scoping for caches, reports, and plugin state
- ✅ CI monitor behavior when `repo` is absent (disable with warning or degrade)
- ✅ Path resolution rules for plugin outputs (repo-root vs workspace root)
- ✅ Subscription schema: support per-filter `reason/justification` (and optional expiry) without enforcing behavior
- ✅ Config tiers: machine-level daemon config listing repo paths; repo-level CI settings in `<repo>.config.atm.toml`; team config for collaboration/transport only
  - Proposed paths: `~/.config/atm/daemon.toml` (machine) and `<repo>/.atm/config.toml` (repo)
- ✅ Plan update for Phase 7/8 to reflect multi-repo support decisions
- ✅ Co-recipient notification confirmed as hard requirement
- ✅ Branch filter syntax: `develop:*` (derived), `develop:feature/*` (derived + pattern)
- ✅ Daemon lifecycle: CLI starts daemon on first use, hot-reload support

**Acceptance criteria**:
- ✅ docs/requirements.md updated with root/repo + multi-repo daemon rules
- ✅ docs/project-plan.md updated with follow-on work items
- ✅ ARCH-ATM + ARCH-CTM agree on the model and sign off

---

## 9. Phase 7: Async Agent Worker Adapter

**Goal**: Generic async worker adapter enabling daemon-managed agent teammates (Codex first backend), with TMUX-based process isolation and log-file IPC.

**Branch prefix**: `feature/p7-*`
**Depends on**: Phase 6.4 design reconciliation complete
**Status**: Planned (5 sprints: 7.1–7.5)

**Design reference**: [`docs/codex-tmux-adapter.md`](./codex-tmux-adapter.md)

### Sprint 7.1: Worker Adapter Trait + Codex Backend

**Branch**: `feature/p7-s1-worker-adapter`
**Depends on**: Phase 6.4
**Parallel**: None (foundation for all subsequent sprints)

**Goal**: Define the generic `WorkerAdapter` trait and implement the Codex TMUX backend. Wire into daemon plugin system.

**Deliverables**:
- `WorkerAdapter` trait in `atm-daemon` with methods: `spawn(agent, config) -> WorkerHandle`, `send_message(handle, message) -> Result<()>`, `shutdown(handle) -> Result<()>`
- `WorkerHandle` struct holding tmux pane ID, log file path, agent identity
- `CodexTmuxBackend` implementing `WorkerAdapter` — spawns Codex in a tmux pane via `tmux new-window` / `tmux send-keys`
- **CRITICAL**: All `tmux send-keys` calls MUST use literal mode (`-l`) to prevent command injection and garbled prompts. Escape sequences must be handled explicitly.
- `WorkerAdapterPlugin` implementing `Plugin` trait — registers with daemon, watches inbox events
- Daemon config schema: `[workers]` section in `daemon.toml` with `enabled`, `backend`, `tmux_session` fields
- Safety: each agent gets its own tmux pane; no stdin injection into user's active terminal
- Unit tests for trait, config parsing, and tmux command generation (mocked)

**File ownership**:
- `crates/atm-daemon/src/plugins/worker_adapter/` — new module (mod.rs, trait.rs, codex_tmux.rs, config.rs, plugin.rs)
- `crates/atm-daemon/src/plugins/mod.rs` — register worker_adapter module

**Acceptance criteria**:
- `WorkerAdapter` trait compiles with at least one backend (CodexTmuxBackend)
- Plugin registers with daemon and can be enabled/disabled via config
- Codex tmux pane spawns successfully when adapter is triggered
- All tests pass, clippy clean with `-D warnings`

### Sprint 7.2: Message Routing + Response Capture + Activity Tracking

**Branch**: `feature/p7-s2-message-routing`
**Depends on**: Sprint 7.1
**Parallel**: None

**Goal**: Complete the message flow: inbox event → worker input → response capture → inbox write-back. Also implement agent activity tracking for accurate offline detection.

**Deliverables**:
- Inbox watcher in `WorkerAdapterPlugin`: subscribe to inbox events for configured agents, filter by agent subscription config
- Message formatting: convert `InboxMessage` to worker-compatible prompt (configurable template)
- Input delivery: `tmux send-keys -t <pane> <formatted-prompt> Enter`
- Response capture via log file tailing: worker backend writes to a known log path, adapter tails file for new output after message delivery
  - **CRITICAL**: Log capture requires an explicit writer contract — backend must use a wrapper/tee to write output to the log file. Cannot rely on implicit stdout capture. Codex backend should launch with output redirected (e.g., `codex ... 2>&1 | tee <log_path>`).
- Response parsing: extract worker output, strip prompt echo, build `InboxMessage` with `from = <agent>`
- Write response to sender's inbox via `inbox_append`
- Request-ID correlation: if incoming message has Request-ID, include it in response for `atm request` compatibility
- Per-agent config in repo-level `.atm/config.toml`: agent name, enabled flag, prompt template
- **Concurrency policy**: enforce before routing — queue incoming messages per-agent by default. Prevents interleaved responses when multiple messages arrive for the same worker. Policy configurable per-agent: `queue` (default), `reject`, or `concurrent`.
- **Agent activity tracking**:
  - `atm send` sets sender's `isActive: true` and `lastActive` timestamp in team `config.json` as heartbeat
  - **CRITICAL**: Activity tracking updates config.json frequently — MUST use atomic swap/lock (same infrastructure as inbox writes) to prevent corruption. This is the same class of bug caught in Phase 3.
  - Daemon monitors inbox file events (already in event loop) and tracks last-activity-per-agent
  - Primary activity signal: messages sent by agent (`from` field in inbox writes). This is the source of truth — the agent actively produced output.
  - Secondary signal: messages read by agent (`read: true` transitions). Indicates consumption but is less reliable (bulk read operations may batch transitions).
  - Configurable inactivity timeout (default: 5 minutes) — daemon sets `isActive: false` after timeout
  - Fix `atm send` offline detection: only warn on explicit `isActive: false`, not missing/null

**File ownership**:
- `crates/atm-daemon/src/plugins/worker_adapter/router.rs` — message routing logic
- `crates/atm-daemon/src/plugins/worker_adapter/capture.rs` — log file tailing + response extraction
- `crates/atm-daemon/src/plugins/worker_adapter/codex_tmux.rs` — extend with send_message implementation
- `crates/atm-daemon/src/plugins/worker_adapter/activity.rs` — agent activity tracker (inbox event → isActive/lastActive updates)
- `crates/atm/src/commands/send.rs` — fix offline detection logic + set sender heartbeat on send

**Acceptance criteria**:
- End-to-end: send message to agent inbox → Codex receives prompt → response appears in sender inbox
- Request-ID correlation works for `atm request` use case
- Log file tailing correctly captures output without race conditions
- `atm send` sets sender `isActive: true` + `lastActive` in config.json
- Daemon marks inactive agents after timeout
- `atm send` only warns on explicit `isActive: false` (no warning for missing/null)
- All tests pass, clippy clean with `-D warnings`

### Sprint 7.3: Worker Lifecycle + Health Monitoring

**Branch**: `feature/p7-s3-worker-lifecycle`
**Depends on**: Sprint 7.2
**Parallel**: None

**Goal**: Production-ready worker management — startup, crash recovery, health checks, graceful shutdown.

**Deliverables**:
- Worker startup on daemon init: auto-spawn configured agents on daemon start
- Health check: periodic tmux pane liveness check (`tmux has-session`), detect crashed/exited workers
- Crash recovery: auto-restart worker pane with configurable retry limit and backoff
- Graceful shutdown: `WorkerAdapter::shutdown()` sends exit command, waits for clean exit, falls back to `tmux kill-pane`
- Concurrent request policy: configurable per-agent — queue (default), reject, or allow concurrent
- Worker status reporting: expose worker state (running, crashed, restarting, idle) via daemon status endpoint
- Log rotation: cap log file size, rotate on worker restart

**File ownership**:
- `crates/atm-daemon/src/plugins/worker_adapter/lifecycle.rs` — startup, health, restart logic
- `crates/atm-daemon/src/plugins/worker_adapter/plugin.rs` — extend with lifecycle hooks

**Acceptance criteria**:
- Workers auto-start on daemon init
- Crashed worker is detected and restarted within configurable interval
- Graceful shutdown works without orphaned tmux panes
- Concurrent request policy is enforced
- All tests pass, clippy clean with `-D warnings`

### Sprint 7.4: Integration Testing + Config Validation

**Branch**: `feature/p7-s4-integration-tests`
**Depends on**: Sprint 7.3
**Parallel**: None

**Goal**: Comprehensive integration tests and config validation for the worker adapter system.

**Deliverables**:
- Integration test: full daemon → worker adapter → Codex tmux → response cycle (using mock backend for CI)
- Mock worker backend: `MockTmuxBackend` implementing `WorkerAdapter` for testing without real tmux/Codex
- Config validation: reject invalid config (missing backend, unknown agent, bad tmux session name)
- Error scenario tests: worker crash during message processing, log file missing, tmux not available
- Cross-platform considerations: tmux availability check (skip gracefully on Windows CI), ATM_HOME compliance
- Documentation: update `docs/codex-tmux-adapter.md` with final architecture, config reference, troubleshooting

**File ownership**:
- `crates/atm-daemon/tests/worker_adapter_tests.rs` — integration tests
- `crates/atm-daemon/src/plugins/worker_adapter/mock_backend.rs` — mock for testing
- `docs/codex-tmux-adapter.md` — update with final design

**Acceptance criteria**:
- All integration tests pass with mock backend on all CI platforms (Ubuntu, macOS, Windows)
- Real tmux tests pass locally on macOS/Linux (skipped on Windows CI)
- Config validation rejects all known invalid configurations
- Documentation is complete and accurate
- All tests pass, clippy clean with `-D warnings`

### Sprint 7.5: Phase 7 Review + Phase 8 Bridge Design

**Branch**: `planning/phase-7-review`
**Depends on**: Sprint 7.4
**Parallel**: None

**Goal**: ARCH-CTM review of Phase 7 implementation, gap analysis, and design planning for Phase 8 (Cross-Computer Bridge Plugin).

**Deliverables**:
- `docs/phase7-review.md` — ARCH-CTM review of worker adapter implementation (correctness, design, gaps)
- Fix sprint for any issues found during review (if needed)
- Phase 8 design outline: Cross-Computer Bridge Plugin
  - Transport protocol selection (TCP/SSH/HTTP/WebSocket)
  - Authentication model between machines
  - Bidirectional inbox sync strategy
  - Offline queue and retry semantics
  - How bridge interacts with multi-repo daemon model and worker adapter
- Requirements updates for Phase 8
- Project plan updates with Phase 8 sprint decomposition

**Acceptance criteria**:
- All review findings addressed (fixes committed or tracked as follow-up)
- Phase 8 design document exists with agreed transport and sync model
- docs/requirements.md updated with bridge plugin details
- docs/project-plan.md updated with Phase 8 sprint list
- ARCH-ATM + ARCH-CTM sign off on Phase 8 plan

### Phase 7 Dependency Graph

```
Phase 6.4 Complete
    │
    └── Sprint 7.1 (Worker Adapter Trait + Codex Backend)
            │
            └── Sprint 7.2 (Message Routing + Response Capture)
                    │
                    └── Sprint 7.3 (Worker Lifecycle + Health Monitoring)
                            │
                            └── Sprint 7.4 (Integration Testing + Config Validation)
                                    │
                                    └── Sprint 7.5 (Phase 7 Review + Phase 8 Bridge Design)
                                            │
                                         Phase 7 Complete
```

### Deferred to Phase 8+

- **WorkerHandle tmux-specific** (Issue #48, Finding 1): `WorkerHandle` has hardcoded `tmux_pane_id` field. Refactor to generic `adapter_handle` or associated type when second backend (SSH, Docker) is added in Phase 8.
- **Parent directory fsync after atomic swap** (Issue #48, Finding 5): After `atomic_swap` rename, parent directory entry is not fsynced. Unlikely to matter in practice but noted for "guaranteed delivery" semantics. Could gate behind config flag. See also: "Windows atomic swap fsync" deferred from Phase 3.
- **Retention wired into daemon event loop** (Issue #48, Finding 6): `retention.rs` exists (Phase 3) but is CLI-only (`atm cleanup`). Wire into daemon as periodic task or threshold-triggered on hot inboxes to prevent unbounded inbox growth.

---

## 10. Phase 8: Cross-Computer Bridge Plugin

**Goal**: Bridge plugin enabling multi-machine agent teams with bidirectional inbox sync via SSH/SFTP.

**Branch prefix**: `feature/p8-*`
**Integration branch**: `integrate/phase-8` (off `develop`)
**Depends on**: Phase 7 complete
**Status**: Complete (all Phase 8 sprints merged)

**Design reference**: [`docs/phase8-bridge-design.md`](./phase8-bridge-design.md) (ARCH-CTM reviewed, approved)

**Key decisions**:
- Local `<agent>.json` files NEVER modified (Claude Code contract)
- Remote origin files additive: `<agent>.<hostname>.json` alongside local files
- Hub-spoke topology, SSH/SFTP transport, atomic temp+rename writes
- Filename parsing: match suffix against hostname registry (not dot-split)
- Bridge assigns `message_id` to messages lacking one
- Self-write filtering to prevent event storm feedback loop

### Sprint 8.1 — Bridge Config + Plugin Scaffold
**Goal**: Bridge plugin scaffold and configuration model.
**Branch**: `feature/p8-s1-bridge-config`

**Deliverables**:
- Bridge plugin scaffold implementing Plugin trait (`init`/`run`/`shutdown`)
- Bridge config structs: hostname, role (hub/spoke), remotes list, sync interval
- Hostname registry with collision detection
- Alias resolution
- Config parsing from `[plugins.bridge]` in `.atm.toml`
- Unit tests for config parsing and hostname validation

**Files**:
- `crates/atm-daemon/src/plugins/bridge/mod.rs`
- `crates/atm-daemon/src/plugins/bridge/config.rs`
- `crates/atm-core/src/config/` (bridge config types)

### Sprint 8.2 — Per-Origin Read Path + Watcher Fix
**Goal**: Enable reading from multiple per-origin inbox files. Can run in parallel with Sprint 8.3.
**Branch**: `feature/p8-s2-read-path`

**Deliverables**:
- New `inbox_read_merged(team_dir, agent_name) -> Vec<InboxMessage>` in `atm-core::io::inbox`
  - Lists inbox dir, filters by known hostnames from registry
  - Merges, deduplicates by `message_id`, sorts by timestamp
  - Backward-compatible: falls back to `<agent>.json` only when bridge not configured
- Update CLI `read.rs` to call merged reader
- Update CLI `inbox.rs` to call merged reader
- Update daemon watcher `parse_event`: add `origin: Option<String>` to `InboxEvent`, normalize agent name by matching known agent names + hostname registry (NOT dot-split — agent names may contain dots)
- Verify daemon `event_loop.rs` cursor tracking with per-origin files
- Unit + integration tests for merge, dedup, and watcher parsing (include test for agent name containing dots with hostname suffix)

**Files**:
- `crates/atm-core/src/io/inbox.rs` (new `inbox_read_merged`)
- `crates/atm/src/commands/read.rs`
- `crates/atm/src/commands/inbox.rs`
- `crates/atm-daemon/src/daemon/watcher.rs`
- `crates/atm-daemon/src/daemon/event_loop.rs`

### Sprint 8.3 — SSH/SFTP Transport
**Goal**: Transport abstraction with SSH/SFTP implementation. Can run in parallel with Sprint 8.2.
**Branch**: `feature/p8-s3-ssh-transport`

**Deliverables**:
- Transport trait: `connect`, `upload`, `download`, `list`, `rename`
- SSH/SFTP implementation using `russh`/`ssh2` crate
- `ControlMaster` connection pooling and lifecycle
- Mock transport implementation for tests
- Connection health check, retry with exponential backoff
- Unit tests with mock transport
- SSH tests gated behind `ATM_TEST_SSH=1` feature flag

**Files**:
- `crates/atm-daemon/src/plugins/bridge/transport.rs`
- `crates/atm-daemon/src/plugins/bridge/ssh.rs`
- `crates/atm-daemon/src/plugins/bridge/mock_transport.rs`

### Sprint 8.4 — Sync Engine + Dedup
**Goal**: Core sync logic connecting transport to inbox files.
**Branch**: `feature/p8-s4-sync-engine`
**Depends on**: Sprint 8.2 + Sprint 8.3

**Deliverables**:
- Push cycle: watch local inbox → SFTP new messages to remote `<agent>.<local-hostname>.json`
- Pull cycle: download remote origin files → write locally
- Atomic remote writes (temp+rename via transport trait)
- Cursor/watermark tracking to avoid re-transferring old messages
- `message_id` assignment for messages that lack one
- Self-write filtering (HashSet with TTL to prevent feedback loop)
- **Invariant**: local `<agent>.json` is NEVER modified by bridge; only per-origin `<agent>.<hostname>.json` files are written
- Integration tests with mock transport simulating 2-node sync (verify local inbox untouched)

**Files**:
- `crates/atm-daemon/src/plugins/bridge/sync.rs`
- `crates/atm-daemon/src/plugins/bridge/dedup.rs`
- `crates/atm-daemon/tests/bridge_sync.rs`

### Sprint 8.5 — Team Config Sync + Hardening
**Goal**: Production hardening, CLI commands, and documentation.
**Branch**: `feature/p8-s5-hardening`
**Depends on**: Sprint 8.4

**Deliverables**:
- Sync team config from hub to spokes
- Hostname registry warnings on config sync
- Logging and operational metrics
- Failure handling and retry policy for partial syncs
- Retention extension: `RetentionConfig` handles per-origin files
- Stale `.bridge-tmp` file cleanup on startup
- `atm bridge status` / `atm bridge sync` CLI commands
- **Invariant**: local `<agent>.json` is NEVER modified by bridge; only per-origin files are written
- End-to-end integration test: 3-node simulated topology with mock transport (verify local inbox untouched)
- Documentation and ops checklist

**Files**:
- `crates/atm-daemon/src/plugins/bridge/`
- `crates/atm/src/commands/bridge.rs`
- `crates/atm-daemon/tests/bridge_e2e.rs`
- `docs/`

### Sprint 8.6 — Bridge Hardening + Blocking Read
**Goal**: Address architecture review gaps and add blocking read support.
**Branch**: `feature/p8-s6-hardening`

**Deliverables**:
- Bridge pull logic fixes (base inbox pull, per-origin handling)
- Per-remote transport map, lazy connect
- Dedup eviction (FIFO cap) and retention for per-origin files
- Shared mock transport E2E tests
- `atm read --timeout` (blocking read with watcher fallback)

**Files**:
- `crates/atm-daemon/src/plugins/bridge/*`
- `crates/atm/src/commands/read.rs`
- `crates/atm/src/commands/wait.rs`
- `crates/atm-daemon/tests/bridge_e2e.rs`

### Phase 8 Dependency Graph

```
Sprint 8.1 (Config + Scaffold)
    │
    ├──→ Sprint 8.2 (Read Path + Watcher)  ──→ Sprint 8.4 (Sync Engine)
    │                                              │
    └──→ Sprint 8.3 (SSH Transport)  ─────────────┘
                                                   │
                                              Sprint 8.5 (Config Sync + Hardening)
```

- 8.2 and 8.3 can run **in parallel** after 8.1 completes
- 8.4 depends on both 8.2 (read path) and 8.3 (transport)
- 8.5 depends on 8.4

---

## 11. Phase 9: CI Monitor Integration + Platform Stabilization

**Goal**: Stabilize CI/tooling and platform fundamentals, then integrate GitHub CI Monitor into team workflows.

**Branch prefix**: `feature/p9-*`
**Integration branch**: `integrate/phase-9` (off `develop`)
**Depends on**: Phase 8.6 verification gate
**Status**: ✅ COMPLETE (v0.9.0)

### Phase 9 Sprint Summary

| Sprint | Name | Track | Status | PR |
|--------|------|-------|--------|----|
| 9.0 | Phase 8.6 Verification Gate | Foundation | ✅ | (gate check) |
| 9.1 | CI/Tooling Stabilization | Foundation | ✅ | [#63](https://github.com/randlee/agent-team-mail/pull/63) |
| 9.2 | Home Dir Resolution | Foundation | ✅ | [#67](https://github.com/randlee/agent-team-mail/pull/67) |
| 9.3 | CI Config & Routing | CI Monitor | ✅ | [#71](https://github.com/randlee/agent-team-mail/pull/71) |
| 9.4 | Daemon Operationalization | CI Monitor | ✅ | [#73](https://github.com/randlee/agent-team-mail/pull/73) |
| 9.5 | WorkerHandle Backend Payload | Worker Adapter | ✅ | [#69](https://github.com/randlee/agent-team-mail/pull/69) |
| 9.6 | Daemon Retention Tasks | Platform | ✅ | [#70](https://github.com/randlee/agent-team-mail/pull/70) |

### Sprint 9.0: Phase 8.6 Verification Gate
- Dependencies: Phase 8.6 merged to develop
- Deliverables: verify Phase 8.6 PRs merged, develop CI green, no open P8 blocking issues
- Files to create/modify: none (gate check only)
- Test requirements: 0 new tests
- QA checklist: confirm no open P8 blocker issues; confirm develop CI green; confirm Phase 8.6 PRs merged
- Exit criteria: all three checks verified and recorded in sprint update

Concrete definition of “no open P8 blocking issues”:
- No open GitHub issues labeled `phase-8` with severity HIGH
- No open PRs marked “blocking Phase 8”
- No failing CI jobs tagged to Phase 8 PRs

### Sprint 9.1: CI/Tooling Stabilization
- Dependencies: Sprint 9.0 complete
- Deliverables: `rust-toolchain.toml` committed, separate clippy CI job with `needs: [clippy]`, QA clippy gate preserved
- Files to create/modify: `.github/workflows/ci.yml`, `rust-toolchain.toml` (verify present)
- Test requirements: 1 CI validation, 1 QA gate
- QA checklist: clippy runs before tests; test job blocked on clippy; clippy failure prevents tests
- Exit criteria: CI passes with separate clippy job, no warnings

### Sprint 9.2: Home Dir Resolution (Cross-Platform)
- Dependencies: Sprint 9.1 complete
- Deliverables: canonical `get_home_dir()` in `atm-core`, replace ALL call sites, clear precedence (ATM_HOME → platform default)
- Files to create/modify:
  - `crates/atm/src/util/settings.rs:14`
  - `crates/atm/src/util/state.rs:62`
  - `crates/atm-core/src/retention.rs:210-214`
  - `crates/atm-core/src/io/spool.rs:303-306`
  - `crates/atm-daemon/src/main.rs:60-64`
  - `crates/atm-daemon/src/plugins/ci_monitor/plugin.rs:366-369`
  - `crates/atm-daemon/src/plugins/ci_monitor/loader.rs:130-134`
  - `crates/atm-daemon/src/plugins/issues/plugin.rs:299-302`
  - `crates/atm-daemon/src/plugins/worker_adapter/config.rs:342-346`
  - `crates/atm-daemon/src/plugins/worker_adapter/config.rs:449-452`
  - `crates/atm-daemon/src/plugins/bridge/ssh.rs:148` (ATM_HOME fallback missing)
- Test requirements: 8 unit, 3 integration (per OS), 1 audit script
- QA checklist: audit script passes; Windows CI green without ATM_HOME; all 11 call sites replaced; path precedence correct
- Exit criteria: all tests pass, audit script validates no lingering dir resolution

### Sprint 9.3: CI Config & Routing
- Dependencies: Sprint 9.2 complete
- Deliverables: branch glob filtering via `globset` or `wildmatch`, `notify_target` config + routing, config validation for invalid targets
- Files to create/modify:
  - `crates/atm-daemon/src/plugins/ci_monitor/config.rs`
  - `crates/atm-daemon/src/plugins/ci_monitor/plugin.rs`
- Test requirements: 10 branch matching, 5 routing, 5 config validation, 2 E2E (22 total)
- QA checklist: empty glob list = all branches; invalid pattern = config error; notify_target defaults to team lead when empty; invalid target fails fast
- Exit criteria: routing works; branch filter verified; tests pass

**Status**: ✅ Complete
**Completed**: 2026-02-16
**Dev-QA iterations**: 1 (passed on first attempt)
**Implementation**:
- Added `globset` dependency for client-side branch glob matching
- Added `NotifyTarget` struct with `agent@team` format parsing
- `CiMonitorConfig` now includes `branch_matcher: Option<GlobSet>` and `notify_target: Vec<NotifyTarget>`
- Glob patterns compiled at config parse time; invalid patterns produce immediate errors
- Client-side branch filtering replaces per-branch API queries (glob patterns can't be passed to GitHub API)
- Notification routing sends to multiple targets; empty = default ci-monitor agent inbox
- 25 new tests (exceeds 22 target): 10 branch matching, 9 routing/validation, 6 plugin-level
- 704 total workspace tests, 0 failures, clippy clean

### Sprint 9.4: Daemon Operationalization
- Dependencies: Sprint 9.3 complete
- Deliverables: daemon writes status JSON file, CLI reads it (no IPC), stale detection based on timestamp
- Files to create/modify:
  - `crates/atm-daemon/src/daemon/status.rs` (new)
  - `crates/atm/src/commands/daemon.rs` (new subcommand) or `commands/status.rs`
- Status file spec:
  - Location: `${ATM_HOME}/daemon/status.json` (via new `get_home_dir()`)
  - Atomic write (temp + rename)
  - Fields: `{ timestamp, pid, version, uptime_secs, plugins: [{name, enabled, status, last_error, last_run}], teams: [<team>] }`
  - `plugins[].status` enum: `running`, `error`, `disabled`
  - CLI stale check: timestamp older than 2x poll interval
- Test requirements: 5 daemon status, 1 startup hint (6 total)
- QA checklist: status file created at startup; updated each poll cycle; stale detection works; CLI reads JSON correctly
- Exit criteria: status file + CLI confirmed on all OSes

### Sprint 9.5: WorkerHandle Backend Payload
- Dependencies: Sprint 9.2 complete
- Deliverables: add payload to WorkerHandle without refactor; safe downcast helpers; registry/adapter passes payload
- Files to create/modify:
  - `crates/atm-daemon/src/plugins/worker_adapter/trait_def.rs`
  - `crates/atm-daemon/src/plugins/worker_adapter/plugin.rs`
  - `crates/atm-daemon/src/plugins/worker_adapter/lifecycle.rs`
  - `crates/atm-daemon/src/plugins/worker_adapter/mock_backend.rs`
  - `crates/atm-daemon/src/plugins/worker_adapter/codex_tmux.rs`
- Requirements:
  - `backend_id` stays as-is
  - Add `payload: Option<Box<dyn Any + Send + Sync>>`
  - Add downcast helpers `payload_ref<T>() -> Option<&T>` (no panic, no unsafe)
- Test requirements: 8 payload/downcast, 5 registry, all 35 existing worker tests must pass (48 total)
- QA checklist: Codex TMUX adapter works; mock backend works with payload; wrong-type downcast returns None
- Exit criteria: all worker tests pass; no regressions in adapters

### Sprint 9.6: Daemon Retention Tasks
- Dependencies: Sprint 9.4 complete
- Deliverables: periodic inbox trimming in daemon loop; CI report file retention; non-blocking execution
- Files to create/modify:
  - `crates/atm-daemon/src/daemon/event_loop.rs`
  - `crates/atm-core/src/retention.rs` (extend for report files)
- Retention spec:
  - Runs every 5 minutes via `tokio::spawn` (configurable in `.atm.toml`)
  - Default policy: 30 days max age, 1000 max messages (configurable)
  - CI reports: delete JSON/Markdown older than max_age in report_dir
  - Concurrency: acquire per-inbox file lock before trimming; release immediately after
- Test requirements: 10 daemon integration, 5 concurrency (retention + bridge sync), 3 cross-platform (18 total)
- QA checklist: retention does not block plugin events; per-origin files handled; report retention works
- Exit criteria: retention runs safely and predictably on all OSes

### Phase 9 Dependency Graph

```
Sprint 9.0 (Verification Gate)
    │
    └── Sprint 9.1 (CI/Tooling)
            │
            └── Sprint 9.2 (Home Dir Resolution)
                    │
                    ├── Sprint 9.3 (CI Config & Routing)
                    │       └── Sprint 9.4 (Daemon Operationalization)
                    │
                    ├── Sprint 9.5 (WorkerHandle Backend Payload)
                    │
                    └── Sprint 9.6 (Daemon Retention Tasks)  ← depends on 9.4
```

Parallel execution plan:
- After 9.2 merges, sprints 9.3 and 9.5 can run in parallel.
- Sprint 9.6 starts after 9.4 to avoid daemon event loop overlap.

### File Ownership Matrix (Phase 9)
- Sprint 9.1: `.github/workflows/ci.yml`, `rust-toolchain.toml`
- Sprint 9.2: home-dir call sites listed above
- Sprint 9.3: `crates/atm-daemon/src/plugins/ci_monitor/config.rs`, `plugin.rs`
- Sprint 9.4: `crates/atm-daemon/src/daemon/status.rs`, `crates/atm/src/commands/daemon.rs`
- Sprint 9.5: `crates/atm-daemon/src/plugins/worker_adapter/*`
- Sprint 9.6: `crates/atm-daemon/src/daemon/event_loop.rs`, `crates/atm-core/src/retention.rs`

### Phase 9 Exit Criteria
- All 667 existing tests pass
- Phase 9 target test count: 750–780
- New code coverage ≥ 80%
- Zero clippy warnings

---

## 12. Phase A: atm-agent-mcp (MCP Stdio Proxy for Codex)

**Status**: IN PROGRESS (7/8 sprints merged, A.8 PR pending)
**Goal**: New `atm-agent-mcp` crate — a thin MCP proxy that wraps a single `codex mcp-server` child
process, managing multiple concurrent Codex sessions with per-session identity, team context,
ATM communication tools, and lifecycle management. Enables Claude to orchestrate Codex agents
over the MCP protocol with native ATM messaging integration.

**Integration branch**: `integrate/phase-A`
**Crate**: `crates/atm-agent-mcp` (binary: `atm-agent-mcp`)
**Requirements**: `docs/atm-agent-mcp/requirements.md` (20 FRs, 6 NFRs, 70+ acceptance tests)
**Design**: `docs/atm-agent-mcp/codex-mcp-crate-design.md`

### Phase A Sprint Summary

Sprints are sequential (each depends on the previous). Scope aligned with `requirements.md` Section 8.

| Sprint | Name | FR Coverage | Status | PR |
|--------|------|-------------|--------|-----|
| A.1 | Crate scaffold + config | FR-12, FR-13 | ✅ MERGED | [#100](https://github.com/randlee/agent-team-mail/pull/100) |
| A.2 | MCP stdio proxy core | FR-1, FR-11, FR-14, FR-15, FR-19 | ✅ MERGED | [#101](https://github.com/randlee/agent-team-mail/pull/101) |
| A.3 | Identity binding + context injection | FR-2, FR-3, FR-16, FR-20.1 | ✅ MERGED | [#102](https://github.com/randlee/agent-team-mail/pull/102) |
| A.4 | ATM communication tools | FR-4, FR-20.4–20.5 | PENDING | — |
| A.5 | Session registry + persistence | FR-5, FR-10, FR-20.2–20.3 | PENDING | — |
| A.6 | Lifecycle state machine + agent_close + approval bridging | FR-17, FR-18 | PENDING | — |
| A.7 | Auto mail injection + turn serialization | FR-8 | ✅ PR OPEN | — |
| A.8 | Shutdown + resume + audit | FR-6, FR-7, FR-9 | 🔄 PR OPEN | pending |

### Phase A Sprint Details

**A.1 — Crate scaffold + config** (PR #100): New workspace crate with CLI skeleton (`serve`, `config`, `sessions`, `summary`), `AgentMcpConfig` struct with full config resolution chain (CLI → env → .atm.toml → defaults), role preset support, env var mapping (`ATM_AGENT_MCP_*`), high-level CLI flags (`--fast`, `--subagents`, `--readonly`/`--explore`).

**A.2 — MCP stdio proxy core** (PR #101): Lazy child process spawn, JSON-RPC pass-through with dual framing (Content-Length + newline-delimited), `tools/list` interception to inject synthetic ATM tool definitions, child crash detection with exit code reporting, configurable request timeout (default 300s), `codex/event` notification forwarding with `agent_id` metadata, mock `echo-mcp-server` test fixture.

**A.3 — Identity binding + context injection** (PR #102): Per-session identity assignment (`codex` calls), identity→agent_id namespace management, cross-process identity lock files with PID liveness detection, `developer-instructions` injection with per-turn context refresh (repo_root, repo_name, branch, cwd), session initialization modes (agent_file, inline prompt, resume), error codes -32001/-32004/-32007/-32008. 1049 workspace tests.

**A.4 — ATM communication tools**: Implement `atm_send`, `atm_read`, `atm_broadcast`, `atm_pending_count` as MCP tools via atm-core. Thread-bound identity enforcement (anti-spoofing). Mail envelope wrapping for injection (FR-8.4–8.5). `max_messages` and `max_message_length` truncation.

**A.5 — Session registry + persistence**: In-memory registry with atomic disk persistence at `~/.config/atm/agent-sessions/<team>/registry.json`. agent_id→backend_id mapping, per-session cwd tracking, stale-session detection on startup, `max_concurrent_threads` enforcement, per-instance independent registry. `agent_sessions` and `agent_status` MCP tools.

**A.6 — Lifecycle state machine + agent_close + approval bridging**: Thread states (busy/idle/closed), `agent_close` MCP tool with summary timeout, resume after close, identity replacement after close, idempotent close, close/cancel/queue precedence. `elicitation/create` request bridging with correlation and timeout.

**A.7 — Auto mail injection + turn serialization**: Post-turn mail check, idle mail polling (configurable interval), deterministic identity routing, single-flight rule per thread, FIFO queue with priority dispatch (close > cancel > Claude > auto-mail), delivery ack boundary, configurable `auto_mail` toggle.

**A.8 — Shutdown + resume + audit**: Graceful shutdown with bounded summary requests per thread, emergency snapshot on timeout, `--resume` flag with summary prepend, fallback for missing summaries, audit log as append-only JSONL, parent disconnect (stdio EOF) as SIGTERM equivalent.

### Phase A Design References

- Requirements: [`docs/atm-agent-mcp/requirements.md`](./docs/atm-agent-mcp/requirements.md)
- Design: [`docs/atm-agent-mcp/codex-mcp-crate-design.md`](./docs/atm-agent-mcp/codex-mcp-crate-design.md)
- Spike reference: `spike/codex-mcp-pattern-copy`
- New crate: `crates/atm-agent-mcp`

---

## 13. Phase B: Team-Lead Session Management

**Status**: PLANNED
**Goal**: Make `atm teams resume` the canonical way for team-lead to re-establish team context after a session restart, `/compress`, or crash — with the daemon as the authority on who is legitimately team-lead.

**Integration branch**: `integrate/phase-B`

### Phase B Sprint Summary

| Sprint | Name | Status | PR |
|--------|------|--------|-----|
| B.1 | Daemon session tracking + `atm teams resume` + `atm teams cleanup` | PLANNED | — |
| B.2 | Unicode-safe message truncation + input validation | MERGED | [#120](https://github.com/randlee/agent-team-mail/pull/120) |
| B.3 | Cleanup safety hardening + documentation alignment | IN PROGRESS | [#122](https://github.com/randlee/agent-team-mail/pull/122) |

---

### Sprint B.3 — Cleanup Safety Hardening + Documentation Alignment

**Branch**: `feature/pB-s3-stabilization`
**Crate(s)**: `crates/atm` (`teams cleanup` safety behavior + tests), `docs` (plan alignment)

#### Problem

`atm teams cleanup` needed two follow-up hardening steps:
1. Team-lead must be protected from automated cleanup removal, including forced cleanup runs.
2. Daemon-unreachable behavior in no-force mode required an explicit companion test to lock the incomplete-cleanup error path.

Additionally, Phase B plan tracking drifted: the sprint summary table did not list B.2 merged status and had no B.3 entry.

#### Solution

1. **Team-lead protection in cleanup path**:
   - `cleanup` now always skips member `team-lead` (single-agent and full-team modes), with warning when explicitly targeted.

2. **Daemon-unreachable safety coverage**:
   - Added companion test asserting no-force cleanup skips removal and returns `Cleanup incomplete` when daemon liveness is unavailable.
   - Renamed misleading single-agent force test to reflect actual behavior.

3. **Project plan synchronization**:
   - Added B.3 sprint entry (scope + exit criteria).
   - Updated Phase B sprint summary table to include B.2 merged PR and B.3 in-progress PR.

#### Exit Criteria

- [x] `atm teams cleanup` never removes `team-lead`, including `--force` paths
- [x] Companion no-force daemon-unreachable test added (`Cleanup incomplete` asserted)
- [x] Misleading cleanup test name corrected for behavior clarity
- [x] Phase B sprint summary table includes B.2 and B.3 with PR references
- [x] B.3 scope and exit criteria documented in this plan

---

### Sprint B.2 — Unicode-Safe Message Truncation + Input Validation

**Branch**: `feature/pB-s2-unicode-validation`
**Crate(s)**: `crates/atm-core` (new `text` module), `crates/atm` (send/broadcast/request), `crates/atm-agent-mcp` (atm_tools, mail_inject)

#### Problem

Several truncation helpers across the codebase performed byte-index slicing on UTF-8 strings (e.g. `&text[..100]`), which panics on multi-byte characters (em dash, CJK, emoji) at a boundary. Additionally, no validation existed to reject null bytes or oversized (>1 MiB) messages before they were written to disk.

#### Solution

1. **New shared module `crates/atm-core/src/text`** — canonical Unicode-safe utilities:
   - `truncate_chars(text, max_chars, suffix)` — allocating truncation with suffix
   - `truncate_chars_slice(text, max_chars)` — zero-copy truncation returning `&str`
   - `validate_message_text(text, max_bytes)` — rejects null bytes and oversized messages
   - `DEFAULT_MAX_MESSAGE_BYTES = 1_048_576` (1 MiB)

2. **Fixed `generate_summary()`** in `send.rs`, `broadcast.rs`, `request.rs` — replaced `&trimmed[..MAX_LEN]` byte slicing with `truncate_chars_slice`

3. **Fixed `one_line()`** in `request.rs` — replaced `&single[..120]` byte slicing with `truncate_chars`

4. **Removed private duplicates** in `atm-agent-mcp`: `char_boundary_at_limit` (atm_tools.rs) and `truncate_utf8_chars` (mail_inject.rs) replaced with shared utilities

5. **Input validation** added in `send.rs`, `broadcast.rs`, `request.rs` — `validate_message_text()` called before any I/O

#### MSRV Note

All truncation uses `char_indices().nth(n)` — MSRV 1.85 compatible. `str::floor_char_boundary` (requires Rust 1.91) is explicitly prohibited.

#### Exit Criteria

- [x] `crates/atm-core/src/text` module exported from `lib.rs`
- [x] All `generate_summary()` functions use char-based truncation
- [x] `one_line()` in request.rs uses char-based truncation
- [x] No private duplicate truncation helpers remain
- [x] `validate_message_text()` called before I/O in send/broadcast/request
- [x] Error messages are user-facing and actionable (`--file` suggestion for oversized)
- [x] Tests for em dash, CJK, emoji, grapheme cluster, null byte, oversize, exact-MAX boundary
- [x] 7 dedicated `one_line()` tests including multi-byte at 120-char boundary
- [x] 6 dedicated `generate_summary()` tests in request.rs
- [x] `cargo test` 1218 tests, 0 failures
- [x] `cargo clippy -- -D warnings` clean
- [x] MSRV 1.85 compliant (no `floor_char_boundary`)

---

### Sprint B.1 — Daemon Session Tracking + `atm teams resume` + `atm teams cleanup`

**Branch**: `feature/pB-s1-teams-resume`
**Crate(s)**: `crates/atm` (new subcommands), `crates/atm-daemon` (new tracking), `crates/atm-core` (schema)

#### Problem

When a Claude Code session restarts (new session, `/compress`, or crash), `leadSessionId` in `config.json` no longer matches the new `CLAUDE_SESSION_ID`. Claude Code then creates a **new team with a random name** instead of rejoining `atm-dev`. Since `.atm.toml` hardcodes `default_team=atm-dev`, non-Claude teammates (arch-ctm) become unreachable.

**Current workaround**: Trigger a gated Task call → extract session ID from gate debug log → manually update `config.json` via Python. This is fragile and not documented as a user-facing workflow.

#### Solution

Two components working together:

1. **Daemon session tracking** — daemon captures `CLAUDE_SESSION_ID` from `SessionStart` hook events and maintains a registry of `agent_name → {session_id, process_id, state}`. This makes the daemon the authoritative source on which session legitimately holds team-lead role.

2. **`atm teams resume <team> [message]`** — new CLI subcommand that:
   - Resolves caller identity (must be `team-lead` via `.atm.toml` / `ATM_IDENTITY`, else rejected)
   - Checks daemon for registered team-lead session:
     - **Same session ID** → no-op, just print TeamCreate reminder (handles `/compress` case)
     - **Different session ID, old session dead** → update `leadSessionId`, notify members, print TeamCreate reminder
     - **Different session ID, old session alive** → reject: `"team-lead is already active in session <id>. (see --help to override)"`
     - **No team on disk** → `"No team 'atm-dev' found. Call TeamCreate(...) to create it."`
   - `--force` → override even if old session appears alive (lost pane, unresponsive)
   - `--force --kill` → SIGTERM old process first, then resume

#### Output Format

**No team on disk:**
```
No team 'atm-dev' found. Call TeamCreate(team_name="atm-dev") to create it.
```

**Team exists, resumed successfully:**
```
atm-dev resumed. leadSessionId updated. 3 members notified.

To re-establish as team-lead, call:
  TeamCreate(team_name="atm-dev", description="ATM Phase A development team - atm-agent-mcp MCP Proxy")
```

**Rejected (already active):**
```
Error: team-lead is already active in session abc12345... (see --help to override)
```

**Rejected (wrong identity):**
```
Error: caller identity is 'publisher'. Only team-lead may call atm teams resume.
```

#### Notification Message

When `resume` notifies members:
- If `[message]` arg provided: use that
- Default: `"Team-lead has rejoined the session. Context may have been reset. Please provide a brief status update."`

#### Daemon Changes (`crates/atm-daemon`)

- `SessionStart` hook watcher: capture `session_id` + `process_id` per agent name
- New in-memory registry: `HashMap<AgentName, SessionRecord { session_id, process_id, state }>`
- New Unix socket query: `{"type": "session_query", "name": "team-lead"}` → `{session_id, process_id, alive}`
- Liveness check: poll `process_id` to confirm alive (already done for `Killed` state detection)
- Shutdown hook (new): mark session dead cleanly on `SessionEnd` / process exit

#### `atm teams resume` Changes (`crates/atm`)

- New subcommand under `atm teams` (consistent with `atm teams add-member`)
- Queries daemon via Unix socket for session registry
- Falls back gracefully if daemon not running (treat old session as dead, update + warn)
- Atomic write to `config.json`: update `leadSessionId` + set `isActive=false` for all Claude members except team-lead
- Sends notifications via existing `atm send` path

#### `atm teams cleanup [team] [agent]`

New subcommand for inbox and member housekeeping.

**`atm teams cleanup atm-dev`** (full team cleanup):
- For each member in `config.json`: check liveness via daemon
- **Alive** → skip, no warning (healthy member)
- **Dead** → remove from `config.json` members array + delete inbox file
- Print summary: `"Removed 3 stale members: publisher, publisher-2, sm-a-8"`

**`atm teams cleanup atm-dev <agent>`** (single agent):
- **Alive** → skip with warning: `"<agent> is still active, skipping cleanup"`
- **Dead** → remove from config + delete inbox
- Use case: called by team-lead after a sprint scrum-master finishes

**Design rationale**: No message preservation — inbox files are deleted unconditionally when agent is dead. Keeping large inboxes wastes context (50+ messages). If message history matters, read before cleaning.

#### CLAUDE.md Startup Instruction

Add to **Initialization Process** section:

```
1. Run: atm teams resume atm-dev
   Follow the output to call TeamCreate.
2. Run: atm teams cleanup atm-dev
   Removes stale members and their inboxes.
```

#### Corner Cases

| Scenario | Behavior |
|----------|----------|
| `/compress` in same session | Same session ID → no-op |
| Clean session restart | Old session dead → update + notify |
| Claude crashes (no shutdown hook) | Daemon detects via PID poll → old session dead → update + notify |
| Teammate accidentally calls resume | Identity check → rejected (not team-lead) |
| Two arch-atm instances on same repo | Second call → rejected (old session still alive) |
| Claude loses tmux pane | `--force` to override |
| Stuck process | `--force --kill` to SIGTERM + override |

#### Exit Criteria

**`atm teams resume`:**
- [ ] Outputs correct TeamCreate call on clean restart
- [ ] Same session ID → no-op, no spurious member notifications
- [ ] Non-team-lead caller rejected with clear error
- [ ] Second concurrent team-lead rejected with clear error
- [ ] `--force` overrides alive-session check
- [ ] `--force --kill` terminates old process before resuming
- [ ] Daemon not running → graceful fallback (assume dead, update + warn)

**`atm teams cleanup`:**
- [ ] Dead members removed from `config.json`, inbox deleted
- [ ] Alive members skipped with warning
- [ ] `cleanup atm-dev <agent>` targets single agent correctly
- [ ] Summary output lists removed members

**`atm send` self-send warning:**
- [ ] If sender identity == recipient, prepend warning to message: `[WARNING: Sent to self — identity=<name>, session=<uuid8>. Check ATM_IDENTITY.]`
- [ ] Message is still delivered normally (not blocked)
- [ ] Warning printed to sender's stdout
- [ ] `read` flag NOT set on delivery (message stays unread for legitimate recipient)

**General:**
- [ ] Daemon survives resume/cleanup calls without restart
- [ ] All existing tests pass
- [ ] New unit + integration tests for each corner case

---

## 14. Phase C: Observability + Codex JSON Mode

**Status**: IN PROGRESS
**Goal**: (1) Unified structured logging across all crates so every event — message delivery, tool call, daemon lifecycle — is traceable. (2) Prove that `codex exec --json` can replace the MCP stdio transport for real-time TUI streaming and non-destructive stdin-based nudging.

**Integration branch**: `integrate/phase-C`

### Phase C Sprint Summary

| Sprint | Name | Status | PR |
|--------|------|--------|-----|
| C.1 | Unified logging infrastructure | ✅ | [#125](https://github.com/randlee/agent-team-mail/pull/125), [#128](https://github.com/randlee/agent-team-mail/pull/128) |
| C.2a | Transport trait + McpTransport refactor | ✅ | [#127](https://github.com/randlee/agent-team-mail/pull/127) |
| C.2b | JsonTransport + stdin queue + integration tests | 🔄 IN PROGRESS | — |
| C.3 | Control receiver stub (daemon endpoint + dedupe) | ⏳ PLANNED | — |

**Execution model**: C.2b scrum-master launches as soon as C.2a QA approves — does not wait for C.2a CI/merge. C.2b branches off C.2a feature branch. C.3 starts after C.2b merges to `integrate/phase-C`.

---

### Sprint C.1 — Unified Logging Infrastructure

**Branch**: `feature/pC-s1-unified-logging`
**Worktree**: `../agent-team-mail-worktrees/feature/pC-s1-unified-logging`
**Crate(s)**: `crates/atm-core`, `crates/atm-daemon`, `crates/atm-agent-mcp`
**Owner**: arch-ctm (design + implementation lead)
**Depends on**: nothing
**Status**: Worktree created off develop `aa7c500`; arch-ctm scoping and implementing

#### Problem

ATM has no unified observability layer. Errors surface as stderr noise or are silently swallowed. There is no way to correlate a message delivery failure, a daemon event, and an MCP proxy event back to a single causal chain. Debugging comms reliability requires grepping multiple ad-hoc log calls.

#### Solution

Structured, JSONL-friendly logging built on `tracing` + `tracing-subscriber`:

1. **`atm-core` logging primitives** — shared `init_logging()` that respects `ATM_LOG` / `ATM_LOG_FILE` env vars; structured fields: `session_id`, `agent_id`, `team`, `message_id`, `span` for causal tracing
2. **Per-crate instrumentation** — `#[instrument]` on key paths: message send/receive/mark-read, daemon plugin dispatch, MCP proxy request/response
3. **JSONL log file** — optional `ATM_LOG_FILE=~/.config/atm/atm.log` for persistent structured output; rotated by size/date
4. **Log levels** — `ERROR` (delivery failures, daemon crashes), `WARN` (retries, stale state), `INFO` (message lifecycle), `DEBUG` (protocol frames, tool calls), `TRACE` (raw bytes)
5. **Session/agent correlation** — every log entry carries `session_id` + `agent_name` so events from arch-ctm, team-lead, and the proxy can be correlated in a single log stream

#### Exit Criteria

- [ ] `tracing` + `tracing-subscriber` added to workspace deps
- [ ] `atm_core::logging::init()` initialises env-filter + optional JSONL file appender
- [ ] All three crates call `init()` at startup; no raw `eprintln!` / `println!` in hot paths
- [ ] Message send/receive/mark-read emit `INFO` structured events with `message_id`, `agent`, `team`
- [ ] Daemon plugin dispatch emits `DEBUG` span with plugin name + event type
- [ ] MCP proxy request/response emits `DEBUG` span with `request_id`
- [ ] `ATM_LOG=debug atm send arch-ctm "hi"` shows structured output
- [ ] JSONL file appender writes to `ATM_LOG_FILE` when set
- [ ] All existing tests pass; new unit tests for log field correctness
- [ ] C.2 can add log instrumentation without touching the logging init

---

### Sprint C.2a — Transport Trait + McpTransport Refactor

**Branch**: `feature/pC-s2a-transport-trait`
**Crate(s)**: `crates/atm-agent-mcp`
**Depends on**: C.1 (logging infrastructure)
**Owner**: scrum-master (Claude)

#### Problem

`atm-agent-mcp` proxy is tightly coupled to `codex mcp-server` with no transport abstraction. Adding JSON mode requires a clean seam.

#### Solution

Extract `CodexTransport` trait; wrap existing MCP protocol in `McpTransport`. Zero behaviour change — all existing tests must pass unchanged.

```rust
pub trait CodexTransport: Send {
    async fn send_frame(&mut self, msg: serde_json::Value) -> Result<()>;
    async fn recv_frame(&mut self) -> Result<Option<serde_json::Value>>;
    async fn is_idle(&self) -> bool;
    async fn shutdown(&mut self) -> Result<()>;
}
// McpTransport — Content-Length framed protocol, wraps existing impl
```

#### Exit Criteria

- [ ] `CodexTransport` trait defined in `crates/atm-agent-mcp/src/transport.rs`
- [ ] `McpTransport` wraps existing proxy protocol; no behaviour change
- [ ] Proxy dispatches through trait; `transport = "mcp"` in `.atm.toml` selects `McpTransport`
- [ ] All existing tests pass unchanged
- [ ] C.1 structured log events emitted for transport init/shutdown

---

### Sprint C.2b — JsonTransport + Stdin Queue + Integration Tests

**Branch**: `feature/pC-s2b-json-transport` (branches off `feature/pC-s2a-transport-trait`)
**Crate(s)**: `crates/atm-agent-mcp`
**Depends on**: C.2a QA approval (does not wait for C.2a CI/merge)
**Owner**: scrum-master (Claude)

#### Problem

`atm-agent-mcp` is hardcoded to `codex mcp-server` (MCP stdio protocol). This prevents:
- Non-destructive message injection at natural idle points (no tmux required)
- Running Codex without MCP server overhead
- Future TUI streaming of agent output (Phase D)

`codex exec --json` streams all agent events (messages, tool calls, file changes) as JSONL to stdout and accepts tool results via stdin. This is a simpler, more observable protocol.

#### Solution

`JsonTransport` implementation + stdin queue for message injection.

**Stdin queue for message injection**:
- Queue: `~/.config/atm/agent-sessions/<team>/<agent>/stdin_queue/`
- Atomic rename `<uuid>.json` → `<uuid>.claimed` prevents double-delivery
- Drain on `idle` event in JSONL stream OR 30s timeout
- Messages encoded as Codex tool result JSON (matching `codex exec --json` stdin format)
- Identity injected in initial prompt (same as MCP context injection today)
- TTL cleanup: entries older than 10 minutes deleted on drain

**JSONL Event Schema** (from `codex exec --json`):

| Event type | Meaning |
|------------|---------|
| `agent_message` | Model output text |
| `tool_call` | Tool invocation with name + args |
| `tool_result` | Tool execution result |
| `file_change` | File write/edit event |
| `idle` | Agent waiting for input — **nudge window** |
| `done` | Session complete |

**Nudging mechanism**: `idle` event → drain stdin queue → write ATM messages as tool result JSON to Codex stdin → Codex continues. No tmux needed.

**Note**: TUI streaming (pub/sub fanout of JSONL stream) is deferred to Phase D.

#### Exit Criteria

**JsonTransport:**
- [ ] `JsonTransport` spawns `codex exec --json`, reads JSONL stream, parses all event types
- [ ] Transport selected via config: `transport = "mcp" | "json"` in `.atm.toml`
- [ ] Idle detection fires within 2s of Codex entering wait state

**Stdin queue:**
- [ ] Queue directory created atomically on first use
- [ ] `*.json` → `*.claimed` rename is atomic; no double-delivery under concurrent writers
- [ ] Queue drained on `idle` event or 30s timeout
- [ ] TTL cleanup: entries older than 10 minutes deleted on drain

**Integration:**
- [ ] `codex exec --json` spawned successfully; JSONL stream parsed without panics
- [ ] ATM message injected via stdin queue reaches Codex mid-session
- [ ] C.1 structured log events emitted for every inject/idle/drain cycle

**Local integration tests** (`#[ignore]`, not run in CI):
- [ ] Both transport modes tested with `codex-mini-latest`
- [ ] `atm_send`, `atm_read`, `atm_broadcast`, `atm_pending_count` verified in MCP mode
- [ ] `atm_send`, `atm_read`, `atm_broadcast`, `atm_pending_count` verified in JSON mode
- [ ] Run with: `cargo test --test mcp_integration -- --ignored`

**Docs:**
- [ ] `codex exec --json` event schema documented in `docs/codex-json-schema.md`
- [ ] ADR written: `docs/adr/003-json-mode-transport.md`
- [ ] All existing tests pass

---

### Sprint C.3 — Control Receiver Stub (Daemon Endpoint + Validation + Dedupe)

**Branch**: `feature/pC-s3-control-receiver`
**Worktree**: `../agent-team-mail-worktrees/feature/pC-s3-control-receiver`
**Crate(s)**: `crates/atm-daemon` (socket handler, dedup module), `crates/atm-core` (control message types)
**Depends on**: C.2b merged to `integrate/phase-C`
**Design refs**: `docs/tui-mvp-architecture.md`, `docs/tui-control-protocol.md`

#### Problem

The TUI control protocol (`docs/tui-control-protocol.md`) defines `control.stdin.request` and `control.interrupt.request` message contracts, but the daemon Unix socket server has no handler for either. Phase D (TUI) cannot build interactive controls without a real receiver endpoint to target. Building the receiver in Phase C de-risks protocol drift and lets Phase D UI work build on proven contract behavior.

#### Solution

Three self-contained components (can be developed with one dev agent in sequence):

1. **Control message types** (`crates/atm-core/src/control.rs` or `crates/atm-daemon/src/control.rs`):
   - `ControlRequest` struct: `v`, `request_id`, `sent_at`, `team`, `session_id`, `agent_id`, `sender`, `action` (`stdin` | `interrupt`), `payload: Option<String>`, `content_ref: Option<ContentRef>`
   - `ControlAck` struct: `request_id`, `result: ControlResult`, `duplicate: bool`, `detail: Option<String>`, `ack_at`
   - `ControlResult` enum: `Ok`, `NotLive`, `NotFound`, `Busy`, `Timeout`, `Rejected`, `InternalError`
   - `ContentRef` struct: `path`, `size_bytes`, `sha256`, `mime`, `expires_at: Option<DateTime>`
   - Full serde round-trip with snake_case field names

2. **Dedupe store** (`crates/atm-daemon/src/dedup.rs`):
   - Key: `(team, session_id, agent_id, request_id)` as a composite `DedupeKey` newtype
   - Bounded `HashMap<DedupeKey, Instant>` — capacity 1 000 entries (configurable via `ATM_DEDUP_CAPACITY`)
   - TTL: 10 minutes (configurable via `ATM_DEDUP_TTL_SECS`)
   - `check_and_insert(&mut self, key: DedupeKey) -> bool` — returns `true` if duplicate (key already present and not expired); inserts with current timestamp on non-duplicate
   - Eviction: on capacity overflow, evict oldest entry (LRU approximation via insertion order)
   - **Not** async — plain `Mutex<DedupeStore>` in daemon shared state

3. **Daemon socket control handler** (extend `crates/atm-daemon/src/daemon/socket.rs`):
   - New `SocketCommand::Control` variant in the existing command enum; payload is raw JSON deserialized into `ControlRequest`
   - Envelope validation: required fields present, `v == 1`, payload ≤ 1 MiB hard limit; return `ControlResult::Rejected` with detail on failure
   - Age check: reject requests where `sent_at` skew > 5 minutes (configurable)
   - Dedupe: call `check_and_insert`; if duplicate return `ControlAck { result: Ok, duplicate: true }` immediately
   - Live-state lookup: resolve `session_id` + `agent_id` via `SessionRegistry`; compute `live = SessionStatus::Active AND AgentState in {Idle, Busy}`; return `NotLive` if false; return `NotFound` if session unknown
   - `control.stdin.request`: forward payload to `stdin_queue::enqueue()` for the target session; return `Ok`
   - `control.interrupt.request`: return `Rejected` with `"interrupt receiver path not yet implemented"`
   - Emit C.1 log event via `emit_event_best_effort` for every request + ack (level info, action `control_stdin_request` / `control_stdin_ack`)

#### Parallelism Note

C.3 is a single sprint (one SM, one dev agent). The three components are naturally sequential: types → dedupe → handler. Total scope is ~400–600 lines of new code. No parallel track needed.

#### Exit Criteria

- [ ] `ControlRequest`, `ControlAck`, `ControlResult`, `ContentRef` types defined with serde round-trip tests
- [ ] `DedupeStore` with TTL + capacity eviction; unit tests: insert, duplicate, TTL expiry, capacity overflow
- [ ] `SocketCommand::Control` handler dispatches `stdin` and `interrupt` actions
- [ ] Envelope validation rejects missing fields, oversized payload (> 1 MiB), stale `sent_at` (> 5 min skew)
- [ ] Live-state check: `SessionStatus::Active AND AgentState in {Idle, Busy}` → live; all other combos → `not_live`
- [ ] `control.stdin.request` forwards payload to `stdin_queue::enqueue()` and returns `Ok`
- [ ] `control.interrupt.request` returns `Rejected` with implementation-pending detail
- [ ] Duplicate requests return `ControlAck { result: Ok, duplicate: true }` without re-executing
- [ ] `emit_event_best_effort` called for every request + ack (C.1 pattern)
- [ ] `content_ref` path validated: must resolve under `get_home_dir()/.config/atm/share/`, canonicalized, no `..` traversal, no symlink escape; `sha256` verified before use
- [ ] Integration test (ignored): end-to-end control request over Unix socket with mock session registry
- [ ] `cargo clippy --workspace -- -D warnings` clean
- [ ] `cargo test --workspace` passes

---

## 15. Phase D: TUI Streaming

**Status**: PLANNED (blocked on Phase C — specifically C.3 for D.2)
**Goal**: Real-time terminal UI for observing and controlling live Codex agent sessions. D.1 delivers a read-only streaming view; D.2 adds interactive controls (stdin injection, interrupt). D.1 and D.2 run **in parallel** — they touch different modules (read path vs. write path).

**Integration branch**: `integrate/phase-D`
**Design refs**: `docs/tui-mvp-architecture.md`, `docs/tui-control-protocol.md`

### Phase D Sprint Summary

| Sprint | Name | Depends On | Status |
|--------|------|------------|--------|
| D.1 | TUI crate + live stream view (read-only) | C.2b | ✅ [#136](https://github.com/randlee/agent-team-mail/pull/136) |
| D.2 | Interactive controls (stdin inject, interrupt) | C.3 | 🔄 IN PROGRESS |
| D.3 | Identifier cleanup + user demo | D.1 + D.2 (independent) | 🔄 IN PROGRESS |

**Execution model**: D.1 and D.2 launch in parallel after Phase C integration merges to develop. D.1 needs only C.2b (session log tail + pub/sub). D.2 needs C.3 (control receiver endpoint).

---

### Sprint D.1 — TUI Crate + Live Stream View

**Branch**: `feature/pD-s1-tui-stream`
**Crate(s)**: `crates/atm-tui` (new binary crate)
**Depends on**: C.2b merged (JsonTransport + session log infrastructure)

#### Scope

1. **New `atm-tui` binary crate** — `ratatui`-based terminal UI, single binary `atm-tui`
2. **Dashboard view** (left panel): team member list with ATM inbox counts sourced from daemon pub/sub (`agent-state` command); mail action via `atm send` semantics (no direct control path in Dashboard)
3. **Agent Terminal view** (right panel): scrolling JSONL output stream tailed from session log file (`${ATM_HOME}/.config/atm/agent-sessions/{team}/{agent_id}/output.log`); full event column names (no abbreviated headers per `docs/tui-mvp-architecture.md §2.1`)
4. **Session selector**: navigate between active sessions via daemon `list-agents` socket command
5. **No interactive input in D.1** — read-only; input field is visible but disabled ("control available in next release")
6. **C.1 logging**: emit structured events for TUI startup, session connect, stream attach/detach

#### Exit Criteria

- [ ] `atm-tui` binary builds and runs with `--team <name>` flag
- [ ] Dashboard view lists team members with inbox counts (daemon-sourced, refreshes on pub/sub event)
- [ ] Agent Terminal view tails session log file in real time (100 ms poll interval)
- [ ] Session selector navigates between sessions without crashing
- [ ] Input field visible but disabled with placeholder text
- [ ] Graceful exit on `q` / `Ctrl-C`
- [ ] `cargo clippy --workspace -- -D warnings` clean
- [ ] `cargo test --workspace` passes

---

### Sprint D.2 — Interactive Controls (Stdin Inject + Interrupt)

**Branch**: `feature/pD-s2-tui-controls`
**Crate(s)**: `crates/atm-tui` (extend D.1), `crates/atm-daemon` (C.3 receiver already done)
**Depends on**: D.1 merged + C.3 merged (control receiver endpoint)

#### Scope

1. **Agent Terminal input field** — enabled when target session is `live` (`SessionStatus::Active AND AgentState in {Idle, Busy}`)
2. **Stdin injection path**: on Enter, build `ControlRequest { action: "stdin", payload: text }` with stable `request_id` (UUID v4), send via Unix socket to daemon, display ack result in status bar
3. **Live-state gate**: input field disabled (greyed out) when `not_live`; status bar shows reason (`Launching`, `Killed`, etc.)
4. **Retry on timeout**: single retry after 2 s ack timeout; surface `timeout` result to user if second attempt fails
5. **Interrupt button**: keyboard shortcut (`Ctrl-I`); sends `ControlRequest { action: "interrupt" }`; currently returns `Rejected` with detail — display detail in status bar (not an error state in TUI)
6. **UX boundary enforcement**: Dashboard remains mail-only (no control path); Agent Terminal remains control-only (no ATM send path); no silent fallback between channels (per `docs/tui-mvp-architecture.md §4`)
7. **Audit logging**: C.1 `emit_event_best_effort` for every control send and ack received

#### Exit Criteria

- [ ] Input field enabled/disabled based on live-state (daemon-sourced)
- [ ] Stdin injection sends `control.stdin.request` to daemon and displays ack result
- [ ] Duplicate request detection: retry uses same `request_id`; `duplicate: true` ack surfaces as "already delivered" in status bar (not an error)
- [ ] Interrupt shortcut (`Ctrl-I`) sends `control.interrupt.request`; `Rejected` result displayed with detail
- [ ] Live-state gate: `Launching`/`Killed`/`Stale`/`Closed` all shown as not-live with reason
- [ ] Dashboard has no control input — mail action calls `atm send` CLI
- [ ] `emit_event_best_effort` called for every control send + ack
- [ ] `cargo clippy --workspace -- -D warnings` clean
- [ ] `cargo test --workspace` passes

---

### Sprint D.3 — Identifier Cleanup + User Demo

**Branch**: `feature/pD-s3-identifier-cleanup`
**Crate(s)**: `docs/`, `crates/atm-daemon/` (doc comment only), `demos/`
**Depends on**: D.1 + D.2 (can run independently of D.1/D.2 — docs-only + demo script)

**Deliverables**:

1. **Enforce `agent_id` as the only public TUI identifier** — mark `thread_id` as MCP-internal in `docs/tui-control-protocol.md` and `docs/tui-mvp-architecture.md`; remove `thread_id` from public example payloads
2. **Audit `hook_watcher.rs`** — confirm `HookEvent.thread_id` is Codex-adapter-only; add doc comment clarifying it is not exposed in the public API
3. **Regression check** — `rg "thread_id|threadId" docs/tui-*.md crates/atm/src crates/atm-daemon/src` must return only approved MCP-internal exceptions (documented in `docs/thread-id-audit.md`)
4. **User demo script** — `demos/tui-demo.sh` committed and runnable from clean checkout; covers dashboard, agent terminal, control send/ack, and one degraded scenario (daemon unavailable or `not_live` target)
5. **Demo artifacts** — `demos/README.md` with sign-off notes

**Acceptance Criteria**:
- [ ] No `thread_id` in `docs/tui-*.md` payload definitions/examples outside of explicit MCP-internal notes
- [ ] `hook_watcher.rs` `thread_id` field has doc comment explicitly marking it adapter-only
- [ ] `docs/thread-id-audit.md` lists all approved exceptions with rationale
- [ ] `rg` audit command documented and expected output shown in `docs/thread-id-audit.md`
- [ ] `demos/tui-demo.sh` committed and runnable; includes one degraded scenario
- [ ] `demos/README.md` with team-lead sign-off
- [ ] `cargo clippy --workspace -- -D warnings` clean
- [ ] `cargo test --workspace` passes

---

## 16. Phase E: ATM Core Bug Fixes (Priority)

**Status**: IN PROGRESS
**Goal**: Fix two known bugs in ATM core that require manual workarounds every session, and wire Claude Code hooks to daemon state.
**Integration branch**: `integrate/phase-E`

### Phase E Sprint Summary

| Sprint | Name | Depends On | Status |
|--------|------|------------|--------|
| E.1 | Fix `atm teams resume` session ID reliability | Phase D | ✅ MERGED |
| E.2 | Fix inbox read marking other agents' messages as read | — | ✅ MERGED |
| E.3 | Hook-to-daemon state bridge | E.1 | ✅ PR #152 |

**Execution model**: E.1 is priority (unblocks session startup). E.2 and E.3 can run in parallel after E.1 merges.

---

### Sprint E.1 — Fix `atm teams resume` session ID reliability

**Branch**: `feature/pE-s1-resume-session-id`
**GitHub Issue**: #141
**Crate(s)**: `crates/atm` (CLI), `crates/atm-core`
**Depends on**: Phase D complete

#### Problem

`atm teams resume <team>` reads `CLAUDE_SESSION_ID` from the process environment to update `leadSessionId` in `config.json`. However, `CLAUDE_SESSION_ID` is not reliably exported to bash subshells — the Rust binary reads a stale or empty value, setting the wrong session ID. The gate hook (`gate-agent-spawns.py`) gets the real session ID from Claude Code's tool call JSON, causing a mismatch that blocks named teammate spawning.

#### Scope

1. `atm teams resume` accepts `--session-id <id>` flag for explicit override (Option B)
2. Fallback: read session ID from gate debug log (`/tmp/gate-agent-spawns-debug.jsonl`) when env var is absent/stale
3. Or: gate hook writes a stable session ID file (`/tmp/atm-session-id`) that `atm` reads reliably (Option A)
4. Update `CLAUDE.md` initialization process to use the fixed command reliably

> *Note: Scope item 2 (reading gate debug log) was superseded by Scope item 3 (`/tmp/atm-session-id` file). Both Options A and B are implemented: the gate hook writes `/tmp/atm-session-id` on every Task tool call (Option A), and `--session-id` provides an explicit override (Option B).*

#### Exit Criteria

- [ ] `atm teams resume atm-dev` sets `leadSessionId` to the correct current session ID without manual Python workaround
- [ ] `atm teams resume atm-dev --session-id <uuid>` explicit flag works and writes that ID to `config.json`
- [ ] Fallback order verified: `--session-id` flag → `CLAUDE_SESSION_ID` env → `/tmp/atm-session-id` file → error with helpful message
- [ ] Gate hook writes `/tmp/atm-session-id` on every Task tool call (pass and block paths)
- [ ] Verification: gate hook allows named teammate spawning immediately after `atm teams resume`
- [ ] CLAUDE.md initialization process updated (no Python workaround)
- [ ] `cargo clippy --workspace -- -D warnings` clean
- [ ] `cargo test --workspace` passes

---

### Sprint E.2 — Fix inbox read marking other agents' messages as read

**Branch**: `feature/pE-s2-inbox-read-scoping`
**GitHub Issue**: #142
**Crate(s)**: `crates/atm` (CLI), `crates/atm-core`
**Depends on**: E.1 (or parallel)

#### Problem

When team-lead runs `atm read` or `atm inbox`, ATM marks messages as `read: true` in **all agents' inboxes**, not just the calling agent's inbox. This causes arch-ctm to silently miss messages that were marked read by team-lead's polling before arch-ctm saw them.

#### Scope

1. `atm read` / `atm inbox` only marks messages read in the **calling agent's** inbox
2. Never touch other agents' inbox `read` flags during polling
3. Add test: team-lead `atm read` does not modify arch-ctm's inbox read status

#### Exit Criteria

- [ ] `atm read` only modifies calling agent's own inbox
- [ ] arch-ctm's messages remain `read: false` after team-lead polls
- [ ] Integration test confirms inbox isolation
- [ ] `cargo clippy --workspace -- -D warnings` clean
- [ ] `cargo test --workspace` passes

---

### Sprint E.3 — Hook-to-Daemon State Bridge

**Branch**: `feature/pE-s3-hook-daemon-bridge`
**Crate(s)**: `crates/atm-daemon` (new hook event handler), `.claude/scripts/` (hook updates)
**Depends on**: E.1 (daemon socket path stabilized)
**Parallel with**: E.2

#### Problem

The `TeammateIdle` relay (`teammate-idle-relay.py`) writes idle events to a flat file (`~/.claude/daemon/hooks/events.jsonl`) but the daemon never reads it. The daemon's session registry (Phase 10 / C.3) is populated only by explicit socket calls — there is no path from Claude Code hook events to daemon state. As a result:

- `atm teams cleanup` liveness checks fall back to PID polling instead of daemon state
- `atm teams resume` daemon guard cannot verify existing lead liveness reliably
- The TUI live-state gate queries daemon state that is never updated from real hook events

#### Solution

Two lightweight components:

**1. Hook scripts (Python, cross-platform, with unit tests)**

All hook scripts are Python (not bash) for cross-platform compatibility and testability. Three scripts send `hook_event` socket messages to the daemon:

- `session-start.py` — replaces `session-start.sh`; handles `SessionStart`
- `session-end.py` — new; handles `SessionEnd`
- `teammate-idle-relay.py` — extended to also send socket message

**`.atm.toml` guard** (all three scripts): before sending any daemon socket message, check for `.atm.toml` in `cwd`. If absent, skip the socket call entirely. This prevents daemon state from being polluted by unrelated Claude Code sessions on the same machine.

Socket message types (new `hook_event` variant):
```json
{"type": "hook_event", "event": "session_start", "session_id": "...", "agent": "...", "team": "...", "source": "init|compact"}
{"type": "hook_event", "event": "teammate_idle",  "session_id": "...", "agent": "...", "team": "..."}
{"type": "hook_event", "event": "session_end",    "session_id": "...", "agent": "...", "team": "...", "reason": "..."}
```

**Unit tests** live alongside each script in `.claude/scripts/tests/`:
- `test_session_start.py` — mock socket, verify message shape, verify `.atm.toml` guard, verify fail-open on socket error
- `test_session_end.py` — same pattern for session_end
- `test_teammate_idle_relay.py` — extend existing tests with socket message assertions

**2. Daemon hook event handler**

Add `SocketCommand::HookEvent` variant. Handler updates session registry and agent activity state:
- `session_start` → register/refresh session; set agent state to `Active`
- `teammate_idle` → set agent state to `Idle` for the named session
- `session_end` → mark session as `Dead`; update agent state to `Closed`

#### Scope Notes

- No new daemon persistence — in-memory state only (existing registry)
- All hooks remain fail-open, always exit `0`
- `events.jsonl` file write is kept as durable audit trail; socket call is additive
- Socket path lookup: `${ATM_HOME:-$HOME}/.claude/daemon/atm-daemon.sock`
- Global `~/.claude/settings.json` updated to add `SessionEnd` hook pointing to `session-end.py`

#### Exit Criteria

- [ ] `session-start.py` replaces `session-start.sh`; sends `hook_event/session_start` **only when `.atm.toml` present in cwd**
- [ ] `session-end.py` (new) added to global `~/.claude/settings.json`; sends `hook_event/session_end` **only when `.atm.toml` present in cwd**
- [ ] `teammate-idle-relay.py` sends `hook_event/teammate_idle` **only when `.atm.toml` present in cwd**
- [ ] All hooks remain fail-open — exit `0` even if socket call fails or `.atm.toml` is absent
- [ ] Unit tests in `.claude/scripts/tests/` cover: message shape, `.atm.toml` guard, socket-error fail-open, daemon-not-running fail-open
- [ ] Daemon handles `SocketCommand::HookEvent`; updates session registry + agent state
- [ ] `session_start` → `Active`; `teammate_idle` → `Idle`; `session_end` → `Dead`/`Closed`
- [ ] Integration test: hook event over socket → daemon state query reflects updated state
- [ ] `cargo clippy --workspace -- -D warnings` clean
- [ ] `cargo test --workspace` passes

---

### Sprint E.4 — TUI Reliability Hardening (Restart, Reconnect, Failure Injection)

**Branch**: `feature/pE-s4-tui-reliability`
**Crate(s)**: `crates/atm-tui`, `crates/atm-daemon` (dedupe store, socket handler)
**Depends on**: E.3 (hook-to-daemon bridge provides live state updates)
**Design refs**: `docs/tui-mvp-architecture.md` §12, §14; `docs/tui-control-protocol.md` §5, §11

#### Problem

Phase D shipped a functional TUI MVP with in-memory dedupe, basic retry, and log-tail streaming. Several reliability gaps remain from the MVP vs Hardening boundary defined in the TUI design docs:

- Dedupe store is memory-only — duplicates possible after daemon restart
- No failure-injection tests for daemon restart, stale sessions, or queue backlog
- Stream source interruption auto-reconnect is untested under sustained load
- Control ack timeout behavior and backpressure are not validated end-to-end

#### Scope

1. **Restart-safe dedupe store**: replace in-memory dedupe with a durable store (file-backed or SQLite) that survives daemon restart. Bounded TTL window (10 min recommended) with atomic lookup/insert.
2. **Failure-injection test suite**: automated tests covering:
   - Daemon restart mid-stream (TUI reconnects, no lost events)
   - Stale session cleanup (TUI detects and displays degraded state)
   - Queue backlog under sustained control input (no UI starvation)
   - Socket unavailable → graceful degradation → reconnect on availability
3. **Stream reconnect hardening**: verify auto-reconnect to log tail after source interruption; add explicit source/error indicator on freeze.
4. **`sent_at` skew validation**: reject control requests older than configurable max age; bounded clock-skew tolerance window.

#### Exit Criteria

- [ ] Dedupe store survives daemon restart — integration test: send request, restart daemon, retry same `request_id`, get `duplicate: true`
- [ ] Failure-injection tests pass: daemon restart, stale session, queue backlog, socket unavailable
- [ ] Stream auto-reconnect verified under sustained load (no silent data loss)
- [ ] `sent_at` skew rejection implemented and tested
- [ ] `cargo clippy --workspace -- -D warnings` clean
- [ ] `cargo test --workspace` passes

---

### Sprint E.5 — TUI Performance, UX Polish, and Operational Validation

**Branch**: `feature/pE-s5-tui-polish`
**Crate(s)**: `crates/atm-tui`, docs
**Depends on**: E.4 (reliability hardening provides stable base for perf testing)
**Design refs**: `docs/tui-mvp-architecture.md` §7, §14, §15

#### Problem

The TUI MVP is functional but untested under sustained real-world load. Several UX and operational items were deferred from Phase D:

- No performance validation under sustained stream + control activity
- Interrupt confirmation policy not finalized (`always`/`never`/`configurable`)
- No per-user UI preferences file
- No documented SLO targets for render responsiveness or ack visibility latency
- No operational runbooks or troubleshooting guidance for TUI operators

#### Scope

1. **Performance validation**: stress test with sustained stream output + concurrent control activity. Measure and document render responsiveness and control-ack visibility latency. Define SLO targets.
2. **Interrupt confirmation policy**: implement configurable policy (`always`, `never`, `confirm`) for Ctrl+C interrupt in Agent Terminal. Default: `confirm`.
3. **Per-user UI preferences**: define file location (`~/.config/atm/tui.toml` or similar) and schema for:
   - Interrupt confirmation policy
   - Follow-mode default
   - Color theme (if applicable)
   - Default filters for event log
4. **Richer timeout/backoff policy**: configurable per-action-type timeout and backoff (stdin vs interrupt may have different latency tolerance).
5. **Operational documentation**: troubleshooting guide covering common failure modes, daemon connectivity issues, and TUI recovery steps.

#### Exit Criteria

- [ ] Stress test passes: sustained stream + control activity without UI starvation (documented SLO met)
- [ ] Interrupt confirmation policy is configurable via preferences file
- [ ] Per-user preferences file (`tui.toml`) schema defined, parsed, and applied on TUI startup
- [ ] Per-action-type timeout/backoff configurable
- [ ] Operational troubleshooting section added to TUI docs
- [ ] `cargo clippy --workspace -- -D warnings` clean
- [ ] `cargo test --workspace` passes

---

### Phase E Sprint Summary

| Sprint | Name | Depends On | Status |
|--------|------|------------|--------|
| E.1 | `atm teams resume` session ID reliability | Phase D | ✅ MERGED (#147) |
| E.2 | Inbox read scoping | E.1 (or parallel) | 🔄 IN PROGRESS |
| E.3 | Hook-to-daemon state bridge | E.1 (parallel with E.2) | 🔄 IN PROGRESS |
| E.4 | TUI reliability hardening (restart, reconnect, failure injection) | E.3 | ⏳ PLANNED |
| E.5 | TUI performance, UX polish, and operational validation | E.4 | ⏳ PLANNED |

**Execution model**: E.1–E.3 are bug fixes / infrastructure. E.4–E.5 are TUI hardening deferred from Phase D design docs (`tui-mvp-architecture.md` §14, `tui-control-protocol.md` §11). E.2 ∥ E.3 after E.1. E.4 after E.3. E.5 after E.4.

---

## 16.5 Phase F: Team Installer (`atm team init`)

**Status**: PLANNED
**Goal**: Allow orchestration packages — hook scripts, agent prompts, and skills — to be installed into `~/.claude/` with a single command, with clean separation between machine-global hooks and project-scoped orchestration.
**Integration branch**: `integrate/phase-F`

### Motivation

Currently, setting up a new machine or onboarding a new project requires manually copying scripts, agents, and skills and hand-editing `settings.json`. `atm team init atm-dev` does all of this in one command. Multiple named orchestration packages can be composed per-project (e.g., `rust-sprint`, `docs-review`, `generic-dev`).

### Two Install Scopes

#### 1. Global (machine-level) — install once

Hook scripts and their `~/.claude/settings.json` entries that apply across **all** Claude Code sessions on the machine:

- `session-start.py` → `~/.claude/scripts/`
- `session-end.py` → `~/.claude/scripts/`
- Hook entries for `SessionStart` and `SessionEnd` → `~/.claude/settings.json` (**global**)

These are installed **once** per machine. `atm team init` skips them if already present (checks script SHA256 + settings entry). `--force` to update.

#### 2. Project/orchestration (project-level) — per repo or per workflow

Orchestration packages install into the **project's** `.claude/` directory:

- Hook scripts (gate, TeammateIdle relay) → `.claude/scripts/`
- Hook entries → `.claude/settings.json` (**project-level**, committed to repo)
- Agent prompts → `.claude/agents/`
- Skills → `.claude/skills/`

Multiple named orchestration packages can be installed in the same project. Each is self-contained. Examples:
- **`rust-sprint`** — scrum-master, rust-developer, rust-qa-agent, gate hook, phase-orchestration skill
- **`docs-review`** — docs-agent, review-agent, lighter gate config
- **`generic-dev`** — general-purpose scrum-master, minimal hooks

### Package Format

Packages live in `.claude/packages/<name>/` within the repo:

```
.claude/
  packages/
    rust-sprint/
      manifest.toml          ← metadata + install map
      scripts/               ← project-level hook scripts (Python)
        gate-agent-spawns.py
        teammate-idle-relay.py
        tests/
      agents/
        scrum-master.md
        rust-developer.md
        rust-qa-agent.md
      skills/
        phase-orchestration/
        rust-development/
      project-hooks.json     ← hook entries for .claude/settings.json
    global/
      manifest.toml          ← scope = "global"
      scripts/
        session-start.py
        session-end.py
        tests/
      global-hooks.json      ← hook entries for ~/.claude/settings.json
```

**`manifest.toml`**:
```toml
[package]
name = "rust-sprint"
version = "0.1.0"
description = "Rust sprint orchestration — scrum-master, dev, QA, gate hook"
atm_min_version = "0.15.0"
scope = "project"           # "project" | "global"

[install]
scripts = "scripts/"        # → .claude/scripts/ (project) or ~/.claude/scripts/ (global)
agents  = "agents/"         # → .claude/agents/
skills  = "skills/"         # → .claude/skills/
hooks   = "project-hooks.json"  # → merged into .claude/settings.json
```

### `atm team init` Command

```
atm team init <team> [--package <name>] [--dry-run] [--force]
```

1. Read team definition from `.atm/teams/<team>/` (or cwd `.atm.toml`)
2. Install **global package** first (scope=global), if not already installed
3. Install **orchestration package(s)** into project `.claude/` (scope=project)
   - Default: install all packages listed in team definition
   - `--package <name>`: install only the named package
4. Write receipt to `~/.atm/installed/<team>/receipt.toml` (global) and `.atm/installed/<package>.toml` (project)
5. Print summary per scope: global (skipped/installed), project (files copied, hooks merged)

### `settings.json` Surgery — Insert/Remove Only

`atm team init` and `atm team uninstall` perform **surgical edits** to `settings.json` — they never rewrite the file wholesale:

- **Install**: read existing JSON, append new hook entries under the appropriate event key, write back. Existing entries from other tools, plugins, or manual configuration are preserved untouched.
- **Uninstall**: read existing JSON, remove only the exact hook entries that were added by this package (matched by command string from receipt), write back. No other entries are touched.
- **Duplicate detection**: before inserting, check if the exact command string is already present under the event key. If so, skip — idempotent.
- **JSON formatting**: preserve existing indentation/style where possible; fall back to 2-space indent if file must be reformatted.
- **Atomic write**: write to a temp file, then rename — never leave `settings.json` in a partial state.

This ensures `atm team init` is safe to run even when the user has manually configured other hooks, plugins (e.g., LSP, nuget-publishing), or project-specific settings.

### Conflict Handling

| Scenario | Behavior |
|----------|----------|
| Global script already installed (identical) | Skip — "already up to date" |
| Global script already installed (different) | Skip with warning — use `--force` to update |
| Project file already exists | Overwrite (project files are owned by the package) |
| Hook entry already present (exact command match) | Skip duplicate — idempotent |
| Other hook entries present in settings.json | Left untouched — surgical insert/remove only |
| ATM version too old | Error with minimum version required |

### Design Discussion: Onboarding Warnings and Setup Nudges

Phase F must address the gap between "ATM hooks are installed globally" and "this project hasn't been set up for ATM." The E.3 review exposed that global hooks fire in every Claude Code session — they must be completely inert in non-ATM projects, but they also represent a natural injection point for reminding users to configure ATM when it would be useful.

#### Key Questions to Resolve in F.1

1. **Where should "ATM not configured" warnings appear?**
   - `SessionStart` hook: currently prints `SESSION_ID=...` and team context when `.atm.toml` is present, prints nothing otherwise. Should it print a one-line nudge like `ATM available but not configured for this project. Run: atm team init <team>` when ATM is installed globally but `.atm.toml` is absent?
   - Pros: visible on every session start, hard to miss
   - Cons: noisy for users who intentionally don't want ATM in certain projects
   - Compromise: only show the nudge if a `.claude/` directory exists in cwd (suggesting it's a Claude Code project that might benefit from ATM)

2. **Opt-out mechanism for nudges**
   - Users who don't want ATM in a project should be able to silence the nudge permanently
   - Options:
     - `atm team init --skip` creates a minimal `.atm.toml` with `disabled = true` (suppresses all nudges)
     - A global `~/.atm/config.toml` with `suppress_nudges = true` (machine-wide opt-out)
     - An env var `ATM_QUIET=1` for temporary suppression
   - Recommendation: support all three, checked in order: env var → project `.atm.toml` `disabled` → global config

3. **What triggers a setup reminder vs silence?**

   | Condition | Behavior |
   |-----------|----------|
   | No ATM binary on PATH | Complete silence — hooks exit immediately |
   | ATM installed, no `.atm.toml`, no `.claude/` dir | Silence — not a Claude project |
   | ATM installed, no `.atm.toml`, `.claude/` dir exists | Candidate for nudge — Claude project without ATM |
   | ATM installed, `.atm.toml` with `disabled = true` | Silence — user opted out |
   | ATM installed, `.atm.toml` present and valid | Full ATM functionality |

4. **`atm team init` as the canonical setup entry point**
   - The nudge message should always point to `atm team init <team>` as the single command to run
   - `atm team init` must handle the full onboarding: create `.atm.toml`, install project hooks, set up agent prompts
   - For first-time users: `atm team init --discover` could list available built-in packages and guide selection

5. **Post-install verification**
   - After `atm team init`, the next `SessionStart` hook should confirm setup: `ATM configured: team=atm-dev, packages=[rust-sprint]`
   - `atm team status` could show current project setup health: installed packages, hook integrity (SHA256 check), missing components

6. **Interaction with `atm team uninstall`**
   - Uninstall should offer to either remove `.atm.toml` entirely (full cleanup) or set `disabled = true` (suppress nudges but keep the marker)
   - If `.atm.toml` is removed, the nudge returns on next session (by design — it's re-discoverable)

7. **Global hooks safety contract (learned from E.3 review)**
   - All global hooks MUST check `.atm.toml` as the first operation in `main()`
   - No file I/O, no directory creation, no socket connections before the guard passes
   - This invariant must be enforced in F.3's built-in package tests
   - Consider a shared `atm_guard()` utility function that all hooks import, ensuring consistent guard behavior

### Phase F Sprint Summary

| Sprint | Name | Depends On | Status |
|--------|------|------------|--------|
| F.1 | Package format + `atm team init` (global + project scopes) | Phase E | ⏳ PLANNED |
| F.2 | `atm team uninstall` + receipt tracking | F.1 | ⏳ PLANNED |
| F.3 | Ship built-in packages: `global`, `rust-sprint`, `generic-dev` | F.1 | ⏳ PLANNED |

**Execution model**: F.1 is the MVP. F.2 and F.3 can run in parallel after F.1 merges.

---

### Sprint F.1 — Package Format + `atm team init`

**Branch**: `feature/pF-s1-team-init`
**Crate(s)**: `crates/atm` (new `team init` subcommand), `crates/atm-core` (manifest parsing, file copy, settings merge)
**Depends on**: Phase E complete

#### Exit Criteria

- [ ] `manifest.toml` schema defined and parsed by `atm-core`
- [ ] `atm team init <team>` copies scripts, agents, skills to `~/.claude/` subdirs
- [ ] Hook entries in `settings-hooks.json` merged non-destructively into `~/.claude/settings.json`
- [ ] `--dry-run` prints planned changes, makes no disk writes
- [ ] `--force` overwrites differing files
- [ ] Conflict handling: skip-identical, warn-on-different without `--force`
- [ ] Install receipt written to `~/.atm/installed/<team>/receipt.toml`
- [ ] ATM version compatibility check against `manifest.toml` `atm_min_version`
- [ ] Global scope: installs to `~/.claude/scripts/`, merges into `~/.claude/settings.json`; skips if already present (SHA256 match)
- [ ] Project scope: installs to `.claude/scripts/`, `.claude/agents/`, `.claude/skills/`, merges into `.claude/settings.json`
- [ ] `--dry-run` prints planned changes per scope, makes no disk writes
- [ ] `--force` overwrites differing global files
- [ ] `--package <name>` installs only the named package
- [ ] Global receipt written to `~/.atm/installed/<team>/receipt.toml`; project receipt to `.atm/installed/<package>.toml`
- [ ] `.atm.toml` created/updated in cwd with `default_team = "<team>"`
- [ ] Integration test: fresh temp dir → `atm team init` → verify global + project files installed, both settings.json updated
- [ ] `cargo clippy --workspace -- -D warnings` clean
- [ ] `cargo test --workspace` passes

---

### Sprint F.2 — `atm team uninstall` + Receipt Tracking

**Branch**: `feature/pF-s2-team-uninstall`
**Crate(s)**: `crates/atm` (new `team uninstall` subcommand), `crates/atm-core` (receipt read, selective removal)
**Depends on**: F.1

#### Exit Criteria

- [ ] `atm team uninstall <team>` removes only receipt-tracked files unmodified since install (SHA256 check)
- [ ] Global files: skip with warning if modified; remove from `~/.claude/settings.json` hook entries
- [ ] Project files: removed unconditionally (project owns them)
- [ ] Receipt deleted after successful uninstall
- [ ] `--force` removes all installed files including modified globals
- [ ] `cargo clippy --workspace -- -D warnings` clean
- [ ] `cargo test --workspace` passes

---

### Sprint F.3 — Built-in Packages: `global`, `rust-sprint`, `generic-dev`

**Branch**: `feature/pF-s3-builtin-packages`
**Crate(s)**: `.claude/packages/` (package directories), `crates/atm` (bundle packages into binary or reference repo path)
**Depends on**: F.1 (parallel with F.2)

#### Packages

**`global`** (scope: global) — machine-level hooks, installed once:
- `session-start.py`, `session-end.py` with unit tests
- `SessionStart` + `SessionEnd` entries for `~/.claude/settings.json`

**`rust-sprint`** (scope: project) — full Rust sprint orchestration:
- `gate-agent-spawns.py`, `teammate-idle-relay.py` with unit tests
- Agent prompts: `scrum-master.md`, `rust-developer.md`, `rust-qa-agent.md`, `atm-qa-agent.md`
- Skills: `phase-orchestration/`, `rust-development/`
- `PreToolUse` + `TeammateIdle` entries for `.claude/settings.json`

**`generic-dev`** (scope: project) — lighter orchestration for non-Rust projects:
- Same hooks as `rust-sprint`
- Agent prompts: `scrum-master.md`, `general-purpose` dev, `general-purpose` QA
- Skills: `phase-orchestration/` only

#### Exit Criteria

- [ ] All three packages ship as directories under `.claude/packages/` in the repo
- [ ] `atm team init atm-dev` installs `global` + `rust-sprint` by default (per team definition)
- [ ] Each package's hook scripts have passing unit tests in `tests/`
- [ ] `atm team init --package global` installs only the global package
- [ ] `atm team init --package rust-sprint` installs only the rust-sprint project package
- [ ] `cargo clippy --workspace -- -D warnings` clean
- [ ] `cargo test --workspace` passes

---
## 17. Future Plugins

Additional plugins planned (each is a self-contained sprint series):

| Plugin | Priority | Depends On | Notes |
|--------|----------|------------|-------|
| Human Chat Interface | Medium | Phase 5 | Slack/Discord integration |
| Beads Mail | Medium | Phase 5 | [steveyegge/beads](https://github.com/steveyegge/beads) — Gastown integration |
| MCP Agent Mail | Medium | Phase 5 | [Dicklesworthstone/mcp_agent_mail](https://github.com/Dicklesworthstone/mcp_agent_mail) — MCP interop |

---

## 18. Sprint Summary

| Phase | Sprint | Name | Status | PR |
|-------|--------|------|--------|-----|
| **1** | 1.1 | Workspace + Schema Types | ✅ | [#3](https://github.com/randlee/agent-team-mail/pull/3) |
| **1** | 1.2 | Schema Version Detection | ✅ | [#5](https://github.com/randlee/agent-team-mail/pull/5) |
| **1** | 1.3 | Atomic File I/O | ✅ | [#7](https://github.com/randlee/agent-team-mail/pull/7) |
| **1** | 1.4 | Outbound Spool | ✅ | [#8](https://github.com/randlee/agent-team-mail/pull/8) |
| **1** | 1.5 | System Context + Config | ✅ | [#6](https://github.com/randlee/agent-team-mail/pull/6) |
| **2** | 2.1 | CLI Skeleton + Send | ✅ | [#10](https://github.com/randlee/agent-team-mail/pull/10) |
| **2** | 2.2 | Read + Inbox | ✅ | [#11](https://github.com/randlee/agent-team-mail/pull/11) |
| **2** | 2.3 | Broadcast | ✅ | [#12](https://github.com/randlee/agent-team-mail/pull/12) |
| **2** | 2.4 | Discovery Commands | ✅ | [#13](https://github.com/randlee/agent-team-mail/pull/13) |
| **3** | 3.0 | Design Review Fixes | ✅ | [#15](https://github.com/randlee/agent-team-mail/pull/15) |
| **3** | 3.1 | E2E Integration Tests | ✅ | [#16](https://github.com/randlee/agent-team-mail/pull/16) |
| **3** | 3.2 | Conflict & Edge Cases | ✅ | [#17](https://github.com/randlee/agent-team-mail/pull/17) |
| **3** | 3.3 | Docs & Polish | ✅ | [#18](https://github.com/randlee/agent-team-mail/pull/18) |
| **3** | 3.4 | Inbox Retention & Cleanup | ✅ | [#19](https://github.com/randlee/agent-team-mail/pull/19) |
| **4** | 4.1 | Plugin Trait + Registry | ✅ | [#22](https://github.com/randlee/agent-team-mail/pull/22) |
| **4** | 4.2 | Daemon Event Loop | ✅ | [#24](https://github.com/randlee/agent-team-mail/pull/24) |
| **4** | 4.3 | Roster Service | ✅ | [#23](https://github.com/randlee/agent-team-mail/pull/23) |
| **4** | 4.4 | Arch Gap Hotfix (ARCH-CTM) | ✅ | [#26](https://github.com/randlee/agent-team-mail/pull/26) |
| **5** | 5.1 | Provider Abstraction | ✅ | [#27](https://github.com/randlee/agent-team-mail/pull/27) |
| **5** | 5.2 | Issues Plugin Core | ✅ | [#28](https://github.com/randlee/agent-team-mail/pull/28) |
| **5** | 5.3 | Issues Plugin Testing | ✅ | [#29](https://github.com/randlee/agent-team-mail/pull/29) |
| **5** | 5.4 | Pluggable Provider Architecture | ✅ | [#31](https://github.com/randlee/agent-team-mail/pull/31) |
| **5** | 5.5 | ARCH-CTM Review Fixes | ✅ | [#32](https://github.com/randlee/agent-team-mail/pull/32), [#33](https://github.com/randlee/agent-team-mail/pull/33) |
| **6** | 6.1 | CI Provider Abstraction | ✅ | [#35](https://github.com/randlee/agent-team-mail/pull/35) |
| **6** | 6.2 | CI Monitor Plugin Core | ✅ | [#36](https://github.com/randlee/agent-team-mail/pull/36) |
| **6** | 6.3 | CI Monitor Testing + Azure External | ✅ | [#37](https://github.com/randlee/agent-team-mail/pull/37) |
| **6.4** | — | Design Reconciliation | ✅ | [#40](https://github.com/randlee/agent-team-mail/pull/40) |
| **7** | 7.1–7.4 | Worker Adapter + Integration Tests | ✅ | [#44](https://github.com/randlee/agent-team-mail/pull/44), [#49](https://github.com/randlee/agent-team-mail/pull/49) |
| **7** | 7.5 | Phase 7 Review + Phase 8 Bridge Design | ✅ | [#52](https://github.com/randlee/agent-team-mail/pull/52) |
| **8** | 8.1 | Bridge Config + Plugin Scaffold | ✅ | [#54](https://github.com/randlee/agent-team-mail/pull/54) |
| **8** | 8.2 | Per-Origin Read Path + Watcher Fix | ✅ | [#55](https://github.com/randlee/agent-team-mail/pull/55) |
| **8** | 8.3 | SSH/SFTP Transport | ✅ | [#56](https://github.com/randlee/agent-team-mail/pull/56) |
| **8** | 8.4 | Sync Engine + Dedup | ✅ | [#57](https://github.com/randlee/agent-team-mail/pull/57) |
| **8** | 8.5 | Team Config Sync + Hardening | ✅ | [#58](https://github.com/randlee/agent-team-mail/pull/58) |
| **8** | 8.5.1 | Phase 8 Arch Review Fixes | ✅ | [#60](https://github.com/randlee/agent-team-mail/pull/60) |
| **8** | 8.6 | Bridge Hardening + Blocking Read | ✅ | [#61](https://github.com/randlee/agent-team-mail/pull/61) |

| **10** | 10.1 | Agent State Machine | ✅ | [#85](https://github.com/randlee/agent-team-mail/pull/85) |
| **10** | 10.2 | Nudge Engine | ✅ | [#86](https://github.com/randlee/agent-team-mail/pull/86) |
| **10** | 10.3 | Unix Socket IPC | ✅ | [#87](https://github.com/randlee/agent-team-mail/pull/87) |
| **10** | 10.4 | Pub/Sub Events | ✅ | [#88](https://github.com/randlee/agent-team-mail/pull/88) |
| **10** | 10.5 | Output Tailing | ✅ | [#89](https://github.com/randlee/agent-team-mail/pull/89) |
| **10** | 10.6 | Agent Launcher | ✅ | [#90](https://github.com/randlee/agent-team-mail/pull/90) |
| **10** | 10.7 | Identity Aliases + Integration | ✅ | [#91](https://github.com/randlee/agent-team-mail/pull/91) |
| **10** | 10.8 | CI Monitor Agent | ✅ | [#92](https://github.com/randlee/agent-team-mail/pull/92) |
| **A** | A.1 | Crate scaffold + config | ✅ | [#100](https://github.com/randlee/agent-team-mail/pull/100) |
| **A** | A.2 | MCP stdio proxy core | ✅ | [#101](https://github.com/randlee/agent-team-mail/pull/101) |
| **A** | A.3 | Identity binding + context injection | ✅ | [#102](https://github.com/randlee/agent-team-mail/pull/102) |
| **A** | A.4 | ATM communication tools | ✅ | [#105](https://github.com/randlee/agent-team-mail/pull/105), [#106](https://github.com/randlee/agent-team-mail/pull/106) |
| **A** | A.5 | Session registry + persistence | ✅ | [#107](https://github.com/randlee/agent-team-mail/pull/107) |
| **A** | A.6 | Thread lifecycle state machine | ✅ | [#108](https://github.com/randlee/agent-team-mail/pull/108) |
| **A** | A.7 | Auto mail injection + polling | ✅ | [#109](https://github.com/randlee/agent-team-mail/pull/109) |
| **A** | A.8 | Shutdown + resume + arch review | ✅ | [#110](https://github.com/randlee/agent-team-mail/pull/110), [#111](https://github.com/randlee/agent-team-mail/pull/111) |
| **B** | B.1 | Teams daemon session tracking + resume | ✅ | [#119](https://github.com/randlee/agent-team-mail/pull/119) |
| **B** | B.2 | Unicode-safe message truncation | ✅ | [#120](https://github.com/randlee/agent-team-mail/pull/120) |
| **B** | B.3 | Teams session stabilization | ✅ | [#122](https://github.com/randlee/agent-team-mail/pull/122) |
| **C** | C.1 | Unified structured JSONL logging | ✅ | [#125](https://github.com/randlee/agent-team-mail/pull/125), [#128](https://github.com/randlee/agent-team-mail/pull/128) |
| **C** | C.2a | Transport trait + McpTransport refactor | ✅ | [#127](https://github.com/randlee/agent-team-mail/pull/127) |
| **C** | C.2b | JsonTransport + stdin queue + integration tests | 🔄 | — |
| **C** | C.3 | Control receiver stub (daemon endpoint + dedupe) | ⏳ | — |
| **D** | D.1 | TUI crate + live stream view (read-only) | ⏳ | — |
| **D** | D.2 | Interactive controls (stdin inject, interrupt) | ⏳ | — |
| **D** | D.3 | Identifier cleanup + user demo | ⏳ | — |

**Completed**: 65 sprints across 13 phases (CI green)
**Current version**: v0.13.0
**Next**: C.2b QA → merge → C.3 → integrate/phase-C → develop; then Phase D (D.1 ∥ D.2)

**Sprint PRs (Phase 9)**:
| Sprint | PR | Description |
|--------|----|-------------|
| 9.1 | [#63](https://github.com/randlee/agent-team-mail/pull/63) | Separate clippy CI job |
| 9.2 | [#67](https://github.com/randlee/agent-team-mail/pull/67) | Canonical `get_home_dir()` replacing 11 call sites |
| 9.3 | [#71](https://github.com/randlee/agent-team-mail/pull/71) | Branch glob matching and notify_target routing |
| 9.4 | [#73](https://github.com/randlee/agent-team-mail/pull/73) | Daemon status file and CLI subcommand |
| 9.5 | [#69](https://github.com/randlee/agent-team-mail/pull/69) | Typed payload for WorkerHandle |
| 9.6 | [#70](https://github.com/randlee/agent-team-mail/pull/70) | Periodic inbox trimming and CI report retention |
| Review | [#72](https://github.com/randlee/agent-team-mail/pull/72), [#74](https://github.com/randlee/agent-team-mail/pull/74), [#77](https://github.com/randlee/agent-team-mail/pull/77), [#78](https://github.com/randlee/agent-team-mail/pull/78) | ARCH-CTM review fixes |

**Phase integration PRs**:
| Phase | Integration PR | Status |
|-------|---------------|--------|
| Phase 3 | [#20](https://github.com/randlee/agent-team-mail/pull/20) | ✅ Merged |
| Phase 4 | [#25](https://github.com/randlee/agent-team-mail/pull/25) | ✅ Merged |
| Phase 5 | [#30](https://github.com/randlee/agent-team-mail/pull/30), [#33](https://github.com/randlee/agent-team-mail/pull/33) | ✅ Merged |
| Phase 7 | [#50](https://github.com/randlee/agent-team-mail/pull/50), [#51](https://github.com/randlee/agent-team-mail/pull/51) | ✅ Merged |
| Phase 9 | [#75](https://github.com/randlee/agent-team-mail/pull/75) | ✅ Merged |
| Phase 8 | [#59](https://github.com/randlee/agent-team-mail/pull/59) | ✅ Merged |
| Phase 10 | [#93](https://github.com/randlee/agent-team-mail/pull/93) | ✅ Merged |
| Phase A | [#103](https://github.com/randlee/agent-team-mail/pull/103) | ✅ Merged |
| Phase B | [#121](https://github.com/randlee/agent-team-mail/pull/121) | ✅ Merged |
| Phase C | TBD | 🔄 IN PROGRESS |

---

## 15. Scrum Master Agent Prompt

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

**Document Version**: 0.3
**Last Updated**: 2026-02-19
**Maintained By**: Claude (ARCH-ATM)
