# agent-team-mail (`atm`) — Project Plan

**Version**: 0.4
**Date**: 2026-02-23
**Status**: Phase M complete (v0.17.0). Phase G released (v0.16.0).

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

| Role | Model | Rationale |
|------|-------|-----------|
| Scrum Master | Sonnet (Opus for escalation) | Coordination, review, process |
| Rust Dev | Sonnet | Implementation, test writing |
| Rust QA | Sonnet | Code review, test validation |
| Rust Architect | Opus | Complex architecture decisions, escalation review |

### 1.2 Dev-QA Loop

Sprint cycle: Scrum Master reviews plan → Dev implements + writes tests → QA reviews + validates → If pass: commit/push/PR → If fail: Dev fixes → back to QA.

**QA checks**: Code review, unit test coverage, 100% `cargo test`, clippy clean, Pragmatic Rust Guidelines, CI matrix (macOS/Linux/Windows).

**Escalation**: QA failures → Dev fixes → Significant issues → Opus Architect review → Human escalation if needed.

### 1.3 Worktree Isolation

All sprint work MUST use dedicated worktrees via `sc-git-worktree` skill. Main repo stays on develop.

---

## 2. Phase Overview

| Phase | Name | Goal | Status |
|-------|------|------|--------|
| 1 | Foundation (`atm-core`) | Schema types, file I/O, atomic swap, config | COMPLETE |
| 2 | CLI (`atm`) | Command structure, messaging, discovery | COMPLETE |
| 3 | Integration & Hardening | E2E tests, conflict scenarios, polish | COMPLETE |
| 4 | Daemon Foundation (`atm-daemon`) | Plugin trait, registry, daemon loop | COMPLETE |
| 5 | First Plugin (Issues) | Provider abstraction, pluggable architecture | COMPLETE |
| 6 | CI Monitor Plugin | GitHub Actions built-in + Azure external | COMPLETE |
| 6.4 | Design Reconciliation | Multi-repo daemon model, root vs repo | COMPLETE |
| 7 | Async Agent Worker Adapter | TMUX-based Codex worker adapter | COMPLETE |
| 8 | Cross-Computer Bridge | SSH/SFTP bidirectional inbox sync | COMPLETE |
| 9 | CI Monitor Integration + Platform | CI/tooling, home dir, daemon ops | COMPLETE |
| 10 | Codex Orchestration | Agent state machine, nudge, IPC, launcher | COMPLETE |
| A | atm-agent-mcp | MCP stdio proxy for Codex | COMPLETE |
| B | Team-Lead Session Management | Unicode, cleanup, session tracking | COMPLETE |
| C | Observability + Codex JSON Mode | Unified logging, transport trait, JSON mode | COMPLETE |
| D | TUI Streaming | Real-time TUI for agent sessions | COMPLETE |
| E | ATM Core Bug Fixes | Resume fix, read scoping, hooks, TUI hardening | COMPLETE |
| F | Team Installer | `atm team init` package installer | PLANNED |
| G | Codex Multi-Transport Hardening | App-server, unified turns, mail injection parity | COMPLETE |
| L | Logging Overhaul | Daemon fan-in architecture, unified JSONL writer | COMPLETE |

---

## 3. Phase 1: Foundation (`atm-core`) — COMPLETE

**Branch prefix**: `feature/p1-*`

