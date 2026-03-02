# agent-team-mail (`atm`) — Project Plan

**Version**: 0.5
**Date**: 2026-02-25
**Status**: Phase T complete (v0.27.0).

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
| M | Codex CLI Parity | Log/stream cleanup, Codex adapter, golden parity harness | COMPLETE |
| N | Hook Infrastructure | PID identity correlation, hook test harness | COMPLETE |
| O | Attached CLI Parity | Attach wiring, renderer parity, control-path + fixtures | COMPLETE |
| O-R | Attach Renderer Parity | RenderClass, event coverage, diff/markdown/reasoning rendering | COMPLETE |
| P | Attach Path Hardening Closure | Close O-R carry-forward attach deviations and parity hardening | COMPLETE |
| Q | MCP Server Setup CLI | `atm mcp install/status` for Claude Code, Codex, Gemini | COMPLETE |
| R | Session Handoff + Hook Installer | Daemon singleton lock, session registry, `atm doctor` | COMPLETE |
| S | Runtime Adapters + Hook Installer | Gemini adapter, `atm init` hook installer | COMPLETE |
| T | Daemon Reliability + Bug Debt | Fix daemon auto-start, config sync, TUI bugs, deferred S work | COMPLETE |

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

## 13. Phase B: Team-Lead Session Management — COMPLETE (B.1 deferred to Phase E)

