# agent-team-mail (`atm`) — Project Plan

**Version**: 0.7
**Date**: 2026-03-10
**Status**: Phase AM refactor in progress. Phase AK queued.

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
| X | Team Onboarding + TUI/Doctor Stability | `/team-join`, spawn path normalization, `atm init` one-command setup, and carry-forward bug-debt mapping | PLANNED |
| Z | Daemon SSoT + Observability Hardening | Canonical daemon-owned member state, session-registry sync closure, and doctor/status observability consistency (Z.1–Z.7 COMPLETE) | COMPLETE |
| AA | Session Correctness + Spawn Authorization + Reliability UX | Session-end correctness, spawn authorization, cleanup/help reliability hardening | COMPLETE |
| AB | GitHub CI Monitor Command + Availability Hardening | Complete `atm gh` plugin requirements and deliver monitor/state/reporting contracts | COMPLETE |
| AC | Daemon Status Convergence + Hook Install Confidence | Finalize daemon status/lifecycle consistency and pre-release hook install confidence for local/global paths | COMPLETE |
| AD | Cross-Platform Script Standardization | Python-first script conversion and runtime policy hardening across ATM tooling | COMPLETE |
| AE | GH Monitor Reliability + Daemon Logging | Stabilize gh-monitor status/lifecycle contracts and daemon observability behavior | COMPLETE |
| AF | External Agent Lifecycle Hardening | Close lifecycle, cleanup, transient registration, and reliability/documentation hardening | COMPLETE |
| AG | sc-composer Full Implementation + CLI | Deliver `sc-composer` library + `sc-compose` CLI and integrate with `atm teams spawn` via direct library APIs | COMPLETE |
| AH | Observability Unification + AG Deferred Closure | Unified JSONL logging pipeline via `sc-observability` crate and baseline observability contracts (OTel/scmux/schook deferred) | COMPLETE |
| AI | GH Monitor Dashboard + Detailed PR Reporting | `atm gh pr list`, `atm gh pr report`, `--template` rendering, `init-report`; CI rollup neutral/skipped fix | IN-PROGRESS |
| AM | CI Monitor Subsystem Refactor | Extract CI-monitor subsystem boundaries out of `socket.rs`, split provider-neutral logic from GitHub-specific adapter logic, and stabilize routing/health/test support on `integrate/phase-AM` | IN-PROGRESS |
| AO | GH Monitor Guardrails + Runtime Admission | Prevent accidental shared-runtime pollers, add isolated-runtime TTL policy, and make GH usage attributable/self-limiting with cached repo-state and operator controls | PLANNED |
| AJ | Session-ID SSoT Normalization | Canonical `session_id` naming, shared caller resolver, runtime session resolution closure, doctor/session consistency | PLANNED |
| AK | Mandatory OTel Rollout | Non-optional OTel across in-scope tools with canonical correlation and health/reporting contracts | PLANNED |

---

## Testing Backlog