| Sprint | Name | PR | Branch |
|--------|------|----|--------|
| 1.1 | Workspace + Schema Types | [#3](https://github.com/randlee/agent-team-mail/pull/3) | `feature/p1-s1-workspace-schema` |
| 1.2 | Schema Version Detection | [#5](https://github.com/randlee/agent-team-mail/pull/5) | `feature/p1-s2-schema-version` |
| 1.3 | Atomic File I/O | [#7](https://github.com/randlee/agent-team-mail/pull/7) | `feature/p1-s3-atomic-io` |
| 1.4 | Outbound Spool + Guaranteed Delivery | [#8](https://github.com/randlee/agent-team-mail/pull/8) | `feature/p1-s4-spool` |
| 1.5 | System Context + Config | [#6](https://github.com/randlee/agent-team-mail/pull/6) | `feature/p1-s5-context-config` |

**Dependency graph**: 1.1 → {1.2, 1.3, 1.5} parallel; 1.3 → 1.4

---

## 4. Phase 2: CLI (`atm`) — COMPLETE

**Branch prefix**: `feature/p2-*`

| Sprint | Name | PR | Branch |
|--------|------|----|--------|
| 2.1 | CLI Skeleton + Send Command | [#10](https://github.com/randlee/agent-team-mail/pull/10) | `feature/p2-s1-cli-send` |
| 2.2 | Read + Inbox Commands | [#11](https://github.com/randlee/agent-team-mail/pull/11) | `feature/p2-s2-read-inbox` |
| 2.3 | Broadcast Command | [#12](https://github.com/randlee/agent-team-mail/pull/12) | `feature/p2-s3-broadcast` |
| 2.4 | Discovery Commands | [#13](https://github.com/randlee/agent-team-mail/pull/13) | `feature/p2-s4-discovery` |

**Dependency graph**: 2.1 → {2.2, 2.3, 2.4} parallel

---

## 5. Phase 3: Integration & Hardening — COMPLETE

**Branch prefix**: `feature/p3-*` | **Integration branch**: `integrate/phase-3` | **Integration PR**: [#20](https://github.com/randlee/agent-team-mail/pull/20)

| Sprint | Name | PR | Branch |
|--------|------|----|--------|
| 3.0 | ARCH-CTM Design Review Fixes (Hotfix) | [#15](https://github.com/randlee/agent-team-mail/pull/15) | `feature/p3-s0-design-fixes` |
| 3.1 | E2E Integration Tests | [#16](https://github.com/randlee/agent-team-mail/pull/16) | `feature/p3-s1-e2e-tests` |
| 3.2 | Conflict & Edge Case Testing | [#17](https://github.com/randlee/agent-team-mail/pull/17) | `feature/p3-s2-conflict-tests` |
| 3.3 | Documentation & Polish | [#18](https://github.com/randlee/agent-team-mail/pull/18) | `feature/p3-s3-docs-polish` |
| 3.4 | Inbox Retention and Cleanup | [#19](https://github.com/randlee/agent-team-mail/pull/19) | `feature/p3-s4-retention` |

**Dependency graph**: 3.0 → 3.1 → {3.2, 3.3, 3.4} parallel

**Deferred**: Managed settings policy paths, destination repo file policy full resolution, Windows atomic swap fsync.

---

## 6. Phase 4: Daemon Foundation (`atm-daemon`) — COMPLETE

**Branch prefix**: `feature/p4-*` | **Integration PR**: [#25](https://github.com/randlee/agent-team-mail/pull/25)

| Sprint | Name | PR | Branch |
|--------|------|----|--------|
| 4.1 | Plugin Trait + Registry | [#22](https://github.com/randlee/agent-team-mail/pull/22) | `feature/p4-s1-plugin-trait` |
| 4.2 | Daemon Event Loop | [#24](https://github.com/randlee/agent-team-mail/pull/24) | `feature/p4-s2-daemon-loop` |
| 4.3 | Roster Service | [#23](https://github.com/randlee/agent-team-mail/pull/23) | `feature/p4-s3-roster` |
| 4.4 | Architecture Gap Hotfix (ARCH-CTM) | [#26](https://github.com/randlee/agent-team-mail/pull/26) | `feature/p4-hotfix-arch-gaps` |

**Dependency graph**: 4.1 → {4.2, 4.3} parallel → 4.4

**Deferred**: Managed settings policy, destination-repo file policy, SchemaVersion wiring, inventory-based registration, plugin temp_dir.

---

## 7. Phase 5: First Plugin (Issues) — COMPLETE

**Branch prefix**: `feature/p5-*` | **Integration PR**: [#30](https://github.com/randlee/agent-team-mail/pull/30), [#33](https://github.com/randlee/agent-team-mail/pull/33)

| Sprint | Name | PR | Branch |
|--------|------|----|--------|
| 5.1 | Provider Abstraction | [#27](https://github.com/randlee/agent-team-mail/pull/27) | `feature/p5-s1-provider-abstraction` |
| 5.2 | Issues Plugin Core | [#28](https://github.com/randlee/agent-team-mail/pull/28) | `feature/p5-s2-issues-plugin` |
| 5.3 | Issues Plugin Testing | [#29](https://github.com/randlee/agent-team-mail/pull/29) | `feature/p5-s3-issues-tests` |
| 5.4 | Pluggable Provider Architecture | [#31](https://github.com/randlee/agent-team-mail/pull/31) | `feature/p5-s4-pluggable-providers` |
| 5.5 | ARCH-CTM Review Fixes | [#32](https://github.com/randlee/agent-team-mail/pull/32), [#33](https://github.com/randlee/agent-team-mail/pull/33) | `review/arch-ctm-phase-5` |

**Dependency graph**: 5.1 → 5.2 → 5.3 → 5.4 → 5.5 (sequential)

---

## 8. Phase 6: CI Monitor Plugin — COMPLETE

**Branch prefix**: `feature/p6-*`

| Sprint | Name | PR | Branch |
|--------|------|----|--------|
| 6.1 | CI Provider Abstraction | [#35](https://github.com/randlee/agent-team-mail/pull/35) | `feature/p6-s1-ci-provider` |
| 6.2 | CI Monitor Plugin Core | [#36](https://github.com/randlee/agent-team-mail/pull/36) | `feature/p6-s2-ci-monitor-plugin` |
| 6.3 | CI Monitor Testing + Azure External | [#37](https://github.com/randlee/agent-team-mail/pull/37) | `feature/p6-s3-ci-monitor-tests` |

**Dependency graph**: 6.1 → 6.2 → 6.3 (sequential)

### Phase 6.4: Design Reconciliation — COMPLETE

**PR**: [#40](https://github.com/randlee/agent-team-mail/pull/40). Updated requirements for multi-repo daemon model, root vs repo distinction, subscription schema, config tiers, branch filter syntax.

---

## 9. Phase 7: Async Agent Worker Adapter — COMPLETE

**Branch prefix**: `feature/p7-*`
**Design reference**: `docs/codex-tmux-adapter.md`

| Sprint | Name | PR | Branch |
|--------|------|----|--------|
| 7.1 | Worker Adapter Trait + Codex Backend | [#44](https://github.com/randlee/agent-team-mail/pull/44) | `feature/p7-s1-worker-adapter` |
| 7.2 | Message Routing + Response Capture + Activity Tracking | [#44](https://github.com/randlee/agent-team-mail/pull/44) | `feature/p7-s2-message-routing` |
| 7.3 | Worker Lifecycle + Health Monitoring | [#49](https://github.com/randlee/agent-team-mail/pull/49) | `feature/p7-s3-worker-lifecycle` |
| 7.4 | Integration Testing + Config Validation | [#49](https://github.com/randlee/agent-team-mail/pull/49) | `feature/p7-s4-integration-tests` |
| 7.5 | Phase 7 Review + Phase 8 Bridge Design | [#52](https://github.com/randlee/agent-team-mail/pull/52) | `planning/phase-7-review` |

**Integration PRs**: [#50](https://github.com/randlee/agent-team-mail/pull/50), [#51](https://github.com/randlee/agent-team-mail/pull/51)
**Dependency graph**: 7.1 → 7.2 → 7.3 → 7.4 → 7.5 (sequential)

**Deferred**: WorkerHandle tmux-specific refactor, parent directory fsync, retention wired into daemon.

---

## 10. Phase 8: Cross-Computer Bridge Plugin — COMPLETE

**Branch prefix**: `feature/p8-*` | **Integration PR**: [#59](https://github.com/randlee/agent-team-mail/pull/59)
**Design reference**: `docs/phase8-bridge-design.md`

**Key decisions**: Local `<agent>.json` NEVER modified; remote origin files: `<agent>.<hostname>.json`; hub-spoke topology; SSH/SFTP transport; atomic temp+rename writes.

| Sprint | Name | PR | Branch |
|--------|------|----|--------|
| 8.1 | Bridge Config + Plugin Scaffold | [#54](https://github.com/randlee/agent-team-mail/pull/54) | `feature/p8-s1-bridge-config` |
| 8.2 | Per-Origin Read Path + Watcher Fix | [#55](https://github.com/randlee/agent-team-mail/pull/55) | `feature/p8-s2-read-path` |
| 8.3 | SSH/SFTP Transport | [#56](https://github.com/randlee/agent-team-mail/pull/56) | `feature/p8-s3-ssh-transport` |
| 8.4 | Sync Engine + Dedup | [#57](https://github.com/randlee/agent-team-mail/pull/57) | `feature/p8-s4-sync-engine` |
| 8.5 | Team Config Sync + Hardening | [#58](https://github.com/randlee/agent-team-mail/pull/58) | `feature/p8-s5-hardening` |
| 8.5.1 | Phase 8 Arch Review Fixes | [#60](https://github.com/randlee/agent-team-mail/pull/60) | — |
| 8.6 | Bridge Hardening + Blocking Read | [#61](https://github.com/randlee/agent-team-mail/pull/61) | `feature/p8-s6-hardening` |

**Dependency graph**: 8.1 → {8.2, 8.3} parallel → 8.4 → 8.5 → 8.6

---

## 11. Phase 9: CI Monitor Integration + Platform Stabilization — COMPLETE (v0.9.0)

**Integration PR**: [#75](https://github.com/randlee/agent-team-mail/pull/75)

| Sprint | Name | PR | Branch |
|--------|------|----|--------|
| 9.0 | Phase 8.6 Verification Gate | (gate check) | — |
| 9.1 | CI/Tooling Stabilization | [#63](https://github.com/randlee/agent-team-mail/pull/63) | — |
| 9.2 | Home Dir Resolution | [#67](https://github.com/randlee/agent-team-mail/pull/67) | — |
| 9.3 | CI Config & Routing | [#71](https://github.com/randlee/agent-team-mail/pull/71) | — |
| 9.4 | Daemon Operationalization | [#73](https://github.com/randlee/agent-team-mail/pull/73) | — |
| 9.5 | WorkerHandle Backend Payload | [#69](https://github.com/randlee/agent-team-mail/pull/69) | — |
| 9.6 | Daemon Retention Tasks | [#70](https://github.com/randlee/agent-team-mail/pull/70) | — |
| Review | ARCH-CTM review fixes | [#72](https://github.com/randlee/agent-team-mail/pull/72), [#74](https://github.com/randlee/agent-team-mail/pull/74), [#77](https://github.com/randlee/agent-team-mail/pull/77), [#78](https://github.com/randlee/agent-team-mail/pull/78) | — |

**Dependency graph**: 9.0 → 9.1 → 9.2 → {9.3, 9.5} parallel; 9.3 → 9.4 → 9.6

---

## 11.5. Phase 10: Codex Orchestration — COMPLETE

**Integration PR**: [#93](https://github.com/randlee/agent-team-mail/pull/93)

| Sprint | Name | PR |
|--------|------|----|
| 10.1 | Agent State Machine | [#85](https://github.com/randlee/agent-team-mail/pull/85) |
| 10.2 | Nudge Engine | [#86](https://github.com/randlee/agent-team-mail/pull/86) |
| 10.3 | Unix Socket IPC | [#87](https://github.com/randlee/agent-team-mail/pull/87) |
| 10.4 | Pub/Sub Events | [#88](https://github.com/randlee/agent-team-mail/pull/88) |
| 10.5 | Output Tailing | [#89](https://github.com/randlee/agent-team-mail/pull/89) |
| 10.6 | Agent Launcher | [#90](https://github.com/randlee/agent-team-mail/pull/90) |
| 10.7 | Identity Aliases + Integration | [#91](https://github.com/randlee/agent-team-mail/pull/91) |
| 10.8 | CI Monitor Agent | [#92](https://github.com/randlee/agent-team-mail/pull/92) |

---

## 12. Phase A: atm-agent-mcp — COMPLETE

**Integration PR**: [#103](https://github.com/randlee/agent-team-mail/pull/103)
**Crate**: `crates/atm-agent-mcp` (binary: `atm-agent-mcp`)
**Requirements**: `docs/atm-agent-mcp/requirements.md` | **Design**: `docs/atm-agent-mcp/codex-mcp-crate-design.md`

| Sprint | Name | PR |
|--------|------|----|
| A.1 | Crate scaffold + config (FR-12, FR-13) | [#100](https://github.com/randlee/agent-team-mail/pull/100) |
| A.2 | MCP stdio proxy core (FR-1, FR-11, FR-14, FR-15, FR-19) | [#101](https://github.com/randlee/agent-team-mail/pull/101) |
| A.3 | Identity binding + context injection (FR-2, FR-3, FR-16, FR-20.1) | [#102](https://github.com/randlee/agent-team-mail/pull/102) |
| A.4 | ATM communication tools (FR-4, FR-20.4-20.5) | [#105](https://github.com/randlee/agent-team-mail/pull/105), [#106](https://github.com/randlee/agent-team-mail/pull/106) |
| A.5 | Session registry + persistence (FR-5, FR-10, FR-20.2-20.3) | [#107](https://github.com/randlee/agent-team-mail/pull/107) |
| A.6 | Thread lifecycle state machine (FR-17, FR-18) | [#108](https://github.com/randlee/agent-team-mail/pull/108) |
| A.7 | Auto mail injection + polling (FR-8) | [#109](https://github.com/randlee/agent-team-mail/pull/109) |
| A.8 | Shutdown + resume + arch review (FR-6, FR-7, FR-9) | [#110](https://github.com/randlee/agent-team-mail/pull/110), [#111](https://github.com/randlee/agent-team-mail/pull/111) |

---

## 13. Phase B: Team-Lead Session Management — COMPLETE

**Integration PR**: [#121](https://github.com/randlee/agent-team-mail/pull/121)

| Sprint | Name | PR |
|--------|------|----|
| B.1 | Daemon session tracking + `atm teams resume` + `atm teams cleanup` | [#119](https://github.com/randlee/agent-team-mail/pull/119) |
| B.2 | Unicode-safe message truncation + input validation | [#120](https://github.com/randlee/agent-team-mail/pull/120) |
| B.3 | Cleanup safety hardening + documentation alignment | [#122](https://github.com/randlee/agent-team-mail/pull/122) |

---

## 14. Phase C: Observability + Codex JSON Mode — COMPLETE

**Integration PR**: [#126](https://github.com/randlee/agent-team-mail/pull/126)
**Mode terminology**: `transport = "mcp" | "cli-json" | "app-server"`

| Sprint | Name | PR |
|--------|------|----|
| C.1 | Unified logging infrastructure (`tracing` + JSONL) | [#125](https://github.com/randlee/agent-team-mail/pull/125), [#128](https://github.com/randlee/agent-team-mail/pull/128) |
| C.2a | Transport trait + McpTransport refactor | [#127](https://github.com/randlee/agent-team-mail/pull/127) |
| C.2b | CliJsonTransport + stdin queue + integration tests | [#127](https://github.com/randlee/agent-team-mail/pull/127) |
| C.3 | Control receiver stub (daemon endpoint + dedupe) | [#126](https://github.com/randlee/agent-team-mail/pull/126) |

---

## 15. Phase D: TUI Streaming — COMPLETE

**Integration PR**: [#140](https://github.com/randlee/agent-team-mail/pull/140)
**Design refs**: `docs/tui-mvp-architecture.md`, `docs/tui-control-protocol.md`

| Sprint | Name | PR |
|--------|------|----|
| D.1 | TUI crate + live stream view (read-only) | [#134](https://github.com/randlee/agent-team-mail/pull/134) |
| D.2 | Interactive controls (stdin inject, interrupt) | [#138](https://github.com/randlee/agent-team-mail/pull/138) |
| D.3 | Identifier cleanup + user demo | [#140](https://github.com/randlee/agent-team-mail/pull/140) |

---

## 16. Phase E: ATM Core Bug Fixes — COMPLETE (v0.15.0)

**Integration PR**: TBD (pending)

| Sprint | Name | PR |
|--------|------|----|
| E.1 | `atm teams resume` session ID reliability | [#147](https://github.com/randlee/agent-team-mail/pull/147) |
| E.2 | Inbox read scoping (fix cross-agent mark-as-read) | [#149](https://github.com/randlee/agent-team-mail/pull/149) |
| E.3 | Hook-to-daemon state bridge | [#152](https://github.com/randlee/agent-team-mail/pull/152) |
| E.4 | TUI reliability hardening (restart, reconnect, failure injection) | [#158](https://github.com/randlee/agent-team-mail/pull/158) |
| E.5 | TUI performance, UX polish, operational validation | [#161](https://github.com/randlee/agent-team-mail/pull/161) |
| E.6 | External agent member management + model registry | [#164](https://github.com/randlee/agent-team-mail/pull/164) |
| E.7 | Unified lifecycle source model + MCP lifecycle emission | [#165](https://github.com/randlee/agent-team-mail/pull/165) |
| E.8 | ATM Identity Role Mapping + Team Backup/Restore | [#162](https://github.com/randlee/agent-team-mail/pull/162) |
| — | Daemon hook-event auth validation | [#163](https://github.com/randlee/agent-team-mail/pull/163) |

**Dependency graph**: E.1 → {E.2, E.3} parallel; E.3 → {E.4, E.6} parallel; E.4 → E.5; E.6 → E.7; E.1 → E.8

---

## 16.5 Phase F: Team Installer (`atm team init`) — PLANNED

**Goal**: Install orchestration packages (hooks, agents, skills) into `~/.claude/` with `atm team init`.

**Two install scopes**:
1. **Global** (machine-level): Hook scripts (`session-start.py`, `session-end.py`) + `~/.claude/settings.json` entries. Installed once per machine.
2. **Project** (per repo/workflow): Gate hooks, agent prompts, skills → `.claude/` directory. Multiple named packages composable.

**Package format**: `.claude/packages/<name>/manifest.toml` with scripts, agents, skills, hooks sections.

**`settings.json` surgery**: Insert/remove only, never wholesale rewrite. Preserves existing entries. Atomic writes.

| Sprint | Name | Depends On | Status |
|--------|------|------------|--------|
| F.1 | Package format + `atm team init` (global + project scopes) | Phase E | PLANNED |
| F.2 | `atm team uninstall` + receipt tracking | F.1 | PLANNED |
| F.3 | Built-in packages: `global`, `rust-sprint`, `generic-dev` | F.1 | PLANNED |

**Execution model**: F.1 is MVP. F.2 and F.3 parallel after F.1.

**Onboarding design questions** (resolve in F.1):
- Nudge when `.claude/` exists but no `.atm.toml` (compromise: only in Claude Code projects)
- Opt-out: env var `ATM_QUIET=1` → project `.atm.toml disabled=true` → global config
- Global hooks must check `.atm.toml` as first operation (no I/O before guard)

---

## 16.6 Phase G: Codex Multi-Transport Runtime Hardening — COMPLETE (v0.16.0)

**Goal**: Stabilize all three `atm-agent-mcp` execution modes (`mcp`, `cli-json`, `app-server`) with unified lifecycle, mail injection, and TUI streaming.

**Design refs**: `docs/atm-agent-mcp/requirements.md`, `codex-execution-modes.md`, `app-server-protocol-reference.md`, `tui-control-protocol.md`

| Sprint | Name | PR |
|--------|------|----|
| G.1 | Mode baseline docs + naming cleanup (`json` -> `cli-json`) | [#168](https://github.com/randlee/agent-team-mail/pull/168) |
| G.2 | CLI-JSON streaming verification + idle detection hardening | [#175](https://github.com/randlee/agent-team-mail/pull/175) |
| G.3 | App-server transport adapter (`CodexTransport` impl) | [#170](https://github.com/randlee/agent-team-mail/pull/170) |
| G.4 | Unified turn control + daemon turn-state reporting | [#171](https://github.com/randlee/agent-team-mail/pull/171) |
| G.5 | Approval/elicitation bridging parity (app-server) | [#172](https://github.com/randlee/agent-team-mail/pull/172) |
| G.6 | Mail injection parity + queue semantics | [#173](https://github.com/randlee/agent-team-mail/pull/173) |
| G.7 | TUI streaming normalization + daemon pubsub/UDP fanout | [#174](https://github.com/randlee/agent-team-mail/pull/174), [#176](https://github.com/randlee/agent-team-mail/pull/176) |
| G.8 | Cross-platform reliability + soak testing | [#177](https://github.com/randlee/agent-team-mail/pull/177) |
| G.9 | Docs finalization + release gate | PLANNED |

**Dependency graph**: G.1 → G.3 → G.4 → {G.5, G.6} parallel; G.4 + G.6 → G.7; G.7 → G.2; G.5 + G.6 + G.7 → G.8; G.2 + G.8 → G.9

**TUI transport notes**: MCP emits TurnIdle only (no TurnStarted/TurnCompleted); cli-json has no explicit turn-start notification. Both transports will not show [BUSY] badge in TUI.

---

## 17. Phase L: Logging Overhaul — COMPLETE

**GitHub Issue**: [#188](https://github.com/randlee/agent-team-mail/issues/188)
**Goal**: Daemon fan-in architecture — all binaries emit to daemon socket, single JSONL writer.

**Design**: All ATM binaries (`atm`, `atm-daemon`, `atm-agent-mcp`, `atm-tui`) send log events to daemon Unix socket. Daemon is the sole JSONL file writer. Eliminates file contention, enables centralized log management.

| Sprint | Name | Depends On | Status |
|--------|------|------------|--------|
| L.1a | Sink architecture + API structs (LogEventV1) | — | COMPLETE |
| L.1b | `init_unified` + bridge to daemon socket | L.1a | COMPLETE |
| L.2 | Coverage — instrument all crates | L.1b | COMPLETE |
| L.3 | `atm logs` CLI command | L.2 | COMPLETE |
| L.4 | TUI log viewer + legacy sunset | L.3 | COMPLETE |
| L.5 | Direct watch stream + daemon boundary hardening (L.5a-L.5d) | L.4 | COMPLETE ([#201](https://github.com/randlee/agent-team-mail/pull/201)) |

**Deferred (explicit)**: Dashboard mail compose workflow is out of scope for current L-series work; Dashboard remains preview/navigation-only until a dedicated composer sprint is scheduled.

**Blocked by**: This is a blocking prerequisite for integration testing.

---

## 17.1 Phase M: Log & Stream Cleanup — COMPLETE

**Goal**: Close post-Phase-L logging/streaming gaps, then deliver Codex CLI look-and-feel parity in ATM watch mode.

| Sprint | Name | Depends On | Status |
|--------|------|------------|--------|
| M.1 | Watch-stream file naming/scoping cleanup | L.5 | COMPLETE |
| M.1b | Legacy bridge removal (`emit_event_best_effort` sunset) | M.1 | COMPLETE |
| M.2 | Codex watch-pane UI import baseline (copy-first) | L.5 | COMPLETE |
| M.3 | Event adapter parity (`CodexAdapter`) | M.2 | COMPLETE |
| M.4 | Input/approval/interrupt parity | M.3 | COMPLETE |
| M.5 | Session/status surface parity | M.4 | COMPLETE |
| M.6 | Replay/reconnect hardening | M.5 | COMPLETE |
| M.7 | Golden parity test harness + rollout gate | M.6 | COMPLETE |

**Parallel tracks**: M.1 and M.2 can execute concurrently (both depend only on L.5). M.1b depends on M.1 but is independent of M.2-M.7 and can run in parallel with the Codex parity track. M.3+ is sequential after M.2.

**M.1 scope**:
- Replace shared `~/.config/atm/watch-stream/events.jsonl` with per-agent or per-session files (for example `watch-stream/<agent-id>.jsonl`).
- Clarify naming semantics so watch-stream cache is not confused with canonical log/audit streams.
- Update `.claude/agents/log-monitor.md` to match final Phase M.1 log-path semantics and monitoring rules.

**M.1b scope** (legacy bridge removal):
- Remove `emit_event_best_effort` dual-write path and `ATM_LOG_BRIDGE` env var support from all crates.
- Remove legacy `events.jsonl` sink code from CLI, daemon, MCP proxy, and TUI.
- Remove legacy bridge log surface (surface 6) from `.claude/agents/log-monitor.md`.
- Update `docs/logging-l1a-spec.md` and `docs/requirements.md` to mark bridge as removed.
- Verify no external consumers depend on the old format before removal.

**M.2-M.7 scope (Codex parity)**:
- Copy Codex CLI rendering/runtime elements first and adapt only integration seams.
- Preserve daemon boundary: lifecycle/state events to daemon, continuous stream stays MCP->TUI.
- Achieve parity for core flows: prompt, tool stream, approval/reject, interrupt/cancel, errors, reconnect.
- Add golden transcript/frame parity tests as a merge gate.
- Keep ATM source attribution visible (`client_prompt`, `atm_mail`, `user_steer`) without altering Codex rendering semantics.
- Use `docs/atm-agent-mcp/codex-parity-test-plan.md` as the baseline matrix and fixture contract for M.3/M.7.

---

## 18. Future Plugins

| Plugin | Priority | Notes |
|--------|----------|-------|
| Human Chat Interface | Medium | Slack/Discord integration |
| Beads Mail | Medium | [steveyegge/beads](https://github.com/steveyegge/beads) — Gastown integration |
| MCP Agent Mail | Medium | [Dicklesworthstone/mcp_agent_mail](https://github.com/Dicklesworthstone/mcp_agent_mail) — MCP interop |

---

## 19. Sprint Summary (All Phases)


| Phase | Sprint | Name | Status | PR |
|-------|--------|------|--------|-----|
| **1** | 1.1 | Workspace + Schema Types | COMPLETE | [#3](https://github.com/randlee/agent-team-mail/pull/3) |
| **1** | 1.2 | Schema Version Detection | COMPLETE | [#5](https://github.com/randlee/agent-team-mail/pull/5) |
| **1** | 1.3 | Atomic File I/O | COMPLETE | [#7](https://github.com/randlee/agent-team-mail/pull/7) |
| **1** | 1.4 | Outbound Spool | COMPLETE | [#8](https://github.com/randlee/agent-team-mail/pull/8) |
| **1** | 1.5 | System Context + Config | COMPLETE | [#6](https://github.com/randlee/agent-team-mail/pull/6) |
| **2** | 2.1 | CLI Skeleton + Send | COMPLETE | [#10](https://github.com/randlee/agent-team-mail/pull/10) |
| **2** | 2.2 | Read + Inbox | COMPLETE | [#11](https://github.com/randlee/agent-team-mail/pull/11) |
| **2** | 2.3 | Broadcast | COMPLETE | [#12](https://github.com/randlee/agent-team-mail/pull/12) |
| **2** | 2.4 | Discovery Commands | COMPLETE | [#13](https://github.com/randlee/agent-team-mail/pull/13) |
| **3** | 3.0 | Design Review Fixes | COMPLETE | [#15](https://github.com/randlee/agent-team-mail/pull/15) |
| **3** | 3.1 | E2E Integration Tests | COMPLETE | [#16](https://github.com/randlee/agent-team-mail/pull/16) |
| **3** | 3.2 | Conflict & Edge Cases | COMPLETE | [#17](https://github.com/randlee/agent-team-mail/pull/17) |
| **3** | 3.3 | Docs & Polish | COMPLETE | [#18](https://github.com/randlee/agent-team-mail/pull/18) |
| **3** | 3.4 | Inbox Retention & Cleanup | COMPLETE | [#19](https://github.com/randlee/agent-team-mail/pull/19) |
| **4** | 4.1 | Plugin Trait + Registry | COMPLETE | [#22](https://github.com/randlee/agent-team-mail/pull/22) |
| **4** | 4.2 | Daemon Event Loop | COMPLETE | [#24](https://github.com/randlee/agent-team-mail/pull/24) |
| **4** | 4.3 | Roster Service | COMPLETE | [#23](https://github.com/randlee/agent-team-mail/pull/23) |
| **4** | 4.4 | Arch Gap Hotfix (ARCH-CTM) | COMPLETE | [#26](https://github.com/randlee/agent-team-mail/pull/26) |
| **5** | 5.1 | Provider Abstraction | COMPLETE | [#27](https://github.com/randlee/agent-team-mail/pull/27) |
| **5** | 5.2 | Issues Plugin Core | COMPLETE | [#28](https://github.com/randlee/agent-team-mail/pull/28) |
| **5** | 5.3 | Issues Plugin Testing | COMPLETE | [#29](https://github.com/randlee/agent-team-mail/pull/29) |
| **5** | 5.4 | Pluggable Provider Architecture | COMPLETE | [#31](https://github.com/randlee/agent-team-mail/pull/31) |
| **5** | 5.5 | ARCH-CTM Review Fixes | COMPLETE | [#32](https://github.com/randlee/agent-team-mail/pull/32), [#33](https://github.com/randlee/agent-team-mail/pull/33) |
| **6** | 6.1 | CI Provider Abstraction | COMPLETE | [#35](https://github.com/randlee/agent-team-mail/pull/35) |
| **6** | 6.2 | CI Monitor Plugin Core | COMPLETE | [#36](https://github.com/randlee/agent-team-mail/pull/36) |
| **6** | 6.3 | CI Monitor Testing + Azure External | COMPLETE | [#37](https://github.com/randlee/agent-team-mail/pull/37) |
| **6.4** | — | Design Reconciliation | COMPLETE | [#40](https://github.com/randlee/agent-team-mail/pull/40) |
| **7** | 7.1-7.4 | Worker Adapter + Integration Tests | COMPLETE | [#44](https://github.com/randlee/agent-team-mail/pull/44), [#49](https://github.com/randlee/agent-team-mail/pull/49) |
| **7** | 7.5 | Phase 7 Review + Phase 8 Bridge Design | COMPLETE | [#52](https://github.com/randlee/agent-team-mail/pull/52) |
| **8** | 8.1 | Bridge Config + Plugin Scaffold | COMPLETE | [#54](https://github.com/randlee/agent-team-mail/pull/54) |
| **8** | 8.2 | Per-Origin Read Path + Watcher Fix | COMPLETE | [#55](https://github.com/randlee/agent-team-mail/pull/55) |
| **8** | 8.3 | SSH/SFTP Transport | COMPLETE | [#56](https://github.com/randlee/agent-team-mail/pull/56) |
| **8** | 8.4 | Sync Engine + Dedup | COMPLETE | [#57](https://github.com/randlee/agent-team-mail/pull/57) |
| **8** | 8.5 | Team Config Sync + Hardening | COMPLETE | [#58](https://github.com/randlee/agent-team-mail/pull/58) |
| **8** | 8.5.1 | Phase 8 Arch Review Fixes | COMPLETE | [#60](https://github.com/randlee/agent-team-mail/pull/60) |
| **8** | 8.6 | Bridge Hardening + Blocking Read | COMPLETE | [#61](https://github.com/randlee/agent-team-mail/pull/61) |
| **9** | 9.0 | Phase 8.6 Verification Gate | COMPLETE | (gate) |
| **9** | 9.1 | CI/Tooling Stabilization | COMPLETE | [#63](https://github.com/randlee/agent-team-mail/pull/63) |
| **9** | 9.2 | Home Dir Resolution | COMPLETE | [#67](https://github.com/randlee/agent-team-mail/pull/67) |
| **9** | 9.3 | CI Config & Routing | COMPLETE | [#71](https://github.com/randlee/agent-team-mail/pull/71) |
| **9** | 9.4 | Daemon Operationalization | COMPLETE | [#73](https://github.com/randlee/agent-team-mail/pull/73) |
| **9** | 9.5 | WorkerHandle Backend Payload | COMPLETE | [#69](https://github.com/randlee/agent-team-mail/pull/69) |
| **9** | 9.6 | Daemon Retention Tasks | COMPLETE | [#70](https://github.com/randlee/agent-team-mail/pull/70) |
| **10** | 10.1 | Agent State Machine | COMPLETE | [#85](https://github.com/randlee/agent-team-mail/pull/85) |
| **10** | 10.2 | Nudge Engine | COMPLETE | [#86](https://github.com/randlee/agent-team-mail/pull/86) |
| **10** | 10.3 | Unix Socket IPC | COMPLETE | [#87](https://github.com/randlee/agent-team-mail/pull/87) |
| **10** | 10.4 | Pub/Sub Events | COMPLETE | [#88](https://github.com/randlee/agent-team-mail/pull/88) |
| **10** | 10.5 | Output Tailing | COMPLETE | [#89](https://github.com/randlee/agent-team-mail/pull/89) |
| **10** | 10.6 | Agent Launcher | COMPLETE | [#90](https://github.com/randlee/agent-team-mail/pull/90) |
| **10** | 10.7 | Identity Aliases + Integration | COMPLETE | [#91](https://github.com/randlee/agent-team-mail/pull/91) |
| **10** | 10.8 | CI Monitor Agent | COMPLETE | [#92](https://github.com/randlee/agent-team-mail/pull/92) |
| **A** | A.1 | Crate scaffold + config | COMPLETE | [#100](https://github.com/randlee/agent-team-mail/pull/100) |
| **A** | A.2 | MCP stdio proxy core | COMPLETE | [#101](https://github.com/randlee/agent-team-mail/pull/101) |
| **A** | A.3 | Identity binding + context injection | COMPLETE | [#102](https://github.com/randlee/agent-team-mail/pull/102) |
| **A** | A.4 | ATM communication tools | COMPLETE | [#105](https://github.com/randlee/agent-team-mail/pull/105), [#106](https://github.com/randlee/agent-team-mail/pull/106) |
| **A** | A.5 | Session registry + persistence | COMPLETE | [#107](https://github.com/randlee/agent-team-mail/pull/107) |
| **A** | A.6 | Thread lifecycle state machine | COMPLETE | [#108](https://github.com/randlee/agent-team-mail/pull/108) |
| **A** | A.7 | Auto mail injection + polling | COMPLETE | [#109](https://github.com/randlee/agent-team-mail/pull/109) |
| **A** | A.8 | Shutdown + resume + arch review | COMPLETE | [#110](https://github.com/randlee/agent-team-mail/pull/110), [#111](https://github.com/randlee/agent-team-mail/pull/111) |
| **B** | B.1 | Teams daemon session tracking + resume | COMPLETE | [#119](https://github.com/randlee/agent-team-mail/pull/119) |
| **B** | B.2 | Unicode-safe message truncation | COMPLETE | [#120](https://github.com/randlee/agent-team-mail/pull/120) |
| **B** | B.3 | Teams session stabilization | COMPLETE | [#122](https://github.com/randlee/agent-team-mail/pull/122) |
| **C** | C.1 | Unified structured JSONL logging | COMPLETE | [#125](https://github.com/randlee/agent-team-mail/pull/125), [#128](https://github.com/randlee/agent-team-mail/pull/128) |
| **C** | C.2a | Transport trait + McpTransport refactor | COMPLETE | [#127](https://github.com/randlee/agent-team-mail/pull/127) |
| **C** | C.2b | CliJsonTransport + stdin queue | COMPLETE | [#127](https://github.com/randlee/agent-team-mail/pull/127) |
| **C** | C.3 | Control receiver stub | COMPLETE | [#126](https://github.com/randlee/agent-team-mail/pull/126) |
| **D** | D.1 | TUI crate + live stream view | COMPLETE | [#134](https://github.com/randlee/agent-team-mail/pull/134) |
| **D** | D.2 | Interactive controls | COMPLETE | [#138](https://github.com/randlee/agent-team-mail/pull/138) |
| **D** | D.3 | Identifier cleanup + user demo | COMPLETE | [#140](https://github.com/randlee/agent-team-mail/pull/140) |
| **E** | E.1 | `atm teams resume` session ID fix | COMPLETE | [#147](https://github.com/randlee/agent-team-mail/pull/147) |
| **E** | E.2 | Inbox read scoping | COMPLETE | [#149](https://github.com/randlee/agent-team-mail/pull/149) |
| **E** | E.3 | Hook-to-daemon state bridge | COMPLETE | [#152](https://github.com/randlee/agent-team-mail/pull/152) |
| **E** | E.4 | TUI reliability hardening | COMPLETE | [#158](https://github.com/randlee/agent-team-mail/pull/158) |
| **E** | E.5 | TUI performance + UX polish | COMPLETE | [#161](https://github.com/randlee/agent-team-mail/pull/161) |
| **E** | E.6 | External agent member mgmt + model registry | COMPLETE | [#164](https://github.com/randlee/agent-team-mail/pull/164) |
| **E** | E.7 | Unified lifecycle source + MCP emission | COMPLETE | [#165](https://github.com/randlee/agent-team-mail/pull/165) |
| **E** | E.8 | Identity Role Mapping + Backup/Restore | COMPLETE | [#162](https://github.com/randlee/agent-team-mail/pull/162) |
| **E** | — | Daemon hook-event auth validation | COMPLETE | [#163](https://github.com/randlee/agent-team-mail/pull/163) |
| **G** | G.1 | Mode baseline docs + naming cleanup | COMPLETE | [#168](https://github.com/randlee/agent-team-mail/pull/168) |
| **G** | G.2 | CLI-JSON streaming verification | COMPLETE | [#175](https://github.com/randlee/agent-team-mail/pull/175) |
| **G** | G.3 | App-server transport adapter | COMPLETE | [#170](https://github.com/randlee/agent-team-mail/pull/170) |
| **G** | G.4 | Unified turn control | COMPLETE | [#171](https://github.com/randlee/agent-team-mail/pull/171) |
| **G** | G.5 | Approval/elicitation bridging | COMPLETE | [#172](https://github.com/randlee/agent-team-mail/pull/172) |
| **G** | G.6 | Mail injection parity | COMPLETE | [#173](https://github.com/randlee/agent-team-mail/pull/173) |
| **G** | G.7 | TUI streaming normalization + pubsub/UDP | COMPLETE | [#174](https://github.com/randlee/agent-team-mail/pull/174), [#176](https://github.com/randlee/agent-team-mail/pull/176) |
| **G** | G.8 | Cross-platform reliability + soak testing | COMPLETE | [#177](https://github.com/randlee/agent-team-mail/pull/177) |

| **L** | L.1a | Sink architecture + API structs (LogEventV1) | COMPLETE | integrate/phase-L |
| **L** | L.1b | `init_unified` + bridge to daemon socket | COMPLETE | integrate/phase-L |
| **L** | L.2 | Coverage — instrument all crates | COMPLETE | integrate/phase-L |
| **L** | L.3 | `atm logs` CLI command | COMPLETE | integrate/phase-L |
| **L** | L.4 | TUI log viewer + legacy sunset | COMPLETE | integrate/phase-L |
| **L** | L.5 | Direct watch stream + daemon boundary hardening | COMPLETE | [#201](https://github.com/randlee/agent-team-mail/pull/201) |
| **M** | M.1 | Watch-stream file naming/scoping cleanup | COMPLETE | [#206](https://github.com/randlee/agent-team-mail/pull/206) |
| **M** | M.1b | Legacy bridge removal (`emit_event_best_effort` sunset) | COMPLETE | [#213](https://github.com/randlee/agent-team-mail/pull/213) |
| **M** | M.2 | Codex watch-pane UI import baseline (copy-first) | COMPLETE | [#207](https://github.com/randlee/agent-team-mail/pull/207) |
| **M** | M.3 | Event adapter parity (`CodexAdapter`) | COMPLETE | [#208](https://github.com/randlee/agent-team-mail/pull/208) |
| **M** | M.4 | Input/approval/interrupt parity | COMPLETE | [#209](https://github.com/randlee/agent-team-mail/pull/209) |
| **M** | M.5 | Session/status surface parity | COMPLETE | [#210](https://github.com/randlee/agent-team-mail/pull/210) |
| **M** | M.6 | Replay/reconnect hardening | COMPLETE | [#211](https://github.com/randlee/agent-team-mail/pull/211) |
| **M** | M.7 | Golden parity test harness + CI gates | COMPLETE | [#212](https://github.com/randlee/agent-team-mail/pull/212) |

**Completed**: 88+ sprints across 19 phases (CI green)
**Current version**: v0.17.0
**Next**: Phase F (Team Installer) planned

---

## 20. Phase Integration PRs

| Phase | Integration PR | Status |
|-------|---------------|--------|
| Phase 3 | [#20](https://github.com/randlee/agent-team-mail/pull/20) | Merged |
| Phase 4 | [#25](https://github.com/randlee/agent-team-mail/pull/25) | Merged |
| Phase 5 | [#30](https://github.com/randlee/agent-team-mail/pull/30), [#33](https://github.com/randlee/agent-team-mail/pull/33) | Merged |
| Phase 7 | [#50](https://github.com/randlee/agent-team-mail/pull/50), [#51](https://github.com/randlee/agent-team-mail/pull/51) | Merged |
| Phase 8 | [#59](https://github.com/randlee/agent-team-mail/pull/59) | Merged |
| Phase 9 | [#75](https://github.com/randlee/agent-team-mail/pull/75) | Merged |
| Phase 10 | [#93](https://github.com/randlee/agent-team-mail/pull/93) | Merged |
| Phase A | [#103](https://github.com/randlee/agent-team-mail/pull/103) | Merged |
| Phase B | [#121](https://github.com/randlee/agent-team-mail/pull/121) | Merged |
| Phase C | [#126](https://github.com/randlee/agent-team-mail/pull/126) | Merged |
| Phase D | [#140](https://github.com/randlee/agent-team-mail/pull/140) | Merged |
| Phase E | [#166](https://github.com/randlee/agent-team-mail/pull/166) | Merged |
| Phase G | [#178](https://github.com/randlee/agent-team-mail/pull/178) | Merged |
| Phase L | [#199](https://github.com/randlee/agent-team-mail/pull/199) | Merged |
| Phase M | [#214](https://github.com/randlee/agent-team-mail/pull/214) | Pending |

---

## 21. TUI Bugs (Issues #181-#188)

| Issue | Description | Notes |
|-------|-------------|-------|
| #181 | Daemon not auto-starting | Resolved in Phase L daemon/logging stabilization (closed). |
| #182 | Agent roster not seeded from config.json | Resolved in TUI/daemon state sync hardening (closed). |
| #183 | Agent state never transitions after registration | Resolved by turn-state streaming + state-store wiring (closed). |
| #184 | TUI right panel contradicts left panel | Resolved by unified stream-state source in L.4-L.5 (closed). |
| #185 | No message viewing in TUI | Resolved by TUI stream + log viewer implementation (closed). |
| #186 | Per-agent output.log never written | Replaced by unified log filtering in L.4 (closed as superseded). |
| #187 | TUI header missing version number | Resolved by TUI header/version updates (closed). |
| #188 | Logging overhaul | Closed via Phase L completion (L.1a-L.5). |

---

## 22. Scrum Master Agent Prompt

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
**Last Updated**: 2026-02-23
**Maintained By**: Claude (ARCH-ATM)