**Integration PR**: [#121](https://github.com/randlee/agent-team-mail/pull/121)

| Sprint | Name | PR |
|--------|------|----|
| B.1 | Daemon session tracking + `atm teams resume` + `atm teams cleanup` (deferred to Phase E as E.1) | — |
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

## 16. Phase E: ATM Core Bug Fixes — COMPLETE (v0.15.0; E.6/E.7 deferred)

**Integration PR**: [#166](https://github.com/randlee/agent-team-mail/pull/166)

| Sprint | Name | PR |
|--------|------|----|
| E.1 | `atm teams resume` session ID reliability | [#147](https://github.com/randlee/agent-team-mail/pull/147) |
| E.2 | Inbox read scoping (fix cross-agent mark-as-read) | [#149](https://github.com/randlee/agent-team-mail/pull/149) |
| E.3 | Hook-to-daemon state bridge | [#152](https://github.com/randlee/agent-team-mail/pull/152) |
| E.4 | TUI reliability hardening (restart, reconnect, failure injection) | [#158](https://github.com/randlee/agent-team-mail/pull/158) |
| E.5 | TUI performance, UX polish, operational validation | [#161](https://github.com/randlee/agent-team-mail/pull/161) |
| E.6 | External agent member management + model registry (deferred) | — |
| E.7 | Unified lifecycle source model + MCP lifecycle emission (deferred) | — |
| E.8 | ATM Identity Role Mapping + Team Backup/Restore | [#162](https://github.com/randlee/agent-team-mail/pull/162) |
| — | Daemon hook-event auth validation | [#163](https://github.com/randlee/agent-team-mail/pull/163) |

**Dependency graph**: E.1 → {E.2, E.3} parallel; E.3 → {E.4, E.6} parallel; E.4 → E.5; E.6 → E.7; E.1 → E.8

---

## 16.5 Phase F: Team Installer (`atm team init`) — PLANNED

**Goal**: Install orchestration packages (hooks, agents, skills) into `~/.claude/` with `atm team init`.

**Status note (2026-02-27)**: Phase F is a historical planning bucket. Current execution for session handoff and hook installer work proceeds under **Phase R** (see section 17.7). Do not add new F.* sprints.

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
| G.9 | Docs finalization + release gate | [#178](https://github.com/randlee/agent-team-mail/pull/178) |

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

## 17.2 Phase N: Hook Infrastructure + PID Identity — COMPLETE (v0.18.0)

**Goal**: Add Claude Code hook test infrastructure, process ID logging across all hooks, and PID-based identity correlation with the new `atm register` command.

| Sprint | Name | Depends On | Status | PR |
|--------|------|------------|--------|----|
| N.1 | Hook test harness + process_id logging | M.7 | COMPLETE | [#216](https://github.com/randlee/agent-team-mail/pull/216) |
| N.2 | PID-based identity correlation + `atm register` | N.1 | COMPLETE | [#217](https://github.com/randlee/agent-team-mail/pull/217) |
| N.2-fix | Identity contract compliance fixes + `--as` flag | N.2 | COMPLETE | [#218](https://github.com/randlee/agent-team-mail/pull/218) |

**Scope**:
- PreToolUse/PostToolUse hook test harness with full gate-agent-spawns coverage.
- `process_id` (PID) logging in all hook scripts (session-start, session-end, teammate-idle-relay, gate-agent-spawns).
- New `atm register` command for PID-based identity correlation via hook files.
- Cross-platform Windows support via `sysinfo` crate for PID-to-session resolution.
- `--as` flag for `atm read` to read inbox as a specific identity.
- 10+ new integration tests for register command with env isolation hardening.

---

## 17.3 Phase O: Attached CLI Parity — COMPLETE

**Goal**: Deliver an `atm-agent-mcp attach <agent-id>` terminal mode with Codex CLI parity for output and interaction semantics while preserving ATM source attribution and daemon boundaries.

| Sprint | Name | Depends On | Status | PR |
|--------|------|------------|--------|----|
| O.1 | Attach command + stream/control wiring | M.7 | COMPLETE | [#223](https://github.com/randlee/agent-team-mail/pull/223) |
| O.2 | Renderer/runtime parity in attached mode | O.1 | COMPLETE | [#224](https://github.com/randlee/agent-team-mail/pull/224) |
| O.3 | Control-path parity (approval/reject, interrupt/cancel, fault states) | O.2 | COMPLETE | [#225](https://github.com/randlee/agent-team-mail/pull/225) |

**Scope**:
- Add `attach <agent-id>` as an interactive terminal entrypoint bound to one active session.
- Preserve source attribution metadata (`client_prompt`, `atm_mail`, `user_steer`) with Codex-parity ordering/formatting.
- Guarantee attach/detach/re-attach continuity with bounded replay and explicit fault surfacing.
- Track and approve intentional parity deviations through a maintained deviation log.

**References**:
- `docs/atm-agent-mcp/requirements.md` (FR-13.9, FR-23, Phase O sprint contract)
- `docs/atm-agent-mcp/live-stream-and-log-viewing.md` (watch and attached parity planning alignment)
- `docs/atm-agent-mcp/phase-o-event-applicability-matrix.md` (explicit event class scope for O.1/O.2/O.3)

---

## 17.4 Phase O-R: Attach Renderer Parity Closure — COMPLETE (v0.20.0)

**Goal**: Close the remaining attached-renderer parity gaps identified in post-Phase O review, with explicit deliverables and CI-verifiable acceptance criteria.

| Sprint | Name | Depends On | Size | Status | PR |
|--------|------|------------|------|--------|----|
| O-R.1 | Structured renderer foundation + applicability contract alignment | O.3 | M | COMPLETE | [#232](https://github.com/randlee/agent-team-mail/pull/232) |
| O-R.2 | Required event coverage expansion + unflattened class rendering | O-R.1 | L | COMPLETE | [#233](https://github.com/randlee/agent-team-mail/pull/233) |
| O-R.3 | Approval/elicitation interaction parity + correlated response routing | O-R.2 | L | COMPLETE | [#234](https://github.com/randlee/agent-team-mail/pull/234) |
| O-R.4 | Diff + markdown + reasoning render parity hardening | O-R.2 | L | COMPLETE | [#235](https://github.com/randlee/agent-team-mail/pull/235) |
| O-R.5 | Error/replay/telemetry/session hardening closure | O-R.3,O-R.4 | M | COMPLETE | [#236](https://github.com/randlee/agent-team-mail/pull/236), [#237](https://github.com/randlee/agent-team-mail/pull/237) |

**Deliverables and acceptance criteria**:
- O-R.1 deliverables: replace the generic attached print path for required classes with structured rendering primitives; add `applicability` field to attached JSON envelope.
- O-R.1 acceptance: required classes no longer rely on `[class][source_kind]` fallback; contract fixtures pass for applicability classification.
- O-R.2 deliverables: implement missing required event families (`mcp_tool_call_*`, `web_search_*`, `plan_*`, `session_configured`, `token_count`, `exec_command_begin`) and split flattened class handlers.
- O-R.2 acceptance: golden fixtures include representative events for each new family; no required family falls back to `unsupported.*` during fixture runs.
- O-R.3 deliverables: build approval/elicitation render+interaction parity with correlated response routing (no stdin-only approval shortcut), and distinct handling of `request_user_input`/`elicitation_request`/exec-approval/patch-approval events.
- O-R.3 acceptance: approval parity fixtures assert correlation-preserving round trip and class-distinct rendering for each approval/elicitation subtype.
- O-R.4 deliverables: implement file diff red/green rendering for `patch_apply*`/`turn_diff`; improve reasoning section-break handling and markdown rendering parity.
- O-R.4 acceptance: diff/reasoning/markdown fixtures pass across supported viewports with stable output snapshots.
- O-R.5 deliverables: add error-source classification (`proxy`/`child`/`upstream`), replay boundary/truncation signaling, unsupported-event summary on detach/end, stdin sanitization, checkpoint continuity, and help text parity for Ctrl-C behavior.
- O-R.5 acceptance: hardening fixtures validate replay/truncation/error-source/telemetry behavior and docs/help output matches runtime behavior.

**Gap-ID mapping**:
- O-R.1: GAP-008, GAP-015
- O-R.2: GAP-003, GAP-004
- O-R.3: GAP-002, GAP-005
- O-R.4: GAP-001, GAP-006, GAP-012
- O-R.5: GAP-009, GAP-010, GAP-011, GAP-013, GAP-014

**References**:
- `docs/atm-agent-mcp/requirements.md` (FR-23.12 through FR-23.25)
- `docs/atm-agent-mcp/codex-cli-atm-tui-render-gap-analysis.md` (current-state evidence and remediation map)
- `docs/atm-agent-mcp/phase-o-event-applicability-matrix.md` (required/degraded/out_of_scope policy)
- Integration completion PR: [#238](https://github.com/randlee/agent-team-mail/pull/238)

---

## 17.5 Phase P: Attach Path Hardening Closure — COMPLETE

**Goal**: Close all approved attach-path deviations carried from O-R so attach-mode behavior matches TUI parity commitments across error classification, replay continuity, telemetry closure, and operator input contract hardening.

| Sprint | Name | Depends On | Size | Status |
|--------|------|------------|------|--------|
| P.1 | Attach error-source + fatal reconnect parity | O-R.5 | M | COMPLETE ([#242](https://github.com/randlee/agent-team-mail/pull/242)) |
| P.2 | Attach replay boundary + checkpoint continuity | P.1 | M | COMPLETE ([#243](https://github.com/randlee/agent-team-mail/pull/243)) |
| P.3 | Attach unsupported-event summary flush parity | P.1 | S | COMPLETE ([#244](https://github.com/randlee/agent-team-mail/pull/244)) |
| P.4 | Attach stdin sanitization hardening | P.1 | M | COMPLETE ([#245](https://github.com/randlee/agent-team-mail/pull/245)) |
| P.5 | Attach help/UX contract parity (`Ctrl-C`/SIGINT) + closeout | P.2,P.3,P.4 | S | COMPLETE ([#246](https://github.com/randlee/agent-team-mail/pull/246)) |

**Deviation closure mapping**:
- P.1: DEV-OR5-001, DEV-OR5-002
- P.2: DEV-OR5-003, DEV-OR5-004
- P.3: DEV-OR5-005
- P.4: DEV-OR5-006
- P.5: DEV-OR5-007

**Carry-forward warnings mapped to Phase P**:
- P.1: QA-W2 (`print_frame` class-arm coverage for `input.client`, `input.user_steer`, `stream.error`, `stream.warning`)
- P.1: QA-W3 (`elicitation.request` split handling: `request_user_input` vs `elicitation_request`)
- P.2: QA-010 (`AdaptedWatchLine` applicability field parity carry-forward)
- P.3: QA-004 (below-threshold boundary coverage for `unknown_summary()`)
- P.4: QA-003 (unit tests for `StreamErrorProxy`/`StreamErrorChild`/`StreamErrorUpstream`/`StreamErrorFatal` render variants)

**P.5 closeout disposition**:
- QA-W2 resolved in P.1 (#242)
- QA-W3 resolved in P.1 (#242)
- QA-010 resolved in P.2 (#243)
- QA-004 resolved in P.3 (#244)
- QA-003 resolved in P.4 (#245)

---

## 17.6 Phase Q: MCP Server Setup CLI — IN PROGRESS

**Goal**: Add `atm mcp install/status` commands so users can configure `atm-agent-mcp` as an MCP server for Claude Code, Codex CLI, and Gemini CLI with a single command. See requirements section 4.8.

| Sprint | Name | Depends On | Size | Status |
|--------|------|------------|------|--------|
| Q.1 | `atm mcp install` + `atm mcp status` commands | — | M | COMPLETE |
| Q.2 | Integration tests + cross-platform validation | Q.1 | S | COMPLETE |
| Q.3 | MCP Inspector CI smoke tests for `atm-agent-mcp` standalone tools | Q.2 | S | COMPLETE |
| Q.4 | Manual MCP Inspector testing with live Codex + collaborative watch verification | Q.3 | M | PLANNED |

**Q.1 deliverables**:
- New `crates/atm/src/commands/mcp.rs` module
- `atm mcp install <client> [scope]` — configure MCP server for Claude/Codex/Gemini
- `atm mcp uninstall <client> [scope]` — remove MCP server configuration
- `atm mcp status` — show current MCP configuration across all clients
- In-process PATH resolution for `atm-agent-mcp` binary (no shell dependency)
- Claude Code: read-modify-write `~/.claude.json` (global) and `.mcp.json` (local)
- Codex: parse-and-merge TOML for `~/.codex/config.toml` (idempotent)
- Gemini: read-modify-write JSON for `~/.gemini/settings.json` and `.gemini/settings.json`
- Cross-scope deduplication: skip local install when global already configured
- Install outcome states: installed/updated/already-configured/skipped/error (per section 4.8.2)
- Uninstall outcome states: removed/not-present/error (per section 4.8.2a)

**Q.2 deliverables**:
- Unit tests for config read/modify/write per client format
- Integration tests using `ATM_HOME` isolation
- Windows CI validation for PATH-based binary detection
- Edge cases: missing config files, malformed JSON/TOML, already-configured (idempotency)

**Q.3 deliverables**:
- Extend `scripts/ci/mcp_inspector_smoke.sh` to keep reference-server baseline and add `atm-agent-mcp` smoke coverage
- Verify `tools/list` includes all 10 ATM tools:
  - `atm_send`, `atm_read`, `atm_broadcast`, `atm_pending_count`
  - `agent_sessions`, `agent_status`, `agent_close`
  - `agent_watch_attach`, `agent_watch_poll`, `agent_watch_detach`
- Verify `tools/call` contract and response schema for 7 standalone tools in an `ATM_HOME`-isolated environment:
  - `atm_send`, `atm_read`, `atm_broadcast`, `atm_pending_count`
  - `agent_sessions`, `agent_status`, `agent_close`
- Keep CI-safe scope: no live Codex execution in this gate

**Q.4 deliverables**:
- Run manual MCP Inspector sessions against live Codex-backed `atm-agent-mcp`
- Validate watch tools end-to-end (`agent_watch_attach`/`poll`/`detach`) and collaborative ATM mail flows
- Capture runbook evidence, known limitations, and parity notes for Phase Q closeout

---

## 17.7 Phase R: Session Handoff + Hook Installer — COMPLETE

**Goal**: Harden daemon foundations (singleton lock, canonical log sink), then build robust session startup for team-lead, hook installation via `atm init`, and embedded hook scripts in binary.

**Status**: ALL COMPLETE. R.0–R.0d merged in v0.24.0 (PR #272). R.0b hardened with cross-platform PID liveness (PR #277). R.0e complete. R.1/R.2a/R.2b moved to Phase S (R.2a completed as S.2a; R.1 deferred to Phase T).

### R.0 — Daemon singleton lock + canonical log sink alignment *(prerequisite)*

Harden daemon foundations required by R.1 session handoff:

1. **Singleton daemon lock**: Daemon acquires an exclusive process lock at `${config_dir}/atm/daemon.lock` on startup. Prevents multiple daemon instances from corrupting shared state (socket, PID file, session registry).
2. **Canonical log sink**: Resolve the path ambiguity between `ATM_HOME` override and XDG `config_dir`. Establish a single canonical scheme used consistently by daemon lock, `atm.log.jsonl`, and `log-spool` across all code paths and requirements docs.
3. **Structural lock enforcement in socket module**: The socket module must not remove the stale socket file unless the daemon lock is already held — enforce structurally (lock guard or marker type), not just by call-site ordering.
4. **Update requirements.md** sections 4.6 and 4.7 to reflect the canonical path scheme (removing the two-root ambiguity between ATM_HOME and config_dir).
5. **Tests**: Unit tests for lock acquisition, single-instance rejection, and log path resolution under both ATM_HOME-set and ATM_HOME-unset scenarios.

**Acceptance criteria**:
- `cargo clippy -- -D warnings` clean.
- `atm logs --limit 10` returns entries (not "Log file not found") in both default and `ATM_HOME`-override configurations.
- Two concurrent daemon starts: second instance exits with clear "daemon already running" error.
- requirements.md 4.6 and 4.7 path specs are internally consistent with implementation.

### R.0b — Persistent session registry + agent lifecycle management

Closes gaps identified during R.0 execution and dogfooding:

1. **Persistent session registry via hooks**: `session_start` hook writes `{agent_name, pid, session_id, team}` to daemon registry persistently. `session_end` hook removes entry. Daemon uses this for kill signals and liveness queries.
2. **`isActive` semantics**: `isActive` is advisory only. Daemon uses PID/session liveness as lifecycle truth and reconciles stale `isActive` drift in `config.json`.
3. **Shutdown-first teardown flow**: For active-agent termination intent, daemon sends `shutdown_request` to mailbox, waits `--timeout`, then force-kills PID if needed.
4. **Coupled teardown invariant**: After confirmed termination (already-dead or timeout+kill), daemon removes roster entry from `config.json` and deletes mailbox together (no partial state).
5. **`atm cleanup --agent <name>`**: CLI cleanup command is non-destructive for active agents unless explicit kill semantics are requested; active termination uses shutdown-first flow.
6. **Daemon `--kill <agent>`**: Runtime kill command backed by persistent registry and shutdown-first protocol.
7. **`atm teams spawn` Claude baseline**: promote `spawn-teammate.sh` behavior into first-class CLI semantics (frontmatter model/color + prompt body, ATM env override compatibility, repo-root launch, resume-aware parent session handoff, post-spawn registration updates).

**Acceptance criteria**:
- `atm status` reflects PID/session truth for idle-but-alive teammates even when `isActive` drifts.
- `atm cleanup --agent quality-mgr` does not remove active-agent mailbox/roster without explicit kill intent.
- For terminal agents, mailbox deletion and roster removal converge together (already-dead and kill-timeout cases).
- `atm daemon --kill <agent>` performs shutdown-first flow and terminates the named process by timeout boundary.
- `atm teams spawn` can reproduce current Claude teammate launcher behavior without custom scripts.

### R.0c — `atm doctor` diagnostics and operational cleanup guidance

Builds operational triage tooling on top of R.0b lifecycle truth.

1. **`atm doctor` command**: single health report command for daemon/session/cleanup drift.
2. **Daemon + PID scan**: verify daemon availability and reconcile live PID/session state for all members.
3. **Roster/session integrity**: detect config roster vs session registry mismatches and zombie artifacts.
4. **Mailbox hygiene checks**: detect stale terminal-agent mailboxes and partial teardown states.
5. **Unified log surfacing**: report warning/error events using incremental default window:
   `max(team-lead session start, last doctor call time)`.
6. **Cleanup recommendations**: output explicit remediation commands (`atm cleanup --agent`, daemon restart, re-register).

**Acceptance criteria**:
- `atm doctor` reports daemon-not-running as critical with clear recovery command.
- `atm doctor` detects and reports partial teardown drift (roster removed xor mailbox present).
- Default repeated runs are incremental for warning/error log output.
- JSON output mode is stable for automation.
### R.0d — Runtime compatibility spec (Gemini first, docs-only)

Define and review runtime-agnostic spawn/identity/teardown/steering contracts
using Gemini CLI as the first external runtime baseline. This sprint is
documentation/specification only (no implementation).

Deliverables:
1. Runtime compatibility design doc for Gemini covering launch flags, session
   model, lifecycle hooks, structured output transport, and signal behavior.
2. Requirements updates for:
   - runtime-aware teammate spawn (fresh + resume),
   - ATM identity vs runtime session identity mapping,
   - request-first teardown with escalation,
   - steering semantics (interactive + headless).
3. Explicit lifecycle envelope mapping for runtime adapters (`source.kind =
   "agent_hook"`) aligned with daemon authZ model.
4. Open-questions list for ACP/interactive steering reliability and default
   sandbox policy, plus additional integration questions (resume override UX,
   lifecycle event provenance, and default teardown timeout policy).

Acceptance criteria:
- Approved docs exist before any runtime adapter code is started.
- Requirements and project plan are consistent on Gemini-first scope and
  implementation sequencing.
- Docs explicitly capture known runtime limitations (e.g., cancel-then-steer if
  in-turn mutation is unavailable).

### R.0e — Runtime compatibility spec (OpenCode baseline, docs-only)

Extend the runtime compatibility spec with OpenCode-specific findings and draft
adapter requirements before implementation.

Deliverables:
1. Verified OpenCode runtime facts in `runtime-compatibility.md` covering:
   - CLI launch/resume controls (`--continue`, `--session`, `--fork`),
   - session identity model (`ses_*`),
   - instruction/system prompt surfaces,
   - interrupt/abort behavior.
2. Requirements updates for OpenCode baseline adapter behavior in section 4.3.
3. Open questions list for OpenCode backend strategy (CLI-pane vs server/API)
   and system-prompt materialization approach.

Acceptance criteria:
- OpenCode discovery findings are source-referenced and reviewable.
- Requirements are consistent with runtime-agnostic contracts already defined in
  R.0d.
- No adapter implementation code starts before docs review sign-off.
### R.1 — `atm teams resume` session handoff

**CLI flag semantics in handoff mode**:
- `message`: optional status text shown with refusal/re-establish guidance.
- `--session-id <id>`: target only the specified lead session. If it does not match the daemon's active lead session, refuse.
- `--force`: bypass soft refusal checks only when no active lead session is confirmed; never steals an active lead identity.
- `--kill`: explicitly terminate stale daemon-tracked lead process before handoff.

**Handoff flow**:
1. Daemon checks whether `team-lead` is active for the team (PID + session ID).
2. **If YES** (team-lead running in another process): refuse; do not steal team-lead identity.
3. **If NO** (no active team-lead):
   - Ensure backup destination exists at `.backups/<team>/<timestamp>/` (agent-team-api backup convention).
   - Create a flat backup snapshot compatible with `atm teams restore`: `config.json`, `inboxes/`, and `tasks/` directly under `.backups/<team>/<timestamp>/`.
   - Remove the active `<team>/` directory only after successful snapshot write.
   - Output: `"Call TeamCreate(<team>) to re-establish as team-lead"`.
4. Team-lead calls `TeamCreate(<team>)`; this succeeds because the active team directory is absent.
5. Daemon watches for `<team>/config.json` to appear.
6. Daemon restores non-Claude members from backup (pane IDs, agent types, inbox history).
7. Preserve the new `leadSessionId` from TeamCreate; restore never overwrites it. `team-lead` member is never restored from backup.
8. Daemon injects status into team-lead session: `"<team> re-established. Active members: <name> (<type>, pane <id>), ..."`.

**Failure-mode acceptance criteria**:
- Stale PID/session mismatch is detected and does not cause identity theft.
- Backup/move failure is surfaced with actionable error and no partial destructive delete.
- Daemon restart during restore resumes idempotently without duplicate members.
- Missing/corrupt backup is handled with explicit degraded-mode warning.
- Duplicate member IDs in backup are deduped deterministically.

### R.2a — `atm init` hook installer core

Install Claude Code hooks for ATM integration. Embedded hook scripts in binary (no external files needed).

**Hook path reference (Claude docs)**:
- https://docs.anthropic.com/en/docs/claude-code/hooks (redirects to https://code.claude.com/docs/en/hooks)
- Use `"$CLAUDE_PROJECT_DIR"/.claude/scripts/...` for project scripts.
- Use `"${CLAUDE_PLUGIN_ROOT}"/...` for plugin-bundled scripts.

- `atm init <team>` — local install (project `.claude/settings.json`)
- `atm init <team> --global` — global install (`~/.claude/settings.json`)
- Global hooks are passive in non-ATM repos (`.atm.toml` guard as first operation)
- Idempotent: safe to run multiple times; merges hook entries, never overwrites

### R.2b — `atm init --check` + upgrade validation

- `atm init --check` — report what's missing without making changes
- Validate upgrade path for existing installs while preserving user customizations

| Sprint | Name | Depends On | Size | Status |
|--------|------|------------|------|--------|
| R.0 | Daemon singleton lock + canonical log sink alignment | Phase Q | S | COMPLETE |
| R.0b | Persistent session registry + agent lifecycle management | R.0 | M | COMPLETE (PR #277) |
| R.0c | `atm doctor` diagnostics and cleanup guidance | R.0b | S | COMPLETE |
| R.0d | Runtime compatibility spec (Gemini first) (docs-only) | R.0b | S | COMPLETE |
| R.0e | Runtime compatibility spec (OpenCode baseline) (docs-only) | R.0d | S | COMPLETE |
| R.1 | `atm teams resume` session handoff + daemon member restore | R.0b | M | MOVED → Phase S |
| R.2a | `atm init` hook installer core + embedded scripts | — | M | MOVED → Phase S |
| R.2b | `atm init --check` + upgrade compatibility validation | S.2a | S | MOVED → Phase S |

---

## 17.8 Phase S: Runtime Adapters + Hook Installer — COMPLETE (v0.25.0)

**Goal**: Implement Gemini CLI runtime adapter, `atm init` hook installer, and (pending open-question resolution) OpenCode runtime adapter. Session handoff (old R.1) deferred for further design.

**Integration branch**: `integrate/phase-S` off `develop`.

### S.1 — Gemini baseline adapter *(runtime adapter, implementation)*

Implement the Gemini CLI runtime adapter defined in `docs/runtime-compatibility.md` sections 3–4 and requirements 4.3.8 (R-GEM-1 through R-GEM-7).

**Deliverables**:
1. `GeminiAdapter` struct implementing the runtime-agnostic spawn/identity/teardown/steering trait contract.
2. **Spawn** (R-GEM-1): launch `gemini` in a tmux pane with correct flags (`--sandbox false`, `--model`, system-prompt injection via stdin or `--prompt-interactive`). ATM identity set via env before launch.
3. **Identity contract** (R-GEM-2): ATM agent name is the identity anchor; Gemini session ID is ephemeral and opaque. `atm status` shows agent name, not Gemini session ID.
4. **Teardown** (R-GEM-3): request-first teardown (Ctrl-C / SIGINT to pane), 10s wait, SIGKILL escalation.
5. **Steering** (R-GEM-4): pane-based steering via `tmux send-keys`; no in-turn mutation assumed.
6. **Lifecycle hooks** (R-GEM-5): emit `agent_hook` lifecycle events (spawn, teardown) into daemon event stream.
7. **Observability** (R-GEM-6): `atm logs --agent <name>` surfaces Gemini adapter events using same log pipeline.
8. **Resume** (R-GEM-7): `atm teams spawn --resume <agent>` passes `--resume-session-id <id>` if daemon registry has a prior Gemini session ID for that agent name.
9. `atm teams spawn --runtime gemini <agent>` CLI flag to select adapter.
10. Tests: spawn/teardown integration test using a mock pane; resume flag test; identity isolation test.

**Acceptance criteria**:
- `atm teams spawn --runtime gemini arch-ctm` launches Gemini in a tmux pane and registers in daemon.
- `atm status` shows correct Online/Offline for Gemini agents using PID/pane liveness.
- Teardown flow sends SIGINT → waits → SIGKILL; no zombie panes.
- Resume flag is passed when prior session ID exists in registry.
- `cargo clippy -- -D warnings` clean; `cargo test` passes.

**References**: `docs/runtime-compatibility.md` §2–4; `docs/requirements.md` §4.3.4–4.3.8.

### S.2a — `atm init` hook installer core *(hook installer, implementation)*

Implement `atm init` as specified in `docs/requirements.md` §4.9.

**Deliverables**:
1. `atm init <team>` — writes ATM hook entries into `.claude/settings.json` (project-local).
2. `atm init <team> --global` — writes into `~/.claude/settings.json` (global scope).
3. Hook scripts embedded in binary at compile time (no external files needed post-install).
4. Idempotent: merges hook entries, never stomps existing user hooks; safe to re-run.
5. Global hooks are guarded by `.atm.toml` presence check as first operation (passive in non-ATM repos).
6. Hooks installed: `SessionStart`, `PreToolUse` (identity write), `PostToolUse` (state tracker).
7. Clear success/error output; non-zero exit on permission or parse errors.
8. Tests: idempotency test; merge test (existing hooks preserved); guard test (non-ATM repo no-op).

**Acceptance criteria**:
- `atm init atm-dev` writes correct hook entries and is safe to run multiple times.
- Existing user hooks in `settings.json` are preserved after `atm init`.
- Running in a repo without `.atm.toml` with `--global` skips execution with informational output.
- `cargo clippy -- -D warnings` clean; `cargo test` passes.

**References**: `docs/requirements.md` §4.9; `docs/agent-teams-hooks.md`.

### S.2b — `atm init --check` + upgrade validation

- `atm init --check` — report what's installed, what's missing, what's outdated; no writes.
- Validate upgrade path for existing installs: detect stale script hashes, offer `atm init` to refresh.
- Exit code: 0 = fully installed, 1 = missing/outdated, 2 = error.

**Acceptance criteria**:
- `atm init --check` exits 0 on a freshly-initialized repo and 1 when hooks are absent.
- Stale script hash is detected and reported with suggested remediation command.

### S.3 — OpenCode baseline adapter *(deferred — open questions)*

Deferred pending resolution of:
- Backend strategy: CLI-pane control vs server/API control model.
- System-prompt materialization: transient `--instructions` file vs persistent instruction surface.
- `ses_*` IDs in `atm status --verbose`: debug-only or default output?

**Status**: Not scheduled. Will be planned once open questions are resolved in user discussion.

### S.4 — `atm teams resume` session handoff *(deferred — needs design review)*

Old R.1. Deferred for further design review. The flow risks disrupting active non-lead members during team directory rotation. Requires pre-flight guard design before implementation.

**Status**: Not scheduled.

| Sprint | Name | Depends On | Size | Status |
|--------|------|------------|------|--------|
| S.1 | Gemini baseline adapter | R.0d | L | COMPLETE (PR #278) |
| S.2a | `atm init` hook installer core | — | M | COMPLETE (PR #276) |
| S.2b | `atm init --check` + upgrade validation | S.2a | S | MOVED → Phase T |
| S.3 | OpenCode baseline adapter | R.0e, S.1 | L | MOVED → Phase T |
| S.4 | `atm teams resume` session handoff | S.1 | M | MOVED → Phase T |

---

## 17.9 Phase T: Daemon Reliability + Bug Debt + Deferred Sprints

**Goal**: Fix critical daemon reliability bugs, close all open GitHub issues, and complete deferred Phase S work. The daemon is the foundation for state tracking — if it doesn't start, nothing else works.

**Integration branch**: `integrate/phase-T` off `develop`.

**Priority order**: Daemon reliability (#181-183) first, then remaining TUI/UX bugs, then deferred feature work.

### T.1 — Daemon auto-start on CLI usage *(bug fix, [#181](https://github.com/randlee/agent-team-mail/issues/181))*

**Problem**: Daemon does not auto-start when CLI commands are used. Users must manually start the daemon. `atm doctor` flags this as critical.

**Deliverables**:
1. CLI commands that require daemon (status, cleanup --agent, daemon --kill) auto-start daemon if not running.
2. Auto-start is transparent — no user action required.
3. Startup failure produces clear error message (port conflict, permissions, etc.).
4. Tests: verify auto-start on first CLI call; verify graceful error on startup failure.

**Acceptance criteria**:
- `atm status` on a fresh machine starts daemon automatically and returns correct status.
- `atm doctor` no longer flags "daemon not running" after any CLI usage.

### T.2 — Agent roster seeding + state transitions consolidation *(bug fix, [#182](https://github.com/randlee/agent-team-mail/issues/182), [#183](https://github.com/randlee/agent-team-mail/issues/183))*

> **Note**: T.2 and T.3 were combined into a single sprint execution. Issue #183 (agent state never transitions) was originally planned as T.3 but was folded into T.2 due to the tight coupling between roster seeding and state transition logic. The sprint table reflects this consolidation — T.3 does not appear as a separate entry.

**Problem**: Agent roster is not seeded from team `config.json` on daemon startup. Daemon starts with empty roster even when agents are configured. The daemon's filesystem watcher watches `inboxes/` but ignores `config.json`, so member adds/removes are invisible to the daemon.

**Deliverables**:
1. On daemon startup, read `config.json` for each team and seed roster with configured members.
2. Add `config.json` to the daemon's filesystem watcher (currently only watches `inboxes/` subdirectory).
3. On config.json change: reconcile roster (add new members, mark removed members, update changed fields).
4. Ensure config.json and mailbox state stay in sync — orphan mailboxes without config entries flagged, config entries without mailboxes get mailbox created.
5. Tests: daemon startup seeding; config.json member add triggers roster update; config.json member remove triggers cleanup; orphan mailbox detection.

**Acceptance criteria**:
- Starting daemon with a configured team shows all members in `atm status` immediately.
- Adding a member to config.json (e.g. via `atm teams add-member`) is reflected in daemon roster within one watch cycle.
- Removing a member from config.json triggers mailbox cleanup (or at minimum flags the orphan).

### T.4 — TUI panel consistency *(bug fix, [#184](https://github.com/randlee/agent-team-mail/issues/184))*

**Problem**: TUI right panel status contradicts left panel + stream panel empty.

**Naming note**: This is the **plan-level** `T.4` for issue #184. The
`docs/test-plan-phase-T.md` execution sequence also uses `T.4` label for Gemini
resume correctness (#281). Keep this distinction explicit to avoid cross-plan
numbering confusion.

**Deliverables**:
1. Right panel state derived from same source as left panel (unified state store).
2. Stream panel shows live output when available.
3. Tests: panel consistency verified via TUI test harness.

### T.5 — TUI message viewing *(enhancement, [#185](https://github.com/randlee/agent-team-mail/issues/185))*

**Problem**: No message viewing capability in TUI.

**Deliverables**:
1. Message list view in TUI showing inbox messages.
2. Message detail view with full content.
3. Mark-as-read on view.

### T.6 — TUI coverage closure *(combined sprint: [#184](https://github.com/randlee/agent-team-mail/issues/184) + [#185](https://github.com/randlee/agent-team-mail/issues/185) + [#187](https://github.com/randlee/agent-team-mail/issues/187))*

**Problem**: Three TUI issues delivered together: panel consistency (#184), message viewing (#185), and missing header version (#187). T.4 and T.5 deliverables were folded into this sprint (PR #299).

**Deliverables**:
1. Right panel state derived from same source as left panel (unified state store) — *from T.4 (#184)*.
2. Stream panel shows live output when available — *from T.4 (#184)*.
3. Message list view in TUI showing inbox messages — *from T.5 (#185)*.
4. Message detail view with full content and mark-as-read on view — *from T.5 (#185)*.
5. Display ATM version in TUI header bar, sourced from compile-time `CARGO_PKG_VERSION` — *from #187*.
6. Test coverage closure for all three issues via TUI test harness.

### T.7 — Permanent publishing process hardening + strengthened `publisher` role

**Problem**: Release publication checks and evidence are not yet enforced as a
single permanent process gate across all future releases.

**Deliverables**:
1. Strengthen `publisher` role responsibilities as the permanent release-quality
   gate owner (pre-publish audit, inventory completeness, post-publish
   verification evidence, residual risk reporting).
2. Require a formal release inventory per release with required fields:
   artifact identifier, version, source reference, publish target, verification
   command(s), and required/optional status.
3. Require post-publish verification for every required inventory item, with
   pass/fail evidence and remediation notes for failures.
4. Define completion gating: release is complete only when all required
   inventory items verify or explicit waivers are documented with approver and
   rationale.
5. Document this as default publishing procedure for subsequent releases.

**Acceptance criteria**:
- Missing required inventory fields, duplicate entries, or non-deterministic
  ordering fail release readiness validation.
- Required artifact verification failures block release completion unless waiver
  criteria are met.
- Publisher report includes audit summary, inventory location, verification
  outcomes, and residual risk list.

### T.8 — `atm teams resume` session handoff *(was S.4)*

Moved from Phase S. Old R.1. Requires pre-flight guard design to avoid disrupting active non-lead members during team directory rotation.

### T.9 — OpenCode baseline adapter *(was S.3, deferred — open questions)*

Moved from Phase S. Deferred pending resolution of backend strategy (CLI-pane vs server/API control model). Key finding from research: `opencode serve` + REST API is the correct control model.

### T.5b — `atm-monitor` agent: status polling + alerting *(enhancement)*

**Problem**: No continuous system health monitoring. Issues (stale sessions, config/mailbox drift, daemon errors) go undetected until someone manually runs `atm doctor`. Existing `log-monitor` agent (`.claude/agents/log-monitor.md`) can tail logs but doesn't poll status or alert proactively.

**Vision**: A lightweight sentinel agent that:
- **Polls** `atm status` / `atm doctor` periodically for health state changes
- **Watches** unified log + hook event journal with filters for warn/error events
- **Alerts** team-lead via `atm send` when issues are detected (config drift, stale sessions, daemon errors, PID death)
- **Debug mode**: runs as a full named teammate you can query interactively ("what happened 5 minutes ago?", "watch for the next session-start event", "why did arch-ctm go offline?")
- **Production mode**: runs as a background agent spun up on-demand when debugging issues

**Deliverables (implemented in this sprint)**:
1. Consolidated `atm-monitor` Claude Code agent definition (`.claude/agents/atm-monitor.md`) replacing/merging `log-monitor`.
2. `atm monitor` CLI subcommand: status polling loop that runs `atm doctor --json` on interval, diffs against previous state, alerts on new findings.
3. Alert dispatch: writes directly to recipient inbox files. Deduplicates repeat alerts (same finding within cooldown window). Supports `--once` and `--max-iterations` flags.
4. Integration tests: polling loop liveness, fault-within-2-cycles alerting, deduplication, daemon-unavailable resilience.

**Deferred to a future sprint**:
- Log watcher: tailing unified log (`atm.log.jsonl`) + hook events (`events.jsonl`) with configurable severity filter (default: warn+error). *Deferred — not implemented in T.5b.*
- Interactive query support: when run as named teammate, responds to questions about recent events, agent state history, log excerpts. *Deferred — not implemented in T.5b.*
- `atm monitor start` / `atm monitor stop` CLI subcommands to launch/stop as background process. *Deferred — not implemented in T.5b.*

**Acceptance criteria (T.5b)**:
- Running `atm-monitor` as background agent detects a deliberately killed agent PID and sends alert to team-lead within 2 poll cycles.
- Duplicate alerts for same finding are suppressed within cooldown window.
- Monitor loop does not exit/panic when daemon is unavailable — continues polling for all requested iterations.

### T.11 — Tmux Sentinel Injection *(enhancement, [#45](https://github.com/randlee/agent-team-mail/issues/45))*

Inject sentinel markers into tmux panes for reliable output boundary detection.

### T.12 — Codex Idle Detection via Notify Hook *(enhancement, [#46](https://github.com/randlee/agent-team-mail/issues/46))*

Detect Codex agent idle state via notify hook mechanism.

### T.13 — Ephemeral Pub/Sub for Agent Availability *(enhancement, [#47](https://github.com/randlee/agent-team-mail/issues/47))*

Lightweight pub/sub mechanism for agent availability announcements.

### T.14 — Gemini adapter resume flag fix *(bug fix, [#281](https://github.com/randlee/agent-team-mail/issues/281))*

`GeminiAdapter.build_command()` emits `--resume --resume-session-id <id>` but verified Gemini CLI uses `--resume <session_id>` as positional arg. Fix flag construction and unit test.

**Status**: COMPLETE ([PR #297](https://github.com/randlee/agent-team-mail/pull/297)).

### T.15 — Gemini adapter end-to-end spawn/teardown wiring *(enhancement, [#282](https://github.com/randlee/agent-team-mail/issues/282))*

S.1 delivered the adapter trait only. Wire `GeminiAdapter` into the tmux spawn pipeline: pane creation, daemon registration, SIGINT/SIGKILL teardown, lifecycle event emission.

### T.16 — S.2a/S.1 plan deliverable accuracy *(documentation, [#283](https://github.com/randlee/agent-team-mail/issues/283))*

Update project-plan.md S.2a deliverable #6 to reflect actual hooks installed (SessionStart, PreToolUse identity, PreToolUse Task gate, PostToolUse cleanup). Note TeammateIdle/SessionEnd deferred.

### Closed/Superseded Issues

| Issue | Status | Notes |
|-------|--------|-------|
| [#186](https://github.com/randlee/agent-team-mail/issues/186) | CLOSED | Superseded by Phase L. Per-agent output.log replaced by unified log filtering (`atm logs --agent`). |
| [#188](https://github.com/randlee/agent-team-mail/issues/188) | CLOSED | Superseded by Phase L. Logging overhaul completed in L.1a-L.5. |

| Sprint | Name | Depends On | Size | Status | Issue |
|--------|------|------------|------|--------|-------|
| T.1 | Daemon auto-start on CLI usage | — | M | COMPLETE | [#181](https://github.com/randlee/agent-team-mail/issues/181) — PR #288 |
| T.2 | Agent roster seeding + state transitions | T.1 | M | COMPLETE | [#182](https://github.com/randlee/agent-team-mail/issues/182), [#183](https://github.com/randlee/agent-team-mail/issues/183) — PR #289 |
| T.4 | TUI panel consistency (stdin fix) | T.2 | S | COMPLETE | [#184](https://github.com/randlee/agent-team-mail/issues/184) — delivered in T.6 combined sprint (PR #299) |
| T.5 | TUI message viewing | T.1 | M | COMPLETE | [#185](https://github.com/randlee/agent-team-mail/issues/185) — delivered in T.6 combined sprint (PR #299) |
| T.5a | CLI crate publishability hardening | T.2 | S | COMPLETE | [#284](https://github.com/randlee/agent-team-mail/issues/284) |
| T.6 | TUI coverage closure (#184 + #185 + #187) | — | M | COMPLETE | [#184](https://github.com/randlee/agent-team-mail/issues/184), [#185](https://github.com/randlee/agent-team-mail/issues/185), [#187](https://github.com/randlee/agent-team-mail/issues/187) — PR #299 |
| T.7 | Permanent publishing process hardening + strengthened `publisher` role | T.5a | S | COMPLETE | — PR #298 |
| T.8 | `atm teams resume` session handoff | S.1 | M | PLANNED | — |
| T.9 | OpenCode baseline adapter | S.1 | L | DEFERRED | — |
| T.5b | Operational health agent / continuous doctor | T.2 | M | COMPLETE | — |
| T.5c | Availability signaling clarification | T.2 | S | COMPLETE | [#46](https://github.com/randlee/agent-team-mail/issues/46), [#47](https://github.com/randlee/agent-team-mail/issues/47) |
| T.11 | Tmux Sentinel Injection | — | M | PLANNED | [#45](https://github.com/randlee/agent-team-mail/issues/45) |
| T.12 | Codex Idle Detection via Notify Hook *(superseded by T.5c)* | — | M | SUPERSEDED | [#46](https://github.com/randlee/agent-team-mail/issues/46) |
| T.13 | Ephemeral Pub/Sub for Agent Availability *(superseded by T.5c)* | — | M | SUPERSEDED | [#47](https://github.com/randlee/agent-team-mail/issues/47) |
| T.14 | Gemini adapter resume flag fix | — | XS | COMPLETE (PR #297) | [#281](https://github.com/randlee/agent-team-mail/issues/281) |
| T.15 | Gemini adapter end-to-end spawn wiring | T.14 | L | COMPLETE | [#282](https://github.com/randlee/agent-team-mail/issues/282) |
| T.16 | S.2a/S.1 plan deliverable accuracy | — | XS | PLANNED | [#283](https://github.com/randlee/agent-team-mail/issues/283) |

---
## 17.10 Phase X: Team Join UX + Cross-Folder Spawn Planning

**Goal**: Add a first-class `/team-join` onboarding flow for existing teams and
standardize runtime launch path selection with `--folder` across spawn surfaces.
Add a one-command `atm init` onboarding flow for `.atm.toml`, team creation, and
hook installation defaults.
**Execution reference**: `docs/test-plan-phase-X.md`.

**Integration branch**: `integrate/phase-X` off `develop`.

### X.1 — `/team-join` contract and skill/CLI alignment ([#351](https://github.com/randlee/agent-team-mail/issues/351))

**Problem**: Existing onboarding requires manual multi-step team/member/session
coordination and does not provide a single guided flow for joining an established
team.

**Deliverables**:
1. Define `/team-join` slash-command UX contract (skill entrypoint) with
   deterministic behavior and explicit outputs.
2. Add CLI contract for `atm teams join`:
   - caller team-context check first,
   - `--team` optional verification in team-lead-initiated mode,
   - required `--team` when caller has no current team context.
3. Define post-join output contract with a copy-pastable
   `claude --resume ...` launch command (folder-aware).
4. Add acceptance test plan for join flow:
   - team-lead-initiated path,
   - self-join path,
   - team mismatch rejection path.

### X.2 — Spawn path normalization: `--folder` support (runtime launch portability)

**Problem**: Spawn launch directory semantics are inconsistently expressed across
runtime surfaces (`--cwd`, repo-root wording, tmux launch wrappers), making
cross-directory session launch fragile.

**Deliverables**:
1. Standardize `--folder <path>` as canonical spawn directory flag with
   `--cwd` compatibility alias.
2. Require identical behavior for Claude/Codex/Gemini spawn flows, including
   tmux-initiated launches.
3. Add validation rule: if both `--folder` and `--cwd` are provided, they must
   match after canonicalization.
4. Add tests for folder resolution and launch command generation in same-folder
   and cross-folder cases.
5. Add codex/gemini startup guidance prompt-injection contract:
   - inject ATM usage guidance before/after caller-supplied prompt,
   - emit guidance-only startup prompt when caller prompt is omitted,
   - verify command text uses current ATM CLI syntax.

### X.3 — `atm init` one-command setup + default-global hooks ([#357](https://github.com/randlee/agent-team-mail/issues/357))

**Problem**: `atm init` currently installs hooks only. Users must separately create
`.atm.toml` and create team state, and local-hook default can silently no-op in
worktree-driven launches.

**Deliverables**:
1. Expand `atm init <team>` flow to run in idempotent order:
   - create `.atm.toml` in cwd when missing (`identity`, `default_team`),
   - create team directory/roster when missing,
   - install hooks.
2. Change install-mode default:
   - default hook install target becomes global scope,
   - add `--local` as explicit project-scoped opt-out.
3. Add flags:
   - `--identity <name>` to seed `.atm.toml` identity (default `team-lead`),
   - `--skip-team` to skip team creation for existing-team joins.
4. Preserve idempotency and explicit status output:
   - existing `.atm.toml` is detected and not overwritten silently,
   - existing team is detected and not recreated,
   - existing hook entries are not duplicated.
5. Add/update `docs/quickstart.md`:
   - one-command setup flow,
   - global-vs-local hook guidance (worktree rationale),
   - first-send/read + `docs/team-protocol.md` pointer.

**Acceptance criteria**:
- Fresh repo: `atm init my-team` creates `.atm.toml`, creates team, installs hooks globally.
- Re-running `atm init` is idempotent (no duplicate hooks, no destructive config churn).
- `--local` installs project-local hooks with preserved existing semantics.
- `--skip-team` skips team creation while still configuring `.atm.toml`/hooks.
- `--identity` writes requested identity in `.atm.toml`.
- `docs/quickstart.md` documents the new flow and worktree/global rationale.

| Sprint | Name | Depends On | Size | Status | Issue |
|--------|------|------------|------|--------|-------|
| X.1 | `/team-join` contract + slash-command flow planning | — | M | PLANNED | [#351](https://github.com/randlee/agent-team-mail/issues/351) |
| X.2 | Spawn `--folder` normalization across runtimes | X.1 | S | PLANNED | (new tracker to create) |
| X.3 | `atm init` one-command setup + default-global hooks | X.1 | M | PLANNED | [#357](https://github.com/randlee/agent-team-mail/issues/357) |

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
| **B** | B.1 | Teams daemon session tracking + resume | DEFERRED (moved to E.1) | — |
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
| **E** | E.6 | External agent member mgmt + model registry | DEFERRED | — |
| **E** | E.7 | Unified lifecycle source + MCP emission | DEFERRED | — |
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
| **N** | N.1 | Hook test harness + process_id logging | COMPLETE | [#216](https://github.com/randlee/agent-team-mail/pull/216) |
| **N** | N.2 | PID-based identity correlation + `atm register` | COMPLETE | [#217](https://github.com/randlee/agent-team-mail/pull/217) |
| **N** | N.2-fix | Identity contract compliance fixes + `--as` flag | COMPLETE | [#218](https://github.com/randlee/agent-team-mail/pull/218) |
| **O** | O.1 | Attach command + stream/control wiring | COMPLETE | [#223](https://github.com/randlee/agent-team-mail/pull/223) |
| **O** | O.2 | Renderer/runtime parity in attached mode | COMPLETE | [#224](https://github.com/randlee/agent-team-mail/pull/224) |
| **O** | O.3 | Control-path parity (approval/reject, interrupt/cancel, fault states) | COMPLETE | [#225](https://github.com/randlee/agent-team-mail/pull/225) |
| **O-R** | O-R.1 | Structured renderer foundation + applicability contract alignment | COMPLETE | [#232](https://github.com/randlee/agent-team-mail/pull/232) |
| **O-R** | O-R.2 | Required event coverage expansion + unflattened class rendering | COMPLETE | [#233](https://github.com/randlee/agent-team-mail/pull/233) |
| **O-R** | O-R.3 | Approval/elicitation interaction parity + correlated response routing | COMPLETE | [#234](https://github.com/randlee/agent-team-mail/pull/234) |
| **O-R** | O-R.4 | Diff + markdown + reasoning render parity hardening | COMPLETE | [#235](https://github.com/randlee/agent-team-mail/pull/235) |
| **O-R** | O-R.5 | Error/replay/telemetry/session hardening closure | COMPLETE | [#236](https://github.com/randlee/agent-team-mail/pull/236), [#237](https://github.com/randlee/agent-team-mail/pull/237) |
| **P** | P.1 | Attach error-source + fatal reconnect parity | COMPLETE | [#242](https://github.com/randlee/agent-team-mail/pull/242) |
| **P** | P.2 | Attach replay boundary + checkpoint continuity | COMPLETE | [#243](https://github.com/randlee/agent-team-mail/pull/243) |
| **P** | P.3 | Attach unsupported-event summary flush parity | COMPLETE | [#244](https://github.com/randlee/agent-team-mail/pull/244) |
| **P** | P.4 | Attach stdin sanitization hardening | COMPLETE | [#245](https://github.com/randlee/agent-team-mail/pull/245) |
| **P** | P.5 | Attach help/UX contract parity (`Ctrl-C`/SIGINT) + closeout | COMPLETE | [#246](https://github.com/randlee/agent-team-mail/pull/246) |
| **Q** | Q.1 | `atm mcp install/uninstall/status` commands | COMPLETE | [#252](https://github.com/randlee/agent-team-mail/pull/252) |
| **Q** | Q.2 | Integration tests + cross-platform validation | COMPLETE | [#253](https://github.com/randlee/agent-team-mail/pull/253) |
| **Q** | Q.3 | MCP Inspector CI smoke tests for `atm-agent-mcp` standalone tools | COMPLETE | — |
| **Q** | Q.4 | Manual MCP Inspector testing with live Codex + collaborative watch verification | PLANNED | — |

**Completed**: 99+ sprints across 23 phases (CI green)
**Current version**: v0.27.0
**Next**: Phase X (planning)

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
| Phase M | [#214](https://github.com/randlee/agent-team-mail/pull/214) | Merged |
| Phase N | [#221](https://github.com/randlee/agent-team-mail/pull/221) | Merged |
| Phase O-R | [#238](https://github.com/randlee/agent-team-mail/pull/238) | Merged |
| Phase P | Sprint PRs targeted develop directly (no integration branch) | Merged |

---

## 21. Open Issues Tracker

**WARNING**: All issues below are OPEN on GitHub. Do not mark as resolved without verifying the fix exists AND closing the GitHub issue.

| Issue | Description | Phase T Sprint | Notes |
|-------|-------------|----------------|-------|
| [#181](https://github.com/randlee/agent-team-mail/issues/181) | Daemon not auto-starting | T.1 | **Critical** — blocks all daemon-dependent features |
| [#182](https://github.com/randlee/agent-team-mail/issues/182) | Agent roster not seeded from config.json | T.2 | **Critical** — daemon starts with empty roster |
| [#183](https://github.com/randlee/agent-team-mail/issues/183) | Agent state never transitions | T.2 | **Critical** — state tracking broken (consolidated into T.2, PR #289) |
| [#184](https://github.com/randlee/agent-team-mail/issues/184) | TUI right panel contradicts left panel | T.4 | Needs investigation — may be fixed by Phase L |
| [#185](https://github.com/randlee/agent-team-mail/issues/185) | No message viewing in TUI | T.5 | Enhancement |
| [#186](https://github.com/randlee/agent-team-mail/issues/186) | Per-agent output.log never written | — | May be superseded by Phase L unified logging — **needs verification** |
| [#187](https://github.com/randlee/agent-team-mail/issues/187) | TUI header missing version number | T.6 | Quick fix |
| [#188](https://github.com/randlee/agent-team-mail/issues/188) | Logging overhaul prerequisite | — | May be addressed by Phase L — **needs verification** |
| [#45](https://github.com/randlee/agent-team-mail/issues/45) | Tmux Sentinel Injection | T.11 | Enhancement |
| [#46](https://github.com/randlee/agent-team-mail/issues/46) | Codex Idle Detection via Notify Hook | T.12 | Enhancement |
| [#47](https://github.com/randlee/agent-team-mail/issues/47) | Ephemeral Pub/Sub for Agent Availability | T.13 | Enhancement |
| [#351](https://github.com/randlee/agent-team-mail/issues/351) | Add `/team-join` slash command | X.1 | New onboarding UX contract; paired with `atm teams join` CLI planning |
| [#357](https://github.com/randlee/agent-team-mail/issues/357) | `atm init` full one-command setup + default global hooks | X.3 | One-command onboarding (`.atm.toml` + team + hooks) plus quickstart updates |

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

**Document Version**: 0.5
**Last Updated**: 2026-02-25
**Maintained By**: Claude (ARCH-ATM)
