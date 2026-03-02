# Known Issues

Last updated: 2026-03-02 (Phase V + Phase W)

## Phase V Issues (Doctor/Lifecycle — arch-ctm's V.0–V.7)

| Issue | Summary | Type | Status | Priority | Planned Sprint | Notes |
|---|---|---|---|---|---|---|
| [#330](https://github.com/randlee/agent-team-mail/issues/330) | isActive conflated with liveness across 9+ code paths | Bug | Open | Critical | V.3 | Root cause of all offline-while-functional reports |
| [#331](https://github.com/randlee/agent-team-mail/issues/331) | TERMINAL_MEMBER_NOT_CLEANED — dead non-lead persists in roster/mailbox | Bug | Open | Critical | V.4 | Lifecycle cleanup not guaranteed on all paths |
| [#332](https://github.com/randlee/agent-team-mail/issues/332) | PARTIAL_TEARDOWN misclassifies team-lead dead session as critical | Bug | Open | High | V.2 | Lead retention policy not in classification logic |
| [#333](https://github.com/randlee/agent-team-mail/issues/333) | cross-team doctor bleed — DAEMON_TRACKS_UNKNOWN_AGENT across teams | Bug | Open | High | V.1 | Unscoped query_list_agents() |
| [#334](https://github.com/randlee/agent-team-mail/issues/334) | Session registry drift after team recreation (DAEMON_TRACKS_UNKNOWN_AGENT persists) | Bug | Open | High | V.4 | Daemon registry not pruned on recreate |
| [#335](https://github.com/randlee/agent-team-mail/issues/335) | Doctor member snapshot header missing (findings shown without context table) | Enhancement | Open | Medium | V.6 | Output ordering: members before findings |
| [#336](https://github.com/randlee/agent-team-mail/issues/336) | Doctor recommends `atm register` when CLAUDE_SESSION_ID unavailable | Bug | Open | Medium | V.5 | Non-actionable recommendation in plain shells |

## Phase W Issues (Release Automation + ATM Bugs — team-lead's W.1–W.4)

| Issue | Summary | Type | Status | Priority | Planned Sprint | Notes |
|---|---|---|---|---|---|---|
| [#323](https://github.com/randlee/agent-team-mail/issues/323) | Release workflow: post-publish-verify crates.io API 403 | Bug | Open | High | W.3 | Add retry logic to curl checks |
| [#324](https://github.com/randlee/agent-team-mail/issues/324) | Release workflow: add Homebrew formula publishing job | Enhancement | Open | High | W.3 | Automate Homebrew tap update |
| [#325](https://github.com/randlee/agent-team-mail/issues/325) | Release workflow: pre-publish audit + waiver gate | Enhancement | Open | High | W.4 | `cargo package --locked` gate before publish |
| [#326](https://github.com/randlee/agent-team-mail/issues/326) | Release workflow: cross-channel verification + completion report | Enhancement | Open | High | W.4 | Consolidated summary in workflow UI |
| [#327](https://github.com/randlee/agent-team-mail/issues/327) | publisher agent: eliminate sub-agent spawning | Bug | Open | High | W.2 | Rewrite publisher.md; trigger gh workflow run directly |
| [#328](https://github.com/randlee/agent-team-mail/issues/328) | atm send: remove default offline action prefix | Bug | Open | High | W.1 | send.rs:419 → `String::new()` |
| [#329](https://github.com/randlee/agent-team-mail/issues/329) | docs/agent-teams-mail-skill.md: remove [PENDING ACTION] tag pattern guidance | Documentation | Open | Medium | W.1 | Skill doc reinforces bad pattern |

## Deferred Backlog (Now Tracked)

| Issue | Summary | Type | Status | Priority | Notes |
|---|---|---|---|---|---|
| [#337](https://github.com/randlee/agent-team-mail/issues/337) | Missing #[serial] on 27 daemon integration tests (ATM_HOME env var races) | Bug | Open | Medium | Flaky CI risk |
| [#338](https://github.com/randlee/agent-team-mail/issues/338) | `atm teams add-member` does not create inbox file | Bug | Open | High | Blocks reliable onboarding |

## State-Model Inconsistency Inventory (Needs Sprint Planning)

These are code-level inconsistencies against the intended model:
- daemon/session registry = liveness source of truth
- `isActive` = activity/busy signal only

| Area | File / Code Path | Current Inconsistency | Next Sprint Action |
|---|---|---|---|
| Doctor member snapshot rendering | `crates/atm/src/commands/doctor.rs` (`member_snapshot` status mapping) | Maps `isActive` directly to `Online/Offline`, conflating activity with liveness. | Derive status from daemon/session query; show activity as separate field if needed. |
| Doctor reconciliation findings | `crates/atm/src/commands/doctor.rs` (`check_pid_session_reconciliation`) | `ACTIVE_WITHOUT_SESSION` / `ACTIVE_FLAG_STALE` logic is based on `is_active`, which is activity metadata. | Reframe findings around daemon liveness vs roster/session invariants; avoid treating activity bit as liveness truth. |
| Daemon config reconcile writes | `crates/atm-daemon/src/daemon/event_loop.rs` (`reconcile_team_member_activity`) | Overwrites `member.is_active` from PID/session liveness (`member.is_active = Some(alive)`). | Stop writing liveness into `isActive`; persist liveness separately (or derive on read). |
| Status command fallback | `crates/atm/src/commands/status.rs` (`resolve_member_active`) | Falls back to `member.is_active` when daemon state missing, presenting it as online/offline. | Require explicit liveness source/fallback label; do not silently map activity to online/offline. |
| Members command labels | `crates/atm/src/commands/members.rs` | Displays `isActive` as `Online/Offline`. | Rename display to activity semantics (`Busy/Idle/Unknown`) or split columns (Liveness + Activity). |
| Send offline detection | `crates/atm/src/commands/send.rs` | Offline warning path reads `isActive` instead of daemon liveness. | Use daemon/session liveness for offline hint; unknown when daemon unavailable. |
| Send heartbeat writes | `crates/atm/src/commands/send.rs` (`set_sender_heartbeat`) | Writes `isActive=true` heartbeat that downstream logic treats as liveness. | Keep heartbeat behavior but ensure all consumers treat it as activity-only. |
| Register/team mutation paths | `crates/atm/src/commands/register.rs`, `crates/atm/src/commands/teams.rs` | Commands set `isActive` in ways interpreted by other components as liveness. | Audit and align all writes/reads to activity semantics; add explicit liveness fields or daemon query adapters. |
| Test expectations | `crates/atm/tests/integration_conflict_tests.rs`, `integration_send.rs`, `integration_register.rs`, daemon tests | Many tests assert `Online/Offline` based on `isActive` toggles. | Rewrite assertions to separate liveness vs activity semantics and add regression coverage. |

## Pre-Phase-V Open Issues (Carried Forward)

| Issue | Summary | Type | Status | Priority | Planned Sprint | Notes |
|---|---|---|---|---|---|---|
| [#181](https://github.com/randlee/agent-team-mail/issues/181) | Daemon not auto-starting | Bug | Open | Critical | T.1 | Blocks daemon-dependent features |
| [#182](https://github.com/randlee/agent-team-mail/issues/182) | Agent roster not seeded from `config.json` | Bug | Open | Critical | T.2 | Daemon can start with empty roster |
| [#183](https://github.com/randlee/agent-team-mail/issues/183) | Agent state never transitions | Bug | Open | Critical | T.2 | State tracking broken |
| [#184](https://github.com/randlee/agent-team-mail/issues/184) | TUI right panel contradicts left panel | Bug | Open | High | T.6 | Test coverage closure sprint (`U.1`) |
| [#185](https://github.com/randlee/agent-team-mail/issues/185) | No message viewing in TUI | Enhancement | Open | Medium | T.6 | Test coverage closure sprint (`U.2`) |
| [#187](https://github.com/randlee/agent-team-mail/issues/187) | TUI header missing version number | Bug | Open | Low | T.6 | Test coverage closure sprint (`U.3`) |
| [#45](https://github.com/randlee/agent-team-mail/issues/45) | Tmux Sentinel Injection | Enhancement | Open | Medium | T.11 | Runtime signaling improvement |
| [#46](https://github.com/randlee/agent-team-mail/issues/46) | Codex Idle Detection via Notify Hook | Enhancement | Open | Medium | T.5c (design) | Availability signaling clarification tranche |
| [#47](https://github.com/randlee/agent-team-mail/issues/47) | Ephemeral Pub/Sub for Agent Availability | Enhancement | Open | Medium | T.5c (design) | Availability signaling clarification tranche |
| [#281](https://github.com/randlee/agent-team-mail/issues/281) | Gemini resume flag drift | Bug | Open | High | T.4 | Runtime resume correctness (after T.3 wiring) |
| [#282](https://github.com/randlee/agent-team-mail/issues/282) | Gemini end-to-end spawn wiring | Enhancement | Open | High | T.3 | Runtime integration completeness baseline |
| [#283](https://github.com/randlee/agent-team-mail/issues/283) | S.2a/S.1 plan deliverable accuracy | Documentation | Open | Medium | T.16 | Planning/doc alignment |
| [#284](https://github.com/randlee/agent-team-mail/issues/284) | CLI crate fails to publish (`include_str!` paths outside crate) | Bug | Open | High | T.5a | Parallel publishability tranche |
| [#286](https://github.com/randlee/agent-team-mail/issues/286) | `atm-monitor` operational health monitor implementation | Enhancement | Open | High | T.5b | Health monitoring implementation tracker |

## Closed / Superseded (Tracked for Context)

| Issue | Status | Priority | Notes |
|---|---|---|---|
| [#186](https://github.com/randlee/agent-team-mail/issues/186) | Closed (superseded) | N/A | Superseded by Phase L unified logging |
| [#188](https://github.com/randlee/agent-team-mail/issues/188) | Closed (superseded) | N/A | Superseded by Phase L logging overhaul |

## In-Flight Fix Branches (No GitHub Issue Yet)

| Item | Type | Branch | Priority | Notes |
|---|---|---|---|---|
| Flaky MCP proxy integration tests (`test_codex_event_forwarded_to_upstream`, `test_multiple_synthetic_tools_count`) | Bug | `fix/mcp-proxy-flaky-tests` | High | Pre-existing on `develop`; root-caused by rust-architect: real OS process spawn latency + time-bounded response drain; fix: ID-targeted response reading in `crates/atm-agent-mcp/tests/proxy_integration.rs`. **MERGED (PR #291).** |

## Deferred Technical Debt

| Item | Type | Priority | Notes |
|---|---|---|---|
| Pre-existing env-var serial violations in daemon integration tests | Bug | Medium | `crates/atm-daemon/tests/`: `issues_error_tests.rs` (8 tests), `ci_monitor_error_tests.rs` (10 tests), `issues_integration.rs` (9 tests) all call `set_var("ATM_HOME", ...)` in shared helpers from `#[tokio::test]` without `#[serial]`. Not T.2 regressions — pre-existing flakiness risk. Cleanup sprint deferred. |

## Non-GitHub Planning Gap

| Item | Type | Status | Priority | Notes |
|---|---|---|---|---|
| Keep provisional sprint mappings synchronized across planning docs | Documentation | Open | Medium | Source-of-truth sequencing for current draft is `docs/test-plan-phase-T.md`; update `project-plan.md` + `issues.md` together on mapping changes |

## New Doctor Findings (Needs GitHub Issue Creation)

| Item | Type | Status | Priority | Notes |
|---|---|---|---|---|
| Partial teardown: dead `team-lead` session with missing mailbox but roster entry remains (`PARTIAL_TEARDOWN`) | Bug | Open | Critical | `atm doctor` reports mailbox teardown integrity drift after team recreation. `team-lead` can remain in roster while mailbox file is absent and session is marked dead. |
| Terminal member not cleaned: dead `arch-ctm` remains in roster + mailbox (`TERMINAL_MEMBER_NOT_CLEANED`) | Bug | Open | Critical | `atm doctor` reports dead-member teardown drift where roster and mailbox persist after terminal session death. Cleanup/reconciliation should remove stale dead members deterministically. |
| `atm doctor` output misses member-status snapshot header (design gap) | UX/Diagnostics | Open | High | Initial design expected doctor to print team-member table first (name/type/model/status, like `atm members`) before findings. Current output lists findings only, which weakens situational context and can mislead diagnosis. |
| Doctor reconciliation appears cross-team (reports unknown agents from other teams) | Bug | Open | High | Observed symmetry: `atm doctor --team atm-dev` reports `researcher` (from `annotations-test`), and `atm doctor --team annotations-test` reports `arch-ctm`/`arch-gtm` from `atm-dev`. Likely roster/session integrity check not strictly team-scoped. |
| Session registry drift after team recreation/removal (`DAEMON_TRACKS_UNKNOWN_AGENT`) | Bug | Open | High | Daemon continues tracking removed agents (for example `arch-ctm`) after roster reset/recreate, causing persistent unknown-agent warnings. |
| `isActive=true` members without daemon session (`ACTIVE_WITHOUT_SESSION`) after restore/recreate | Bug | Open | Medium | Restored/re-added members can remain marked active with no live daemon session record; doctor warns until explicit registration/reconciliation occurs. |
| `atm doctor` recommends `atm register` even when `CLAUDE_SESSION_ID` is unavailable | UX/Diagnostics | Open | Medium | In non-hook shells, `atm register` fails with \"Cannot determine session_id\"; recommendation should be context-aware or include actionable fallback guidance (`--as`, run from managed session, etc.). |

### Root-Cause Notes (Documented, Not Yet Implemented)

| Issue | Root Cause (Code Path) | Proposed Sprint Scope |
|---|---|---|
| Doctor reconciliation appears cross-team (`DAEMON_TRACKS_UNKNOWN_AGENT`) | `crates/atm/src/commands/doctor.rs` currently calls unscoped `query_list_agents()` in `check_roster_session_integrity`, so tracked agents from other teams leak into a single-team doctor run. | Add team-scoped list query (`list-agents` payload with `team`), update doctor to use scoped results, and add regression tests proving no cross-team bleed. |
| `PARTIAL_TEARDOWN` on `team-lead` after recreation | `check_mailbox_integrity` treats all dead sessions the same, but `team-lead` is intentionally retained in roster by cleanup flows; missing mailbox + dead session can be expected transiently and is currently over-classified as critical drift. | Split teardown logic for lead vs non-lead members; classify lead state with explicit guidance (`register`/recreate session) instead of stale-member cleanup critical. |
| `ACTIVE_WITHOUT_SESSION` after restore/recreate | `check_pid_session_reconciliation` currently interprets `member.is_active` as liveness-adjacent despite `isActive` being activity metadata; restore/recreate paths expose this conflation when session registry is reset/rebuilt. | Split activity vs liveness semantics in doctor checks; use daemon/session truth for liveness findings and keep `isActive` for activity-only diagnostics. Add regression tests for restore/recreate transitions. |
| `TERMINAL_MEMBER_NOT_CLEANED` (dead member remains roster+mailbox) | `check_mailbox_integrity` correctly detects dead session + mailbox + roster for non-lead members, but lifecycle cleanup is not guaranteed to run (or complete) on all termination paths, so stale non-lead artifacts survive and doctor repeatedly reports critical drift. | Harden termination/cleanup orchestration so dead non-lead members are removed from roster and mailbox together across all kill/timeout paths; add teardown convergence tests. |
| Session registry drift after team recreation/removal (`DAEMON_TRACKS_UNKNOWN_AGENT`) | Daemon tracked-state/session entries can persist for removed members after team reset/recreate; stale daemon-side registry data is not deterministically pruned during roster reconciliation, so doctor continues to see unknown tracked agents even when team config is clean. | Add deterministic prune/reconcile pass in daemon roster/session synchronization (remove tracked entries absent from current team config after recreation/reset) and add regression tests covering remove/recreate cycles. |
| `atm register` recommendation fails without `CLAUDE_SESSION_ID` | `build_recommendations` unconditionally recommends `atm register <team>` for `ACTIVE_WITHOUT_SESSION`/`ACTIVE_FLAG_STALE`, but `atm register` requires a resolvable session id (managed environment or explicit `--as`) and can fail in plain shells. | Make recommendations context-aware: emit actionable alternatives when session id is unavailable (for example `--as` guidance, run from managed session, or daemon-assisted recovery command), with coverage tests for recommendation text/selection. |
| Cleanup recommendation loop (`atm teams cleanup`) on lead teardown | `build_recommendations` suggests `atm teams cleanup` for `PARTIAL_TEARDOWN`, but cleanup intentionally does not remove `team-lead`, creating a non-actionable loop for this finding class. | Make recommendation routing code-aware: lead/session repair command for lead findings, cleanup for terminal non-lead findings only. |
| Missing context table in doctor output | Human output prints findings first without the expected member status snapshot (`atm members` style), reducing triage clarity in degraded states. | Prepend doctor human output with concise member snapshot table before findings; keep JSON schema stable. |

## Recently Resolved (No Longer Open)

| Item | Status | Notes |
|---|---|---|
| PR #278 QA/CI blockers (`/home/tester` hardcode + Windows PID test) | Resolved | Fixed and merged; removed from open-issues set |