- Issue [#655](https://github.com/randlee/agent-team-mail/issues/655) tracks the
  comprehensive workspace testing strategy and ignored-test cleanup effort.
  Initial deliverable PR: [#657](https://github.com/randlee/agent-team-mail/pull/657).

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
- Update `docs/logging-l1a-spec.md` and `docs/observability/requirements.md` to mark bridge as removed.
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
- `--project <name>`: include Claude Code project-scoped task files in the automatic backup/restore handoff path as `tasks-cc/`.
- `--session-id <id>`: target only the specified lead session. If it does not match the daemon's active lead session, refuse.
- `--force`: bypass soft refusal checks only when no active lead session is confirmed; never steals an active lead identity.
- `--kill`: explicitly terminate stale daemon-tracked lead process before handoff.

**Handoff flow**:
1. Daemon checks whether `team-lead` is active for the team (PID + session ID).
2. **If YES** (team-lead running in another process): refuse; do not steal team-lead identity.
3. **If NO** (no active team-lead):
   - Ensure backup destination exists at `.backups/<team>/<timestamp>/` (agent-team-api backup convention).
   - Create a flat backup snapshot compatible with `atm teams restore`: `config.json`, `inboxes/`, and `tasks/` directly under `.backups/<team>/<timestamp>/`.
   - When `--project <name>` is supplied, include Claude Code project task-list files as `tasks-cc/` sourced from `~/.claude/tasks/<project>/`. If that source path is absent, omit `tasks-cc/` without error.
   - Remove the active `<team>/` directory only after successful snapshot write.
   - Output: `"Call TeamCreate(<team>) to re-establish as team-lead"`.
4. Team-lead calls `TeamCreate(<team>)`; this succeeds because the active team directory is absent.
5. Daemon watches for `<team>/config.json` to appear.
6. Daemon restores non-Claude members from backup (pane IDs, agent types, inbox history).
7. Preserve the new `leadSessionId` from TeamCreate; restore never overwrites it. `team-lead` member is never restored from backup.
8. Daemon injects status into team-lead session: `"<team> re-established. Active members: <name> (<type>, pane <id>), ..."`.
9. Restore recomputes `.highwatermark` for each restored task directory from the highest numeric task id present after file copy; when no numeric task files are present, it sets `.highwatermark` to `0`.
10. `atm teams remove-member --archive-inbox` archives mail to `.claude/teams/.archives/<team>/removed-<agent>-<timestamp>/` so retention pruning for `.backups/` cannot delete archived mail unexpectedly.

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
**Dependency graph**: X.1 → {X.2, X.3}; X.4/X.5/X.6 deferred follow-on.

**Dependency rationale**:
- X.2 depends on X.1 because join/launch output contracts must settle canonical
  folder semantics before spawn normalization can be finalized.
- X.3 depends on X.1 so one-command init guidance and join guidance remain
  consistent (single onboarding contract, no split UX semantics).

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
5. Align output contract explicitly with requirements JSON schema in
   `docs/requirements.md` §4.3.2a (`team`, `agent`, `folder`, `launch_command`, `mode`).

**Acceptance criteria**:
- `atm teams join --help` documents required surface (`<agent>`, optional
  `--team`, and output mode support).
- Team-mismatch rejection path returns non-zero and explicit mismatch guidance.
- JSON output contains all required fields from `docs/requirements.md` §4.3.2a.
- Human output includes copy-pastable launch command with explicit folder context.

**References**: `docs/requirements.md` §4.3.2a; `docs/test-plan-phase-X.md` X.1.

### X.2 — Spawn path normalization: `--folder` support (runtime launch portability) ([#361](https://github.com/randlee/agent-team-mail/issues/361))

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
5. Create `docs/quickstart.md` (new document):
   - one-command setup flow,
   - global-vs-local hook guidance (worktree rationale),
   - first-send/read + `docs/team-protocol.md` pointer.

**Acceptance criteria**:
- Fresh repo: `atm init my-team` creates `.atm.toml`, creates team, installs hooks globally.
- Re-running `atm init` is idempotent (no duplicate hooks, no destructive config churn).
- `--local` installs project-local hooks with preserved existing semantics.
- `--skip-team` skips team creation while still configuring `.atm.toml`/hooks.
- `--identity` writes requested identity in `.atm.toml`.
- `docs/quickstart.md` is created with minimum required sections and worktree/global rationale.

### Deferred Technical Debt Carry-Forward (Phase X Follow-On)

The following issues are explicitly tracked but deferred from X.1-X.3 to keep
the current tranche focused on onboarding contract closure.

| Sprint | Issue | Status | Deferral Rationale |
|--------|-------|--------|--------------------|
| X.4 | [#287](https://github.com/randlee/agent-team-mail/issues/287) | DEFERRED | Doctor duration parser correctness is isolated from join/init onboarding scope; scheduled after X.1-X.3 merge. |
| X.5 | [#337](https://github.com/randlee/agent-team-mail/issues/337) | DEFERRED | Test-serialization hardening is CI debt cleanup and can proceed independently after onboarding contract stabilization. |
| X.6 | [#338](https://github.com/randlee/agent-team-mail/issues/338) | DEFERRED | `add-member` inbox atomicity is important but not a prerequisite for `/team-join`/`atm init` contract planning closure in this tranche. |

| Sprint | Name | Depends On | Size | Status | Issue |
|--------|------|------------|------|--------|-------|
| X.1 | `/team-join` contract + slash-command flow planning | — | M | PLANNED | [#351](https://github.com/randlee/agent-team-mail/issues/351) |
| X.2 | Spawn `--folder` normalization across runtimes | X.1 | S | PLANNED | [#361](https://github.com/randlee/agent-team-mail/issues/361) |
| X.3 | `atm init` one-command setup + default-global hooks | X.1 | M | PLANNED | [#357](https://github.com/randlee/agent-team-mail/issues/357) |
| X.4 | Doctor duration parser boundary fix (`parse_since_input`) | — | XS | DEFERRED | [#287](https://github.com/randlee/agent-team-mail/issues/287) |
| X.5 | Serialize env-mutating daemon tests (`ATM_HOME`) | — | S | DEFERRED | [#337](https://github.com/randlee/agent-team-mail/issues/337) |
| X.6 | `teams add-member` inbox atomicity | — | S | DEFERRED | [#338](https://github.com/randlee/agent-team-mail/issues/338) |

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
| **Z** | Z.1 | Quick Wins: Doctor + Release Fix | COMPLETE | [#423](https://github.com/randlee/agent-team-mail/pull/423) |
| **Z** | Z.2 | Log Format + Doctor UX | COMPLETE | [#425](https://github.com/randlee/agent-team-mail/pull/425) |
| **Z** | Z.3 | SSoT Fast Path (`register-hint`, send-path daemon sync) | COMPLETE | [#427](https://github.com/randlee/agent-team-mail/pull/427) |
| **Z** | Z.4 | Canonical Member State Completion | COMPLETE | [#429](https://github.com/randlee/agent-team-mail/pull/429) |
| **Z** | Z.5 | Lifecycle Logging + Hook Events | COMPLETE | [#430](https://github.com/randlee/agent-team-mail/pull/430) |
| **Z** | Z.6 | Cross-folder Spawn + QA Blocker Closure | COMPLETE | [#431](https://github.com/randlee/agent-team-mail/pull/431) |
| **Z** | Z.7 | Review Findings Hardening | COMPLETE (d1–7 shipped; d8–12 deferred) | [#432](https://github.com/randlee/agent-team-mail/pull/432), [#433](https://github.com/randlee/agent-team-mail/pull/433), [#435](https://github.com/randlee/agent-team-mail/pull/435) |
| **AA** | AA.1 | Session-End Correctness Hardening | COMPLETE | [#453](https://github.com/randlee/agent-team-mail/pull/453) |
| | AA.2 | Spawn Authorization Gate Alignment | COMPLETE | [#455](https://github.com/randlee/agent-team-mail/pull/455) |
| | AA.3 | CI/Release Reliability Closure | COMPLETE | [#454](https://github.com/randlee/agent-team-mail/pull/454) |
| | AA.4 | Cleanup + Spawn Help UX Polish | COMPLETE | [#457](https://github.com/randlee/agent-team-mail/pull/457) |
| **AB** | AB.1 | GitHub CI Monitor Requirements Lock + Core Contracts | COMPLETE | [#462](https://github.com/randlee/agent-team-mail/pull/462) |
| | AB.2 | `atm gh monitor` Command Surface | COMPLETE | [#463](https://github.com/randlee/agent-team-mail/pull/463) |
| | AB.3 | Progress + Final Reporting Payloads | COMPLETE | [#464](https://github.com/randlee/agent-team-mail/pull/464) |
| | AB.4 | Availability State + Connectivity Recovery Signals | COMPLETE | [#465](https://github.com/randlee/agent-team-mail/pull/465) |
| | AB.5 | Runtime Drift Baselines (Optional Enhancement) | COMPLETE | [#466](https://github.com/randlee/agent-team-mail/pull/466) |
| | AB.6 | PR Merge-Conflict + CI Gap Detection | COMPLETE | [#467](https://github.com/randlee/agent-team-mail/pull/467) |
| | AB.7 | Architecture Review Findings Hardening | COMPLETE | [#468](https://github.com/randlee/agent-team-mail/pull/468) |
| **AC** | AC.1 | ReconcileCycleState Per-Test Injection | COMPLETE | `41053cf` (integrate/phase-AC) |
| | AC.1b | Codex PPID Detection + Stable Session Key | COMPLETE | `da6cae5` (integrate/phase-AC) |
| | AC.2 | Cleanup Guard Tests + gh Monitor Repo Validation | COMPLETE | [#476](https://github.com/randlee/agent-team-mail/pull/476), [#484](https://github.com/randlee/agent-team-mail/pull/484) |
| | AC.3 | atm spawn Interactive UX | COMPLETE | [#477](https://github.com/randlee/agent-team-mail/pull/477) |
| | AC.4 | Daemon Logging Observability | COMPLETE | [#479](https://github.com/randlee/agent-team-mail/pull/479) |
| | AC.5 | Daemon Status Convergence + Lifecycle State Validation | COMPLETE | [#481](https://github.com/randlee/agent-team-mail/pull/481) |
| | AC.6 | Hook Install Confidence + Multi-Team Recovery Matrix | COMPLETE | [#485](https://github.com/randlee/agent-team-mail/pull/485) |
| | AC.7 | Hook Lifecycle Coverage + Restart Recovery Convergence | COMPLETE | [#486](https://github.com/randlee/agent-team-mail/pull/486) |
| | AC.8 | Init Install Matrix QA Blocker Closure | COMPLETE | [#487](https://github.com/randlee/agent-team-mail/pull/487) |
| | AC.9 | Multi-Team Recovery Determinism | COMPLETE | [#488](https://github.com/randlee/agent-team-mail/pull/488) |
| | AC.10 | Final AC Verification + Release Readiness | COMPLETE | [#489](https://github.com/randlee/agent-team-mail/pull/489) |
| **AD** | AD.1 | Python Runtime Policy + atm init Auto-Install | COMPLETE | [#513](https://github.com/randlee/agent-team-mail/pull/513) |
| | AD.2 | Runtime Config Discovery Parity | COMPLETE | [#514](https://github.com/randlee/agent-team-mail/pull/514) |
| | AD.3 | GH Monitor Status Hardening | COMPLETE | [#515](https://github.com/randlee/agent-team-mail/pull/515) |
| | AD.4 | Live State + Config Reload | COMPLETE | [#516](https://github.com/randlee/agent-team-mail/pull/516) |
| | AD.5 | Script Conversion + atm init Auto-Install | COMPLETE | [#517](https://github.com/randlee/agent-team-mail/pull/517) |
| **AE** | AE.1 | Config Discovery + `atm gh init` Baseline | COMPLETE | [#518](https://github.com/randlee/agent-team-mail/pull/518) |
| | AE.2 | Live Status + JSON + Output Consistency | COMPLETE | [#519](https://github.com/randlee/agent-team-mail/pull/519) |
| | AE.3 | Monitor Reload Semantics | COMPLETE | [#521](https://github.com/randlee/agent-team-mail/pull/521) |
| | AE.4 | Daemon Logging/Autostart/Plugin Isolation | COMPLETE | [#522](https://github.com/randlee/agent-team-mail/pull/522) |
| | AE.5 | Identity Ambiguity + Phase Closeout | COMPLETE | [#523](https://github.com/randlee/agent-team-mail/pull/523) |
| **AF** | AF.1 | Lifecycle Correctness (Session + PID Liveness) | COMPLETE | [#524](https://github.com/randlee/agent-team-mail/pull/524) |
| | AF.2 | Spawn Authorization + Preview UX | COMPLETE | [#526](https://github.com/randlee/agent-team-mail/pull/526) |
| | AF.3 | Transient Agent Registration Controls | COMPLETE | [#527](https://github.com/randlee/agent-team-mail/pull/527) |
| | AF.4 | Cleanup Preview + tmux Sentinel | COMPLETE | [#528](https://github.com/randlee/agent-team-mail/pull/528) |
| | AF.5 | Reliability Regression + Documentation Closure | COMPLETE | [#529](https://github.com/randlee/agent-team-mail/pull/529) |
| **AG** | AG.0 | Stale Daemon Hygiene + CI Recovery | COMPLETE | [#540](https://github.com/randlee/agent-team-mail/pull/540) |
| | AG.1 | `sc-composer` Library MVP | COMPLETE | [#547](https://github.com/randlee/agent-team-mail/pull/547) |
| | AG.2 | Resolver + Include Expansion Hardening | COMPLETE | [#551](https://github.com/randlee/agent-team-mail/pull/551) |
| | AG.3 | `sc-compose` Binary + Logging Baseline | COMPLETE | [#552](https://github.com/randlee/agent-team-mail/pull/552) |
| | AG.4 | ATM Spawn Integration (`--system-prompt .j2`) | COMPLETE | [#553](https://github.com/randlee/agent-team-mail/pull/553) |

**Completed**: 133+ sprints across 29 phases (CI green)
**Current version**: v0.42.0
**Current planning phase**: Phase AJ
**Next planned phase**: Phase AK (mandatory OTel rollout)

---

## 17.17 Phase AH: Observability Unification + AG Deferred Closure (Historical)

_Historical record: AH delivered logging unification baseline. OTel/scmux/schook
rollout is deferred and planned in AJ/AK._

**Goal**: Extract `sc-observability` as a shared logging platform across ATM
tools and close deferred AG observability/render/docs gaps.

**Planning doc**: `docs/phase-ah-planning.md`
**Requirements doc**: `docs/observability/requirements.md`
**Architecture doc**: `docs/observability/architecture.md`

### Planned Sprint Map
| Sprint | Focus | Issues | Status |
|---|---|---|---|
| AH.1 | Shared crate foundation (`sc-observability`) + spool/size-guard/socket-error/L1a contracts | #556 | COMPLETE |
| AH.2 | `sc-compose` migration to shared logging | #556 | COMPLETE |
| AH.3 | Diagnostics + output derivation closure | #555, #557 | COMPLETE |
| AH.4 | ATM/daemon/tui/mcp integration + doctor/status health surfaces | #556 | COMPLETE |
| AH.5 | Runbook + install/release docs closeout | #558 | COMPLETE |

---

## 17.18 Phase AI: GH Monitor Reporting Surfaces

**Goal**: Add GH monitor dashboard/report UX for PR triage without expanding AH
observability scope.

**Planning doc**: `docs/phase-ai-planning.md`

### Planned Sprint Map
| Sprint | Focus | Issues | Status |
|---|---|---|---|
| AI.0 | `gh_monitor` cold-start init bug fix prerequisite | #564 | COMPLETE |
| AI.1 | `atm gh pr list` rollup dashboard + `--json` | #560 | COMPLETE |
| AI.2 | `atm gh pr report <PR>` built-in report + `--json` | #561 | COMPLETE |
| AI.3 | Template customization (`--template`) + optional `init-report` | #561 (follow-up) | COMPLETE |
| AI.4 | Report semantics hardening (`skip` pass semantics, review none, mergeability retry, blocker/advisory split) | #582 | COMPLETE |

---

## 17.19 Phase AJ: Session-ID SSoT Normalization

**Goal**: Make daemon registry the canonical session authority and eliminate
identity/session ambiguity by standardizing on `session_id` across ATM surfaces.
**Prerequisites**: Phase AH baseline complete.

**Planning doc**: `docs/phase-aj-planning.md`  
**Test plan**: `docs/test-plan-phase-AJ.md`

### Planned Sprint Map
| Sprint | Focus | Primary Issues | Status |
|---|---|---|---|
| AJ.1 | Shared resolver SSoT for `send/read/register/doctor` | #593, #595 | PLANNED |
| AJ.2 | Codex/Gemini runtime session resolution closure | #597 | PLANNED |
| AJ.3 | Stale session lifecycle + cleanup reliability | #594 | PLANNED |
| AJ.4 | Doctor/members session display consistency | #596 | PLANNED |
| AJ.5 | Spawn env normalization + resume/continue semantics | #593, #597 | PLANNED |

---

## 17.20 Phase AK: Mandatory OTel Rollout

**Goal**: Ship non-optional OpenTelemetry across in-scope tools while keeping
local structured logging always-on and fail-open.
**Prerequisites**: Phase AH and Phase AJ complete.

**Planning doc**: `docs/phase-ak-planning.md`  
**Requirements**: `docs/observability/requirements.md`  
**Architecture**: `docs/observability/architecture.md`  
**Test plan**: `docs/test-plan-phase-AK.md`

### Planned Sprint Map
| Sprint | Focus | Primary Issues | Status |
|---|---|---|---|
| AK.1 | Contract reconciliation + schema hardening (`trace_id/span_id/subagent_id`, paths, health JSON keys) | ATM-QA-004, ATM-QA-008, ATM-QA-007, ATM-QA-009 | PLANNED |
| AK.2 | `sc-observability` mandatory OTel core (`default-on`, retry/fail-open, correlation contract) | OTel baseline | PLANNED |
| AK.3 | Producer integration (`atm`, `atm-daemon`, `atm-tui`, `atm-agent-mcp`, `scmux`, `schook`, `sc-compose`, `sc-composer`) | OTel rollout | PLANNED |
| AK.4 | Doctor/status observability health + runbook finalization | health/reporting | PLANNED |
| AK.5 | End-to-end QA, release gates, and cross-platform validation | release confidence | PLANNED |

---

## 17.21 Phase AM: CI Monitor Subsystem Refactor

**Goal**: Refactor CI monitoring into a dedicated daemon subsystem so
`socket.rs` remains transport glue while provider-neutral orchestration,
GitHub-specific adapter logic, routing, and health handling live under
`plugins/ci_monitor`.
**Prerequisites**: Phase AI baseline complete.

**Integration branch**: `integrate/phase-AM`
**Planning doc**: `docs/phase-am-planning.md`

### Planned Sprint Map
| Sprint | Focus | Primary Branch | Status |
|---|---|---|---|
| AM.1 | Extract CI domain types, helpers, and shared test support | `feature/pAM-s1-extract-ci-types` | MERGED |
| AM.2 | Introduce CI monitor service layer and thin socket dispatch | `feature/pAM-s2-ci-monitor-service` | IN-PROGRESS |
| AM.3 | Split provider-neutral logic from GitHub adapter | `feature/pAM-s3-provider-split` | IN-PROGRESS |
| AM.4 | Extract routing and notification policy | `feature/pAM-s4-routing-split` | IN-PROGRESS |
| AM.5 | Extract health and availability state handling | `feature/pAM-s5-health-state` | IN-PROGRESS |
| AM.6 | Thin `socket.rs` and reorganize subsystem tests | `feature/pAM-s6-thin-socket` | IN-PROGRESS |

### Exit Criteria
1. `socket.rs` dispatches CI-monitor requests instead of owning CI-monitor policy.
2. CI-monitor business logic lives under `crates/atm-daemon/src/plugins/ci_monitor/`.
3. GitHub-specific logic is isolated behind one clear adapter boundary.
4. Subsystem tests are organized around CI-monitor modules instead of socket-only entrypoints.

---

## 17.22 Phase AN: CI Monitor Extraction Readiness

**Goal**: Prepare CI monitor code for clean extraction by tightening daemon/core
boundaries, narrowing the production module surface, moving reusable logic into
the extracted crate boundary, and landing the multi-repo `atm gh` contract.

**Integration branch**: `integrate/phase-AN`

### Planned Sprint Map
| Sprint | Focus | Primary Branch | Status |
|---|---|---|---|
| AN.1 | CI core boundary cleanup | `feature/pAN-s1-ci-core-boundary` | COMPLETE |
| AN.2 | Service split from daemon wire types | `feature/pAN-s2-service-split` | COMPLETE |
| AN.3 | Trait injection for provider/registry seams | `feature/pAN-s3-trait-injection` | COMPLETE |
| AN.4 | Narrow production `mod.rs` surface | `feature/pAN-s4-mod-narrowing` | IN-PROGRESS |
| AN.5 | Transport adapter boundary in `gh_monitor_router` | `feature/pAN-s5-plugin-init-split` | IN-PROGRESS |
| AN.6 | Extract `agent-team-mail-ci-monitor` crate | `feature/pAN-s6-crate-extraction` | IN-PROGRESS |
| AN.7 | Multi-repo `atm gh` routing and repo inference | `feature/pAN-s7-multi-repo-gh` | IN-PROGRESS |
| AN.8 | Phase AO guardrail planning and requirements closure | `feature/pAN-s8-gh-monitor-guardrails` | IN-PROGRESS |

### Exit Criteria
1. Reusable CI monitor logic is isolated behind crate-friendly boundaries.
2. Daemon-only transport and lifecycle adapters remain in `atm-daemon`.
3. The extracted `agent-team-mail-ci-monitor` crate owns the shared CI-monitor
   core surface.
4. Multi-repo `atm gh` routing is stable and Phase AO planning is ready to
   begin.

## 17.23 Phase AO: GH Monitor Guardrails + Runtime Admission

**Goal**: Prevent accidental shared-runtime pollers, make isolated test runtimes
explicit and short-lived, and make GitHub usage attributable, budgeted, and
operator-controllable.

**Prerequisites**: Phase AN merged to `develop`.
**Integration branch**: `integrate/phase-AO`

**Planning doc**: `docs/phase-ao-gh-monitor-guardrails.md`
**Requirements authority**:
- `docs/ci-monitoring/requirements.md`
- `docs/ci-monitoring/architecture.md`

### Planned Sprint Map
| Sprint | Focus | Primary Branch | Status |
|---|---|---|---|
| AO.1 | Shared runtime admission guard (`release`/`dev` only, hard-stop invalid shared launches) | `feature/pAO-s1-runtime-admission` | COMPLETE |
| AO.2 | Explicit isolated runtime creation + 10-minute TTL cleanup policy | `feature/pAO-s2-isolated-runtime-ttl` | ACTIVE |
| AO.3 | Shared repo-state cache, single `(team, repo)` shared poller, PR-list primary poll surface, bounded poll cadence, team budgets (`100/hour`), attributed `run_gh()` path, merge-conflict checks, and config/init parity | `feature/pAO-s3-repo-state-budget-observability` | ACTIVE |
| AO.4 | Single `(team, repo)` lease ownership + hidden human-authorized cross-team stop/disable path with operator-facing owner metadata | `feature/pAO-s4-operator-control` | ACTIVE |
| AO.5 | Post-integration deletion sprint: simplify runtime/poller paths and narrow final contracts | `feature/pAO-s5-path-contract-simplification` | ACTIVE |

### Exit Criteria
1. Shared `release` and `dev` runtimes reject invalid owners and duplicate daemon starts.
2. Isolated runtimes are explicit, short-lived, and do not enable live GH polling by default.
3. GitHub calls are budgeted per team, counted locally, and surfaced with freshness metadata in `atm gh status` and `atm doctor`; one shared `(team, repo)` poller uses the repo-wide PR list view as its primary poll surface, polling at most once per 5 minutes when idle and once per 1 minute when active (`GH-CI-FR-10a`, `GH-CI-FR-10b`, `GH-CI-FR-10c`); pre-run/post-completion merge-conflict checks plus config/init parity remain on the attributed `run_gh()` path.
4. One active `gh_monitor` owner exists per `(team, repo)`, operator-facing status shows the active owner metadata, and operators can stop a runaway monitor with auditable cross-team controls.
5. Transitional runtime, polling, and state paths preserved during AO are removed or narrowed so the post-AO implementation exposes only the canonical contracts.

**Execution note**: AO.3, AO.4, and AO.5 were authorized to execute in parallel with merge-forwards between sprint branches as fixes landed. Phase exit still requires the combined AO surface plus AO.5 simplification criteria.

**Dependency graph**: AO.1 → AO.2 → {AO.3, AO.4, AO.5 with merge-forward discipline}

---

## 17.24 Phase AP: Test Stability and Harness Hardening

**Goal**: eliminate hang-prone, flaky, and operationally unsafe test patterns
that can block CI without clearly identifying the failing test or resource.

**Prerequisites**: Phase AN merged to `develop`; Phase AO may proceed in
parallel, but AP.1 should start before new daemon-heavy test coverage expands.
**Integration branch**: `integrate/phase-AP`

**Planning doc**: `docs/phase-ap-test-hardening.md`

### Planned Sprint Map
| Sprint | Focus | Primary Branch | Status |
|---|---|---|---|
| AP.1 | Environment/process safety: scoped `ATM_HOME`, subprocess RAII, autostart diagnostics | `feature/pAP-s1-process-safety` | PLANNED |
| AP.2 | Deterministic timing: replace wall-clock sleeps, bound loop/watcher waits, improve test attribution | `feature/pAP-s2-deterministic-timing` | PLANNED |
| AP.3 | Pathing/serialization cleanup and final audit | `feature/pAP-s3-test-hygiene-audit` | PLANNED |

### Exit Criteria
1. Blocking/high-priority tests no longer rely on raw wall-clock sleeps as their
   sole synchronization mechanism.
2. Test helpers and integration tests do not leak daemon/subprocess children on
   panic.
3. Shared mutable environment state (`ATM_HOME`, shared runtime paths) is scoped
   safely or serialized explicitly.
4. Cross-platform fixtures avoid hardcoded `/tmp`, and risky integration suites
   fail with bounded, attributable diagnostics instead of hanging silently.
5. Duplicate low-value tests and ad hoc helper paths are removed where they do
   not provide unique coverage, leaving a smaller canonical harness.

**Dependency graph**: AP.1 → AP.2 → AP.3

---

## 17.11 Phase Z: Daemon SSoT + Observability Hardening

**Goal**: Close daemon single-source-of-truth gaps for member/session state and make
doctor/log observability reliable and diagnosable from structured events.

**Integration branch**: `integrate/phase-Z`

**Dependency graph**:
- Z.1 and Z.2 start in parallel.
- Z.3 depends on Z.1 (register-hint + spawn metadata contract).
- Z.4 depends on Z.3 (canonical union completion on top of fast path).
- Z.5 is independent and can run in parallel with Z.3/Z.4.

### Sprint Summary
| Sprint | Name | PR | Branch | Issues | Status |
|--------|------|----|--------|--------|--------|
| Z.1 | Quick Wins: Doctor + Release Fix | [#423](https://github.com/randlee/agent-team-mail/pull/423) | `feature/pZ-s1-quick-wins` | #407, #408, #403, #399 | COMPLETE |
| Z.2 | Log Format + Doctor UX | [#425](https://github.com/randlee/agent-team-mail/pull/425) | `feature/pZ-s2-log-format` | #410, #411, #412, #419 | COMPLETE |
| Z.3 | SSoT Fast Path | [#427](https://github.com/randlee/agent-team-mail/pull/427) | `feature/pZ-s3-ssot-fast-path` | #413, #415, #409 | COMPLETE |
| Z.4 | Canonical Member State Completion | [#429](https://github.com/randlee/agent-team-mail/pull/429) | `feature/pZ-s4-canonical-state` | #414, #416, #417, #418, #401, #402 | COMPLETE |
| Z.5 | Lifecycle Logging + Hook Events | [#430](https://github.com/randlee/agent-team-mail/pull/430) | `feature/pZ-s5-observability` | #420, #421 | COMPLETE |
| Z.6 | Cross-folder Spawn + QA Blocker Closure | [#431](https://github.com/randlee/agent-team-mail/pull/431) | `feature/pZ-s6-cross-folder-spawn` | #422, #424, #426, #428 | COMPLETE |
| Z.7 | Review Findings Hardening | [#432](https://github.com/randlee/agent-team-mail/pull/432), [#433](https://github.com/randlee/agent-team-mail/pull/433) | `feature/pZ-s7-review-hardening` | QA findings closure | COMPLETE |

### Z.1 — Quick Wins: Doctor + Release Fix
**Deliverables**
1. #407: treat sysinfo process lookup `None` as inconclusive (not mismatch) in PID/backend validation.
2. #408: short session-id formatting in doctor/member surfaces.
3. #403: prevent reconcile pass from re-overwriting mismatch-offline states.
4. #399: migrate release checks from fragile `curl` calls to resilient crates verification path.

**Acceptance Criteria**
1. Doctor no longer emits false Offline/PID mismatch from missing sysinfo process metadata.
2. Session identifiers in doctor output are compact and human-parseable.
3. Mismatch-offline transitions remain sticky until valid re-registration.
4. Release verification paths avoid Cloudflare/IP-based transient failures.

### Z.2 — Log Format + Doctor UX
**Deliverables**
1. #410/#411: normalize send log identity fields and sender->recipient formatting.
2. #412: `ATM_LOG_MSG` binary contract (`1` enables preview, unset/other disables).
3. #419: doctor log-window labeling with relative elapsed semantics.

**Acceptance Criteria**
1. Send log records always include sender/recipient identity + PID fields.
2. Message preview appears only when `ATM_LOG_MSG=1`.
3. Doctor log-window text and JSON fields agree on effective time window semantics.

### Z.3 — SSoT Fast Path
**Deliverables**
1. #413: add `register-hint` daemon command path for external runtime/session registration.
2. #415: remove send-path bypasses and route session truth through daemon.
3. #409: `atm teams spawn` writes model/backend metadata before launch when daemon-backed path is active.

**Acceptance Criteria**
1. send/spawn command paths do not infer liveness independently of daemon canonical state.
2. `register-hint` supports backward-compat handling (daemon unavailable skip, unknown command guidance).
3. Spawn preserves daemon-unavailable UX while keeping metadata writes fail-open and model-safe.

### Z.4 — Canonical Member State Completion
**Deliverables**
1. #414/#416: align register/teams/send code paths to daemon-owned canonical state.
2. #417/#418: team-scoped union roster query (config members + daemon-only sessions) with cold-start PID-hint bootstrap.
3. #401/#402: strict PID validation parity between send path and daemon validator.

**Acceptance Criteria**
1. `atm doctor`, `atm status`, and `atm members` read from the same canonical daemon snapshot.
2. Daemon-only members are surfaced with explicit unregistered/ghost markers.
3. PID/backend mismatch handling is consistent across lifecycle, send, and diagnostics.

### Z.5 — Lifecycle Logging + Hook Events
**Deliverables**
1. #420: lifecycle transition logging (`member_state_change`, `member_activity_change`,
   `session_id_change`, `process_id_change`) with reason/source fields.
2. #421: first-class hook lifecycle logs (`hook.session_start`,
   `hook.pre_compact`, `hook.compact_complete`, `hook.session_end`, `hook.failure`).
3. Requirements alignment for event coverage and always-on hook observability semantics.

**Acceptance Criteria**
1. Offline<->Online transitions always emit INFO events exactly once per change on Unix builds (`#[cfg(unix)]` scope).
2. Busy<->Idle transitions emit DEBUG-only events (no INFO spam).
3. Hook lifecycle events are always emitted with consistent structured fields and WARN failure records.
4. Doctor findings are diagnosable directly from structured logs without ad-hoc inference.

### Z.7 Deferred Technical Debt Carry-Forward

The following Z.7 deliverables (d8–d12) were deferred due to scope and are tracked as next-phase debt:

- **d8**: `ATM_TEAM` env precedence test (non-mismatch path) — ATM-ZQA-001 test added to cover the fix; full precedence matrix coverage deferred
- **d9–d12**: Advanced mismatch-override audit trails, cross-folder spawn edge cases, additional liveness-check hardening, and team-scoped register-lead conflict resolution

These items should be addressed in the next hardening phase.

## 17.12 Phase AA: Session Correctness + Spawn Authorization + Reliability UX

**Goal**: close session-end correctness gaps, enforce leaders-only spawn policy,
and harden release/test reliability and operator UX.

**Integration branch**: `integrate/phase-AA` (created from `planning/next-phase`)

**Branching note**:
- `integrate/phase-AA` was created from `planning/next-phase` for planning review.
- Before sprint implementation begins, planning changes must be merged to
  `develop`; sprint branches then follow normal policy (branch from develop).

**Dependency graph**:
- AA.1 is foundational for session-end correctness.
- AA.2 is intentionally config-driven and independent of AA.1 (does not depend
  on daemon session state for authorization decisions).
- AA.3 is independent.
- AA.4 is independent and can run in parallel with AA.1–AA.3.
- AA follow-on for #449 depends on AA.1 completion and post-AA.1 assessment.

### Sprint Summary
| Sprint | Name | PR | Branch | Issues | Status |
|--------|------|----|--------|--------|--------|
| AA.1 | Session-End Correctness Hardening | [#453](https://github.com/randlee/agent-team-mail/pull/453) | `feature/pAA-s1-session-end-correctness` | [#448](https://github.com/randlee/agent-team-mail/issues/448) | COMPLETE |
| AA.2 | Spawn Authorization Gate Alignment | [#455](https://github.com/randlee/agent-team-mail/pull/455) | `feature/pAA-s2-spawn-authorization` | [#394](https://github.com/randlee/agent-team-mail/issues/394) | COMPLETE |
| AA.3 | CI/Release Reliability Closure | [#454](https://github.com/randlee/agent-team-mail/pull/454) | `feature/pAA-s3-reliability-closure` | [#372](https://github.com/randlee/agent-team-mail/issues/372) | COMPLETE |
| AA.4 | Cleanup + Spawn Help UX Polish | [#457](https://github.com/randlee/agent-team-mail/pull/457) | `feature/pAA-s4-ux-polish` | [#373](https://github.com/randlee/agent-team-mail/issues/373), [#424](https://github.com/randlee/agent-team-mail/issues/424) | COMPLETE |

### AA.1 — Session-End Correctness Hardening
**Deliverables**
1. Define/implement no-record `session_end` handling: no mutation, DEBUG log,
   no tombstone/session-row creation.
2. Define/implement duplicate dead `session_end` replay as strict no-op with no
   teardown/cleanup/reconcile retrigger.
3. Keep mismatch protection: mismatched session_id must not dead-mark current
   record; warning must include expected vs received session id.
4. Keep dead+alive mismatch rule: no auto-promote without explicit re-registration.

**Acceptance Criteria**
1. Sending `session_end` for unknown `(team, agent, session_id)` does not create
   records or mutate state; DEBUG event is emitted.
2. Replaying identical `session_end` for already-dead record is idempotent no-op.
3. Mismatched-session `session_end` leaves active record unchanged and emits
   structured warning fields: `team`, `agent`, `expected_session_id`,
   `current_session_id`, `received_session_id`.
4. `Dead + alive PID + mismatch` remains dead until `session_start`/`register-hint`.

### AA.2 — Spawn Authorization Gate Alignment
**Deliverables**
1. CLI path: `atm teams spawn` validates caller identity against
   `.atm.toml` `spawn_policy` (`leaders-only`) + `co_leaders`.
2. Hook path: `PreToolUse` gate in `.claude/settings.json` (matcher `Task`,
   command invoking `.claude/scripts/gate-agent-spawns.py`) aligns with the same
   `spawn_policy` contract and `SPAWN_UNAUTHORIZED` behavior.
3. Tests: leaders-only pass (`team-lead`), leaders-only pass (`co_leader`),
   leaders-only fail (other), and missing `[team.<name>]` fails non-lead with
   `SPAWN_UNAUTHORIZED` (not config parse error).
4. Documentation: `SPAWN_UNAUTHORIZED` semantics and guidance are explicit.

**Acceptance Criteria**
1. Authorized leader/co-leader can spawn without policy error.
2. Unauthorized caller receives `SPAWN_UNAUTHORIZED` before side effects.
3. Missing `[team.<name>]` defaults to team-lead-only and fails non-lead callers.
4. CLI and hook gate outcomes match for equivalent caller/team inputs.

### AA.3 — CI/Release Reliability Closure
**Deliverables**
0. Root-cause gate on [#372](https://github.com/randlee/agent-team-mail/issues/372):
   comment root-cause determination before mitigation path is selected.
1. Redesign flaky concurrency coverage into deterministic tests with explicit
   timeout + teardown guardrails for spawned daemons.
2. If root cause is production data-loss, ship production fix and keep coverage
   active (no ignore path).
3. If root cause is timing/harness-only, redesign test harness/process control
   to remain deterministic on macOS/Linux/Windows without suppression.
4. Add post-publish verify retry/backoff in `release.yml` (`cargo search`):
   5 attempts, 60s intervals, structured retry logs, terminal failure with full
   crate list.

**Acceptance Criteria**
1. #372 root-cause comment exists before AA.3 merge decision.
2. Concurrency/reliability tests are bounded (no unbounded hang) on CI.
3. `test_concurrent_sends_no_data_loss` (or replacement coverage) passes on
   macOS, Linux, and Windows without platform-specific skip/ignore.
4. Release verification fails only after 5 retries and reports all failed crates.

### AA.4 — Cleanup + Spawn Help UX Polish
**Deliverables**
1. `atm teams cleanup --dry-run` outputs non-mutating candidate-action preview
   table with per-row reason and action totals.
2. Empty dry-run output is explicit and concise.
3. `atm teams spawn --help` adds generated launch-command block for
   claude/codex/gemini with token-substituted placeholders when config absent.
4. Help output remains static/safe and never fails on missing `.atm.toml`.

**Acceptance Criteria**
1. Non-empty dry-run shows preview table + totals and exits 0 with no writes.
2. Empty dry-run prints `Nothing to clean up for team <name>.` and no table header.
3. `spawn --help` succeeds without `.atm.toml` and uses meaningful placeholders.

### AA.D1 Design Discussion (Not Yet a Sprint): Named Agent Lifecycle + Mailbox Archival

Recommended direction:
1. Use explicit `agent_type = "task" | "persistent"` metadata (preferred over
   inferred classification for auditability and deterministic cleanup).
2. Default archive semantics: retain mailbox + mark inactive + TTL cleanup.
3. Hard-delete mailbox is permissible only when mailbox is empty, all messages
   are read, or explicit operator acknowledgment is provided.
4. Trigger model should converge lifecycle (`session_end`, shutdown_approved)
   and operator actions (`teams cleanup`, future `teams archive`).
5. Shutdown convergence should be daemon-driven for both CLI shutdown paths
   (`atm shutdown`/kill flows) and Claude hook lifecycle (`session_end`), with
   policy controls designed as config/flag knobs (for example
   `auto_cleanup_on_shutdown`, optional cleanup delay).

Unblock requirement:
- AA.2 should establish spawn metadata schema ownership for `agent_type` so
  lifecycle policy can be enforced in a later sprint.

Follow-on note for #449:
- Pick up after AA.1 merges and PID/session behavior is re-verified.
- If AA.1 exposes additional PID freshness risk, promote #449 to AA.5;
  otherwise keep as deferred `0.37/0.38` follow-on.

## 17.13 Phase AB: GitHub CI Monitor Command + Availability Hardening

**Goal**: finalize and implement a reliable GitHub CI monitor plugin contract with
daemon-safe lifecycle behavior, actionable `atm gh` command UX, and complete
progress/failure observability.

**Requirements references**:
- `docs/ci-monitoring/requirements.md`
- `docs/requirements.md` §4.11 and §5.8–§5.10

**Integration branch**: `integrate/phase-AB` (planned)

**Dependency graph**:
- AB.1 defines hard contracts and must land first.
- AB.2 depends on AB.1 command ownership/daemon routing contracts.
- AB.3 depends on AB.2 monitor command baseline.
- AB.4 depends on AB.1 and can run in parallel with AB.2/AB.3 after state
  contracts are finalized.
- AB.5 is optional enhancement after AB.3 + AB.4.

### Sprint Summary
| Sprint | Name | PR | Branch | Issues | Status |
|--------|------|----|--------|--------|--------|
| AB.1 | Requirements Lock + Core Plugin Contracts | [#462](https://github.com/randlee/agent-team-mail/pull/462) | `feature/pAB-s1-requirements-lock` | — | COMPLETE |
| AB.2 | `atm gh monitor` Command Surface | [#463](https://github.com/randlee/agent-team-mail/pull/463) | `feature/pAB-s2-gh-monitor-command` | — | COMPLETE |
| AB.3 | Progress + Final Reporting Payloads | [#464](https://github.com/randlee/agent-team-mail/pull/464) | `feature/pAB-s3-reporting-contract` | — | COMPLETE |
| AB.4 | Availability State + Connectivity Recovery Signals | [#465](https://github.com/randlee/agent-team-mail/pull/465) | `feature/pAB-s4-availability-state` | — | COMPLETE |
| AB.5 | Runtime Drift Baselines (Optional Enhancement) | [#466](https://github.com/randlee/agent-team-mail/pull/466) | `feature/pAB-s5-runtime-drift` | — | COMPLETE |
| AB.6 | PR Merge-Conflict + CI Gap Detection | [#467](https://github.com/randlee/agent-team-mail/pull/467) | `feature/pAB-s6-conflict-detection` | — | COMPLETE |
| AB.7 | Architecture Review Findings Hardening | [#468](https://github.com/randlee/agent-team-mail/pull/468) | `feature/pAB-s7-arch-findings` | — | COMPLETE |

### AB.1 — Requirements Lock + Core Plugin Contracts
**Deliverables**
1. Lock naming split: shared `ci_monitor` contract + concrete `gh_monitor` plugin key + namespace ownership (`atm gh`).
2. Lock plugin failure-isolation contract (plugin failure must not crash daemon).
3. Lock availability-state contract (`healthy`, `degraded`, `disabled_config_error`).
4. Lock global plugin command-gating contract (`<namespace>`, `<namespace> init`, help only when plugin is not configured/enabled for current team).
5. Publish finalized requirements references for implementation/QA traceability.

**Acceptance Criteria**
1. Core requirements and plugin requirements are internally consistent.
2. No key-name ambiguity remains in docs/examples (`gh_monitor` concrete key, `ci_monitor` shared contract).
3. Daemon/plugin boundary requirements are explicit and testable.

### AB.2 — `atm gh monitor` Command Surface
**Deliverables**
1. Implement command forms for PR/workflow/run monitoring and status checks.
2. Implement no-target status UX for plugin health:
   - `atm gh` (namespace status)
   - `atm gh status` (health/availability status)
   with explicit configured/enabled/availability output.
3. Implement PR start-timeout behavior (`2m` default, override allowed).
4. Emit actionable no-run-started alerts to designated monitor recipients.

**Acceptance Criteria**
1. `atm gh monitor pr <n>` reports `ci_not_started` when timeout expires with no run.
2. Workflow and run monitor commands resolve and track expected run targets.
3. `atm gh` and `atm gh status` (no target) provide actionable non-error plugin status output.
4. Coverage maps to `GH-CI-TR-2` in `docs/ci-monitoring/requirements.md`.

### AB.3 — Progress + Final Reporting Payloads
**Deliverables**
1. Enforce progress cadence (<= 1/minute) while preserving immediate terminal update.
2. Emit final summary table (job/test, status, runtime).
3. Enforce failure payload fields (run/job/PR URLs + metadata contract).

**Acceptance Criteria**
1. Progress is rate-limited under active monitoring.
2. Terminal completion/failure message is immediate and complete.
3. Failure notifications include required URLs and identifying metadata.
4. Coverage maps to `GH-CI-TR-3` in `docs/ci-monitoring/requirements.md`.

### AB.4 — Availability State + Connectivity Recovery Signals
**Deliverables**
1. Implement and expose state transitions (`healthy`, `degraded`, `disabled_config_error`).
2. On connectivity/auth/rate-limit/provider failure, emit structured logs and ATM alerts.
3. On recovery, emit structured logs and ATM recovery alerts.
4. Ensure invalid config disables polling loop (zero steady-state polling CPU).

**Acceptance Criteria**
1. Transition events are visible in logs and ATM mail.
2. Invalid configuration does not run polling and status is visible in
   `atm status` / `atm doctor`.
3. Transient failures do not crash daemon.
4. Coverage maps to `GH-CI-TR-1` in `docs/ci-monitoring/requirements.md`.
5. Coverage maps to `GH-CI-TR-4` in `docs/ci-monitoring/requirements.md`.

### AB.5 — Runtime Drift Baselines (Optional Enhancement)
**Deliverables**
1. Persist runtime history for workflows/jobs.
2. Compute baseline and alert on significant drift.
3. Expose drift threshold policy as config.

**Acceptance Criteria**
1. Drift alert can be reproduced in deterministic integration tests.
2. Baseline calculations are stable across restarts.
3. Coverage maps to `GH-CI-TR-5` in `docs/ci-monitoring/requirements.md`.

### AB.6 — PR Merge-Conflict + CI Gap Detection
**Deliverables**
1. Pre-run preflight: before starting CI polling, check PR `mergeStateStatus`. If `DIRTY`, emit a `merge_conflict` alert (skip `ci_not_started`), do not start polling loop.
2. Post-CI-completion check: after a run reaches terminal state, re-check `mergeStateStatus`. If `DIRTY`, emit an additional merge-conflict alert alongside the CI result.

**Acceptance Criteria**
1. Post-completion merge-conflict alert emitted when PR becomes DIRTY during a CI run.
2. Pre-run merge-conflict alert emitted when PR is DIRTY before any run starts (distinct message from `ci_not_started`).
3. Coverage maps to GH-CI-TR-2 and GH-CI-TR-4 in `docs/ci-monitoring/requirements.md`.

### AB.7 — Architecture Review Findings Hardening
**Deliverables**
1. Fix PR start detection: scope `wait_for_pr_run_start` to PR number association + recency gate so stale/unrelated branch runs cannot be selected (GH-CI-FR-17 gap, socket.rs:2271-2306).
2. Fix workflow status ambiguity: pass `ref` parameter in `gh status` workflow lookup (socket.rs:2209-2224) to prevent nondeterministic results with parallel refs.
3. Fix classification schema drift: add `infra` class to `classify_failure` to match requirements.md:139 examples (socket.rs:2768-2781).
4. Fix duplicate failure notifications: add dedup check so polling-path `should_notify_failure` skips notification when command-path `monitor_gh_run` terminal handler already fired for the same run ID.

**Acceptance Criteria**
1. `wait_for_pr_run_start` queries by PR number; stale same-branch runs are excluded. Test: two runs on same branch, only the PR-associated one is selected.
2. `gh status` workflow lookup includes `ref` param. Test: parallel-ref scenario returns deterministic result.
3. `classify_failure` returns `infra` for infra-category failures. Test: infra error input → `infra` classification.
4. No double alert when both code paths fire for same run. Test: terminal command-path fires → polling-path suppressed.
5. All tests: isolated temp dirs, process_id=0 pattern.

---

## 17.14 Phase AC: Daemon Status Convergence + Hook Install Confidence

**Goal**: close remaining daemon status/lifecycle edge cases and validate hook behavior
for both project-local and global-install paths before Homebrew/global hook rollout.

**Requirements references**:
- `docs/requirements.md` §4.3.3c (daemon canonical member-state contract)
- `docs/requirements.md` §4.5 (hook lifecycle/event contracts)
- `docs/requirements.md` §4.7 (daemon autostart/single-instance guarantees)

**Integration branch**: `integrate/phase-AC`

**Dependency graph**:
- AC.5 established baseline daemon state/lifecycle correctness.
- AC.6 extends AC.5 with hook install confidence and parity coverage.
- AC.7 hardens lifecycle + restart convergence behavior.
- AC.8 closes init matrix QA blockers.
- AC.9 validates multi-team recovery determinism after merge-forward.
- AC.10 performs final AC verification and release-readiness closeout.

### Sprint Summary
| Sprint | Name | PR | Branch | Issues | Status |
|--------|------|----|--------|--------|--------|
| AC.1 | ReconcileCycleState Per-Test Injection | `41053cf` | integrate/phase-AC | daemon test isolation | COMPLETE |
| AC.1b | Codex PPID Detection + Stable Session Key | `da6cae5` | integrate/phase-AC | stable session key | COMPLETE |
| AC.2 | Cleanup Guard Tests + gh Monitor Repo Validation | [#476](https://github.com/randlee/agent-team-mail/pull/476), [#484](https://github.com/randlee/agent-team-mail/pull/484) | `feature/pAC-s2-cleanup-guard` | cleanup guard, gh_monitor config | COMPLETE |
| AC.3 | atm spawn Interactive UX | [#477](https://github.com/randlee/agent-team-mail/pull/477) | `feature/pAC-s3-spawn-ux` | spawn interactive panel, pane modes | COMPLETE |
| AC.4 | Daemon Logging Observability | [#479](https://github.com/randlee/agent-team-mail/pull/479) | `feature/pAC-s4-daemon-logging` | PRODUCER_TX, autostart stderr, plugin isolation | COMPLETE |
| AC.5 | Daemon Status Convergence + Lifecycle State Validation | [#481](https://github.com/randlee/agent-team-mail/pull/481) | `feature/pAC-s5-status-convergence` | #330, #331, #333, #334, #336 | COMPLETE |
| AC.6 | Hook Install Confidence + Multi-Team Recovery Matrix | [#485](https://github.com/randlee/agent-team-mail/pull/485) | `feature/pAC-s6-hook-install-confidence` | #357 follow-on hardening | COMPLETE |
| AC.7 | Hook Lifecycle Coverage + Restart Recovery Convergence | [#486](https://github.com/randlee/agent-team-mail/pull/486) | `feature/pAC-s7-hook-lifecycle-coverage` | lifecycle/restart hardening closure | COMPLETE |
| AC.8 | Init Install Matrix QA Blocker Closure | [#487](https://github.com/randlee/agent-team-mail/pull/487) | `feature/pAC-s8-init-install-matrix` | init onboarding contract | COMPLETE |
| AC.9 | Multi-Team Recovery Determinism | [#488](https://github.com/randlee/agent-team-mail/pull/488) | `feature/pAC-s9-multiteam-recovery` | team-scoped reload + restart determinism | COMPLETE |
| AC.10 | Final AC Verification + Release Readiness | [#489](https://github.com/randlee/agent-team-mail/pull/489) | `feature/pAC-s10-release-confidence` | final QA pass + release-closeout checklist | COMPLETE |

### AC.5 — Daemon Status Convergence + Lifecycle State Validation
**Deliverables**
1. Ensure `atm doctor`, `atm status`, and `atm members` consume one daemon canonical snapshot path per invocation; no command-specific liveness derivation drift.
2. Validate lifecycle transition handling across hook/event signals:
   `session_start`, `permission_request`, `stop`, `notification_idle_prompt`,
   `teammate_idle`, `session_end`.
3. Enforce no-op/idempotent behavior for invalid lifecycle replay paths
   (unknown `session_end`, duplicate dead `session_end`, mismatched-session `session_end`).
4. Re-verify that `isActive` remains activity-only and cannot be interpreted as liveness in diagnostics/status surfaces.
5. Preserve prior Z.5 hook lifecycle coverage for `hook.pre_compact` and
   `hook.compact_complete`; AC.5 does not redefine or regress those contracts.

**Acceptance Criteria**
1. The same member state is rendered consistently in `atm doctor`, `atm status`, and `atm members`.
2. Activity transitions (`Busy/Idle/Blocked`) do not flip liveness (`Online/Offline`) incorrectly.
3. Team-scoped diagnostics do not report cross-team tracked-agent drift.
4. Lifecycle replay/error paths are deterministic and produce actionable diagnostics/logs.

### AC.6 — Hook Install Confidence + Multi-Team Recovery Matrix
**Deliverables**
1. Hook artifact parity validation between repo-local scripts (`.claude/scripts`) and embedded install scripts (`crates/atm/scripts`) for:
   `session-start.py`, `session-end.py`, `permission-request-relay.py`,
   `stop-relay.py`, `notification-idle-relay.py`, `atm_hook_lib.py`.
2. `atm init` local/global hook installation confidence matrix:
   fresh install, idempotent re-run, command-path correctness, and absolute-path
   global script wiring.
3. Multi-team + daemon-restart recovery validation:
   team isolation, restart state rebuild, and no status drift after recovery.

**Acceptance Criteria**
1. Hook tests validate behavior from both script roots (local and embedded/global materialization source).
2. `atm init` hook installs are idempotent and route correctly in both scopes.
3. Daemon restart/recovery does not introduce cross-team bleed or status regressions.

### AC.10 — Final AC Verification + Release Readiness
**Deliverables**
1. Confirm `atm doctor` reports `DAEMON_NOT_RUNNING` with exit code 2 and `liveness: null`
   when daemon is unreachable, regardless of member `isActive` state.
2. Validate `docs/test-plan-phase-AC.md` Sprint Mapping covers AC.7–AC.10 with
   all AC.5 §1 acceptance cases audited.
3. Full AC regression guardrail: `atm doctor`, `atm status`, and `atm members`
   remain consistent under daemon-unreachable conditions.

**Acceptance Criteria**
1. `test_doctor_status_members_consistent_unknown_when_daemon_unreachable` passes
   on all platforms with `DAEMON_NOT_RUNNING` finding, exit code 2, and
   `liveness: null` for all members regardless of `isActive`.
2. `docs/test-plan-phase-AC.md` Sprint Mapping rows AC.7–AC.10 are present and
   reference the correct branches and deliverables.
3. `cargo test` and `cargo clippy -- -D warnings` clean across all crates.

---

## 17.15 Phase AD: Cross-Platform Script Standardization

**Goal**: remove product/runtime shell-script dependencies and standardize runtime
script behavior on Python across macOS/Linux/Windows.

**Requirements references**:
- `docs/requirements.md` §4.9.3a (Python-only runtime script policy)
- `docs/requirements.md` §4.9.5 (`atm init` runtime detection + install contract)

**Integration branch**: `integrate/phase-AD`

**Planning doc**: `docs/phase-ad-planning.md`

### Sprint Summary
| Sprint | Name | PR | Branch | Issues | Status |
|--------|------|----|--------|--------|--------|
| AD.1 | Python Runtime Policy + atm init Auto-Install | [#513](https://github.com/randlee/agent-team-mail/pull/513) | `feature/pAD-s1-python-policy` | #500 (→AE), #506 (→AE) | COMPLETE |
| AD.2 | Runtime Config Discovery Parity | [#514](https://github.com/randlee/agent-team-mail/pull/514) | `feature/pAD-s2-config-parity` | #499 (→AE) | COMPLETE |
| AD.3 | GH Monitor Status Hardening | [#515](https://github.com/randlee/agent-team-mail/pull/515) | `feature/pAD-s3-gh-status-hardening` | #504 (→AE), #507 | COMPLETE |
| AD.4 | Live State + Config Reload | [#516](https://github.com/randlee/agent-team-mail/pull/516) | `feature/pAD-s4-live-state` | #502 (→AE), #503 (→AE), #505 (→AE) | COMPLETE |
| AD.5 | Script Conversion + atm init Auto-Install | [#517](https://github.com/randlee/agent-team-mail/pull/517) | `feature/pAD-s5-script-conversion` | TBD | COMPLETE |
| AD.6 | Bash Wrapper Removal | — | `feature/pAD-s6-bash-removal` | — | CANDIDATE |

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
| Phase Q | `integrate/phase-Q` → [#262](https://github.com/randlee/agent-team-mail/pull/262) | Merged |
| Phase Y | `integrate/phase-Y` → [#396](https://github.com/randlee/agent-team-mail/pull/396) | Merged |
| Phase Z | `integrate/phase-Z` → [#436](https://github.com/randlee/agent-team-mail/pull/436) | Merged |
| Phase AA | `integrate/phase-AA` | Merged ([#459](https://github.com/randlee/agent-team-mail/pull/459)) |
| Phase AB | `integrate/phase-AB` | [#469](https://github.com/randlee/agent-team-mail/pull/469) Pending merge |
| Phase AD | `integrate/phase-AD` → [#520](https://github.com/randlee/agent-team-mail/pull/520) | Merged |
| Phase AE | `integrate/phase-AE` → [#525](https://github.com/randlee/agent-team-mail/pull/525) | Merged |
| Phase AF | `integrate/phase-AF` → [#530](https://github.com/randlee/agent-team-mail/pull/530) | Pending merge |

---

## 23. External Contributions

### 23.1 Erik's Session File Lifecycle Work (PR #428)

**Branch**: `origin/erik/atm-project-template-phase-1`
**Worktree**: `/Users/randlee/Documents/github/agent-team-mail-worktrees/erik/atm-project-template-phase-1`
**Status**: DRAFT — under review by atm-qa

Key commits:
- `01998f7` — session-end hook + session-start write (initial implementation)
- `1d2bc83` — SessionEnd hook hardening

**Adopted in PR #444** (`feature/session-file-lifecycle`):
- Session file lifecycle (SessionStart write, SessionEnd cleanup, atm-identity-write.py refresh, `read_session_file` in hook_identity.rs)
- **Deviations from Erik's implementation**:
  - Path changed to team-scoped: `~/.claude/teams/<team>/sessions/<session_id>.json` (Erik used `~/.claude/sessions/`) — per `docs/requirements.md §4.5 SessionStart — Session File`
  - `pid` field uses `os.getppid()` in `session-start.py` (Erik's version incorrectly used `os.getpid()`; corrected during integration per `docs/requirements.md §4.5` rule that PID must be the long-lived Claude session process PID, not the short-lived hook subprocess PID)

---

## 21. Open Issues Tracker

**WARNING**: All issues below are OPEN on GitHub. Do not mark as resolved without verifying the fix exists AND closing the GitHub issue.

| Issue | Description | Planned Sprint | Notes |
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
| [#361](https://github.com/randlee/agent-team-mail/issues/361) | Spawn path normalization (`--folder` canonical, `--cwd` compatibility) | X.2 | Canonical spawn-directory contract across runtimes |
| [#357](https://github.com/randlee/agent-team-mail/issues/357) | `atm init` full one-command setup + default global hooks | X.3 | One-command onboarding (`.atm.toml` + team + hooks) plus quickstart updates |
| [#448](https://github.com/randlee/agent-team-mail/issues/448) | `session_end` must be session-id scoped; reconcile stale dead/alive mismatch state | AA.1 | Correctness-first Phase AA sprint |
| [#449](https://github.com/randlee/agent-team-mail/issues/449) | PID liveness cache TTL, periodic re-probe, and `last_alive` in member state | AA follow-on (`0.37/0.38`) | Reassess after AA.1; promote to AA.5 if urgency increases |
| [#394](https://github.com/randlee/agent-team-mail/issues/394) | Gate terminal spawn to team-lead + co-leaders only | AA.2 | Config-driven authorization (no daemon-state dependency) |
| [#372](https://github.com/randlee/agent-team-mail/issues/372) | macOS CI hang in `test_concurrent_sends_no_data_loss` | AA.3 | Root-cause gate required before mitigation path |
| [#373](https://github.com/randlee/agent-team-mail/issues/373) | Add `--dry-run` preview mode to `atm teams cleanup` | AA.4 | Non-mutating table with reasons/totals + empty-state messaging |
| [#424](https://github.com/randlee/agent-team-mail/issues/424) | Improve `atm teams spawn --help` with generated launch-command reference | AA.4 | Always-show copy/paste launch reference with placeholders when config absent |
| [#649](https://github.com/randlee/agent-team-mail/issues/649) | Add `atm teams remove-member` command | BF.1 | Promoted to active fix sprint BF.1 |
| [#650](https://github.com/randlee/agent-team-mail/issues/650) | Backup/restore should capture Claude Code project task list (`~/.claude/tasks/<project>/`) | BF.1 | Promoted to active fix sprint BF.1 |
| [#651](https://github.com/randlee/agent-team-mail/issues/651) | Restore sets highwatermark off-by-one | BF.1 | Promoted to active fix sprint BF.1 |
| [#652](https://github.com/randlee/agent-team-mail/issues/652) | `ux(gh pr list)`: make merge conflicts and CI-blocked-by-merge visually prominent | AI follow-on | Explicitly out of BF.1 scope; unrelated to backup/restore hardening |
| [#287](https://github.com/randlee/agent-team-mail/issues/287) | `parse_since_input` accepts `0m` and negative durations | X.4 (deferred) | Deferred follow-on from Phase X onboarding tranche |
| [#337](https://github.com/randlee/agent-team-mail/issues/337) | Missing `#[serial]` on env-mutating daemon tests (`ATM_HOME`) | X.5 (deferred) | Deferred CI-debt cleanup in Phase X follow-on |
| [#338](https://github.com/randlee/agent-team-mail/issues/338) | `add-member` does not create inbox atomically | X.6 (deferred) | Deferred follow-on after onboarding contract closure |

---

### Active Fix Work

| Sprint | Name | PR | Branch | Issues | Status |
|---|---|---|---|---|---|
| BF.1 | Backup/Restore Hardening | [#653](https://github.com/randlee/agent-team-mail/pull/653) | `fix/backup-restore-hardening` | #649, #650, #651 | MERGED |
| BF.2 | SSoT Path Helpers + ATM_LOG_PATH Removal | [#674](https://github.com/randlee/agent-team-mail/pull/674) | `fix/ssot-path-helpers-663-664-665` | #663, #664, #665 | IN PROGRESS (CI) |
| BF.3 | UX/Docs/Test Backlog | [#675](https://github.com/randlee/agent-team-mail/pull/675) | `fix/ux-docs-test-652-645-656` | #652, #645, #656 | MERGED |
| BF.4 | gh_monitor config fallback | [#677](https://github.com/randlee/agent-team-mail/pull/677) | `fix/issue-676-gh-monitor-config-repo-fallback` | #676 | QA FAIL (fix pass 2) |
| BF.5 | AJ deferred test improvements | — | `fix/test-improvements-627-642-643` | #627, #642, #643 | IN PROGRESS |

---

## 22. Phase AE: GH Monitor Reliability + Daemon Logging Isolation

**Goal**: complete GH monitor operational contracts and close daemon
observability/runtime gaps discovered during dogfooding.

**Integration branch**: `integrate/phase-AE`

### Dependency graph

1. AE.1 is foundational (config/init contract).
2. AE.2 depends on AE.1.
3. AE.3 depends on AE.1 + AE.2.
4. AE.4 runs in parallel with AE.2/AE.3.
5. AE.5 runs after AE.2 + AE.4.

### Sprint Summary

| Sprint | Name | PR | Branch | Issues | Status |
|---|---|---|---|---|---|
| AE.1 | Config Discovery + `atm gh init` Baseline | [#518](https://github.com/randlee/agent-team-mail/pull/518) | `feature/pAE-s1-config-init` | #499, #500 | COMPLETE |
| AE.2 | Live Status + JSON + Output Consistency | [#519](https://github.com/randlee/agent-team-mail/pull/519) | `feature/pAE-s2-live-status-json` | #503, #504, #505 | COMPLETE |
| AE.3 | Monitor Reload Semantics | [#521](https://github.com/randlee/agent-team-mail/pull/521) | `feature/pAE-s3-reload-semantics` | #502 | COMPLETE |
| AE.4 | Daemon Logging/Autostart/Plugin Isolation | [#522](https://github.com/randlee/agent-team-mail/pull/522) | `feature/pAE-s4-daemon-observability` | #472, #473, #474 | COMPLETE |
| AE.5 | Identity Ambiguity + Phase Closeout | [#523](https://github.com/randlee/agent-team-mail/pull/523) | `feature/pAE-s5-identity-closeout` | #506 | COMPLETE |

## 23. Phase AF: Team Management Reliability + Lifecycle Hardening

**Goal**: close team-member lifecycle, spawn authorization, and cleanup reliability
gaps before transitioning to post-AF phase work.

**Integration branch**: `integrate/phase-AF`

### Sprint Summary

| Sprint | Name | PR | Branch | Issues | Status |
|---|---|---|---|---|---|
| AF.1 | Lifecycle Correctness (Session + PID Liveness) | [#524](https://github.com/randlee/agent-team-mail/pull/524) | `feature/pAF-s1-lifecycle-correctness` | #448, #449 | COMPLETE |
| AF.2 | Spawn Authorization + Preview UX | [#526](https://github.com/randlee/agent-team-mail/pull/526) | `feature/pAF-s2-spawn-auth-preview` | #394, #456 | COMPLETE |
| AF.3 | Transient Agent Registration Controls | [#527](https://github.com/randlee/agent-team-mail/pull/527) | `feature/pAF-s3-transient-registration` | #393 | COMPLETE |
| AF.4 | Cleanup Preview + tmux Sentinel | [#528](https://github.com/randlee/agent-team-mail/pull/528) | `feature/pAF-s4-cleanup-sentinel` | #373, #45 | COMPLETE |
| AF.5 | Reliability Regression + Documentation Closure | [#529](https://github.com/randlee/agent-team-mail/pull/529) | `feature/pAF-s5-reliability-closeout` | #448, #449, #393, #394, #456, #373, #45 | COMPLETE |

## 24. Scrum Master Agent Prompt

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

**Document Version**: 0.7
**Last Updated**: 2026-03-10
**Maintained By**: Claude (ARCH-ATM)
