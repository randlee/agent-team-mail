# Known Issues

Last updated: 2026-03-01

## Open GitHub Issues

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

## Recently Resolved (No Longer Open)

| Item | Status | Notes |
|---|---|---|
| PR #278 QA/CI blockers (`/home/tester` hardcode + Windows PID test) | Resolved | Fixed and merged; removed from open-issues set |
