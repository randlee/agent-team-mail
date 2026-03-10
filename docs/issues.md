# Known Issues

Last updated: 2026-03-10

## Active Critical Fixes (Current)

| Issue | Summary | Type | Status | Priority | Planned Sprint | Notes |
|---|---|---|---|---|---|---|
| [#636](https://github.com/randlee/agent-team-mail/issues/636) | `register-hint` blocked by `validate_pid_backend` gate; members remain `Unknown` | Bug | Open (active fix branch) | Critical | `fix/636` | Write-path must be non-blocking; mismatch remains advisory. Implementation plan: `docs/adr/issue-636-registration-gate-fix-plan.md` |

## Phase AD Dogfood Blockers (GH Monitor Setup Session 2026-03-07)

| Issue | Summary | Type | Status | Priority | Planned Sprint | Notes |
|---|---|---|---|---|---|---|
| [#497](https://github.com/randlee/agent-team-mail/issues/497) | DG-001: daemon process leak (multiple concurrent `atm-daemon` processes) | Bug | Open | Critical | Pre-AD hotfix | Must be fixed before AD sprint implementation starts |
| [#498](https://github.com/randlee/agent-team-mail/issues/498) | DG-002: daemon socket path mismatch between hook context and CLI context | Bug | Open | Critical | Pre-AD hotfix | `ATM_HOME`/socket-path divergence caused false daemon-unreachable errors |
| [#499](https://github.com/randlee/agent-team-mail/issues/499) | DG-003: repo `.atm.toml` plugin config not visible to daemon | Bug | Closed (implemented) | High | AD.2 | Closed in AD.2 (daemon/CLI config resolution parity delivered). |
| [#500](https://github.com/randlee/agent-team-mail/issues/500) | DG-004: missing `atm gh init` guided setup command | Enhancement | Open | High | AD.1 | Required by plugin requirements; currently dead-end setup flow |
| [#501](https://github.com/randlee/agent-team-mail/issues/501) | DG-005: missing daemon stop/restart/reload command set | Bug | Open | High | Pre-AD hotfix | Operational recovery requires manual PID kill today |
| [#502](https://github.com/randlee/agent-team-mail/issues/502) | DG-006: monitor restart reloads lifecycle but not updated config | Bug | Closed (implemented) | Medium | AD.4 | Closed in AD.4 (restart now reloads monitor config). |
| [#503](https://github.com/randlee/agent-team-mail/issues/503) | DG-007: `atm gh monitor status` reads cached state instead of live daemon state | Bug | Closed (implemented) | Medium | AD.4 | Closed in AD.4 (status surfaces now resolve live daemon state). |
| [#504](https://github.com/randlee/agent-team-mail/issues/504) | DG-008: `atm gh monitor status --json` not supported | Bug | Closed (implemented) | Medium | AD.3 | Closed in AD.3 (`--json` support delivered for monitor status). |
| [#505](https://github.com/randlee/agent-team-mail/issues/505) | DG-009: inconsistent daemon reachability across `atm gh` commands | Bug | Closed (implemented) | Medium | AD.4 | Closed in AD.4 (reachability/state reporting parity across `atm gh`). |
| [#506](https://github.com/randlee/agent-team-mail/issues/506) | DG-010: `disabled_config_error` has no actionable guidance | UX | Closed (implemented) | Low | AD.1 | Closed in AD.1 (unavailable states now include actionable init guidance). |
| [#507](https://github.com/randlee/agent-team-mail/issues/507) | DG-011: duplicate daemon status output blocks in `atm gh` surfaces | UX | Closed (implemented) | Low | AD.3 | Closed in AD.3 (single canonical status output retained). |

## Phase AF Reliability Mapping (Lifecycle + Spawn + Cleanup)

| Issue | Summary | Type | Status | Priority | Planned Sprint | Notes |
|---|---|---|---|---|---|---|
| [#448](https://github.com/randlee/agent-team-mail/issues/448) | `session_end` session-id scoping + stale dead/alive drift | Bug | Open (implemented in AF.1) | Critical | AF.1 | Implemented in PR [#524](https://github.com/randlee/agent-team-mail/pull/524); pending GitHub issue closure |
| [#449](https://github.com/randlee/agent-team-mail/issues/449) | PID liveness cache TTL + periodic re-probe | Enhancement | Open (implemented in AF.1) | High | AF.1 | Implemented in PR [#524](https://github.com/randlee/agent-team-mail/pull/524); pending GitHub issue closure |
| [#394](https://github.com/randlee/agent-team-mail/issues/394) | Gate terminal spawn to team-lead/co-leaders | Enhancement | Open (implemented in AF.2) | High | AF.2 | Implemented in PR [#526](https://github.com/randlee/agent-team-mail/pull/526); pending GitHub issue closure |
| [#456](https://github.com/randlee/agent-team-mail/issues/456) | Spawn auth-failure path must still print launch preview | Bug | Open (implemented in AF.2) | High | AF.2 | Implemented in PR [#526](https://github.com/randlee/agent-team-mail/pull/526); pending GitHub issue closure |
| [#393](https://github.com/randlee/agent-team-mail/issues/393) | Task-tool transient agents must not auto-register to persistent roster | Bug | Open (implemented in AF.3) | High | AF.3 | Implemented in PR [#527](https://github.com/randlee/agent-team-mail/pull/527); ADR + non-member regression test added in AF.5 |
| [#373](https://github.com/randlee/agent-team-mail/issues/373) | `atm teams cleanup --dry-run` preview mode | Enhancement | Open (implemented in AF.4) | Medium | AF.4 | Implemented in PR [#528](https://github.com/randlee/agent-team-mail/pull/528); reason-code parity hardened |
| [#45](https://github.com/randlee/agent-team-mail/issues/45) | tmux sentinel injection | Enhancement | Open (implemented in AF.4) | Medium | AF.4 | Implemented in PR [#528](https://github.com/randlee/agent-team-mail/pull/528); nudge sentinel tier contract now documented |

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
| [#45](https://github.com/randlee/agent-team-mail/issues/45) | Tmux Sentinel Injection | Enhancement | Open | Medium | T.11 | Runtime signaling improvement |
| [#46](https://github.com/randlee/agent-team-mail/issues/46) | Codex Idle Detection via Notify Hook | Enhancement | Open | Medium | T.5c (design) | Availability signaling clarification tranche |
| [#47](https://github.com/randlee/agent-team-mail/issues/47) | Ephemeral Pub/Sub for Agent Availability | Enhancement | Open | Medium | T.5c (design) | Availability signaling clarification tranche |
| [#281](https://github.com/randlee/agent-team-mail/issues/281) | Gemini resume flag drift | Bug | Open | High | T.4 | Runtime resume correctness (after T.3 wiring) |
| [#282](https://github.com/randlee/agent-team-mail/issues/282) | Gemini end-to-end spawn wiring | Enhancement | Open | High | T.3 | Runtime integration completeness baseline |
| [#283](https://github.com/randlee/agent-team-mail/issues/283) | S.2a/S.1 plan deliverable accuracy | Documentation | Open | Medium | T.16 | Planning/doc alignment |
| [#284](https://github.com/randlee/agent-team-mail/issues/284) | CLI crate fails to publish (`include_str!` paths outside crate) | Bug | Open | High | T.5a | Parallel publishability tranche |
| [#286](https://github.com/randlee/agent-team-mail/issues/286) | `atm-monitor` operational health monitor implementation | Enhancement | Open | High | T.5b | Health monitoring implementation tracker |
| [#287](https://github.com/randlee/agent-team-mail/issues/287) | `parse_since_input` accepts `0m` and negative durations | Bug | Open | Medium | X.4 (deferred) | Deferred follow-on from current Phase X onboarding tranche |
| [#337](https://github.com/randlee/agent-team-mail/issues/337) | Missing `#[serial]` on env-mutating daemon tests (`ATM_HOME`) | Bug | Open | Medium | X.5 (deferred) | Deferred CI-debt cleanup in Phase X follow-on |
| [#338](https://github.com/randlee/agent-team-mail/issues/338) | `teams add-member` does not create inbox atomically | Bug | Open | High | X.6 (deferred) | Deferred follow-on after X.1-X.3 contract closure |
| [#351](https://github.com/randlee/agent-team-mail/issues/351) | Add `/team-join` slash command | Enhancement | Open | High | X.1 | Caller-team check first; `--team` optional verification in lead-initiated mode; output `claude --resume` launch command |
| [#357](https://github.com/randlee/agent-team-mail/issues/357) | `atm init` full one-command setup + default global hooks | Enhancement | Open | High | X.3 | One-command init flow (`.atm.toml` + team + hooks) and quickstart creation |
| [#361](https://github.com/randlee/agent-team-mail/issues/361) | Spawn path normalization (`--folder` canonical, `--cwd` compatibility) | Enhancement | Open | Medium | X.2 | Replaces Phase X placeholder tracker for spawn normalization |

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
| Phase X deferred follow-on queue (`#287`, `#337`, `#338`) | Bug | Medium/High | Explicitly mapped to X.4/X.5/X.6 with documented deferral rationale in `docs/project-plan.md` and verification stubs in `docs/test-plan-phase-X.md`. |

## Non-GitHub Planning Gap

| Item | Type | Status | Priority | Notes |
|---|---|---|---|---|
| Keep provisional sprint mappings synchronized across planning docs | Documentation | Open | Medium | Source-of-truth sequencing for current draft is `docs/test-plan-phase-T.md`; update `project-plan.md` + `issues.md` together on mapping changes |
| Spawn directory flag normalization (`--folder` canonical, `--cwd` compatibility alias) | Documentation/Design | Tracked | Medium | Planned Sprint: X.2. GitHub tracker: [#361](https://github.com/randlee/agent-team-mail/issues/361). |
| Codex/Gemini startup guidance prompt injection for `atm teams spawn` | Documentation/Design | Open | Medium | Phase X requirement: inject ATM usage guidance before/after caller prompt (or guidance-only when prompt omitted) |

## New Doctor Findings (Issue Creation Completed)

| Item | Type | Status | Priority | Notes |
|---|---|---|---|---|
| Partial teardown: dead `team-lead` session with missing mailbox but roster entry remains (`PARTIAL_TEARDOWN`) | Bug | Open | Critical | `atm doctor` reports mailbox teardown integrity drift after team recreation. `team-lead` can remain in roster while mailbox file is absent and session is marked dead. |
| Terminal member not cleaned: dead `arch-ctm` remains in roster + mailbox (`TERMINAL_MEMBER_NOT_CLEANED`) | Bug | Open | Critical | `atm doctor` reports dead-member teardown drift where roster and mailbox persist after terminal session death. Cleanup/reconciliation should remove stale dead members deterministically. |
| `atm doctor` output misses member-status snapshot header (design gap) | UX/Diagnostics | Open | High | Initial design expected doctor to print team-member table first (name/type/model/status, like `atm members`) before findings. Current output lists findings only, which weakens situational context and can mislead diagnosis. |
| Doctor reconciliation appears cross-team (reports unknown agents from other teams) | Bug | Open | High | Observed symmetry: `atm doctor --team atm-dev` reports `researcher` (from `annotations-test`), and `atm doctor --team annotations-test` reports `arch-ctm`/`arch-gtm` from `atm-dev`. Likely roster/session integrity check not strictly team-scoped. |
| Session registry drift after team recreation/removal (`DAEMON_TRACKS_UNKNOWN_AGENT`) | Bug | Open | High | Daemon continues tracking removed agents (for example `arch-ctm`) after roster reset/recreate, causing persistent unknown-agent warnings. |
| `isActive=true` members without daemon session (`ACTIVE_WITHOUT_SESSION`) after restore/recreate | Bug | Open | Medium | Restored/re-added members can remain marked active with no live daemon session record; doctor warns until explicit registration/reconciliation occurs. |
| `atm doctor` recommends `atm register` even when `CLAUDE_SESSION_ID` is unavailable | UX/Diagnostics | Open | Medium | In non-hook shells, `atm register` fails with \"Cannot determine session_id\"; recommendation should be context-aware or include actionable fallback guidance (`--as`, run from managed session, etc.). |

## Phase V Active Mapping (Issue-Backed)

| Sprint | Focus | Issue |
|---|---|---|
| V.1 | Team-scoped doctor reconciliation | [#333](https://github.com/randlee/agent-team-mail/issues/333) |
| V.2 | Lead/non-lead teardown semantics | [#332](https://github.com/randlee/agent-team-mail/issues/332) |
| V.3 | `isActive`/liveness separation | [#330](https://github.com/randlee/agent-team-mail/issues/330) |
| V.4 | Terminal cleanup convergence + stale tracked members | [#331](https://github.com/randlee/agent-team-mail/issues/331), [#334](https://github.com/randlee/agent-team-mail/issues/334) |
| V.5 | Recommendation actionability | [#336](https://github.com/randlee/agent-team-mail/issues/336) |
| V.6 | Doctor output context snapshot ordering | [#335](https://github.com/randlee/agent-team-mail/issues/335) |

## Phase W Coordination

- Release automation moved to Phase W (`W.1`–`W.4`) to avoid naming collision with Phase V.
- `send.rs` overlap between W.1 and V.3 requires W.1 to land first.

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

## Recently Resolved (Implemented in `develop`; Issue Cleanup Pending Where Applicable)

| Item | Status | Notes |
|---|---|---|
| [#181](https://github.com/randlee/agent-team-mail/issues/181) | Implemented | Delivered in Phase T PR [#288](https://github.com/randlee/agent-team-mail/pull/288); issue may remain open pending verification/closure housekeeping. |
| [#182](https://github.com/randlee/agent-team-mail/issues/182) | Implemented | Delivered in Phase T PR [#289](https://github.com/randlee/agent-team-mail/pull/289); issue may remain open pending verification/closure housekeeping. |
| [#183](https://github.com/randlee/agent-team-mail/issues/183) | Implemented/Closed | Delivered in Phase T PR [#289](https://github.com/randlee/agent-team-mail/pull/289) and currently closed on GitHub. |
| [#184](https://github.com/randlee/agent-team-mail/issues/184) | Implemented | Delivered in Phase T PR [#299](https://github.com/randlee/agent-team-mail/pull/299); issue may remain open pending verification/closure housekeeping. |
| [#185](https://github.com/randlee/agent-team-mail/issues/185) | Implemented | Delivered in Phase T PR [#299](https://github.com/randlee/agent-team-mail/pull/299); issue may remain open pending verification/closure housekeeping. |
| [#187](https://github.com/randlee/agent-team-mail/issues/187) | Implemented | Delivered in Phase T PR [#299](https://github.com/randlee/agent-team-mail/pull/299); issue may remain open pending verification/closure housekeeping. |
| PR #278 QA/CI blockers (`/home/tester` hardcode + Windows PID test) | Resolved | Fixed and merged; removed from open-issues set |
