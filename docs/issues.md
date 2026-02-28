# Known Issues

Last updated: 2026-02-28

## Open GitHub Issues

| Issue | Summary | Type | Status | Priority | Planned Sprint | Notes |
|---|---|---|---|---|---|---|
| [#181](https://github.com/randlee/agent-team-mail/issues/181) | Daemon not auto-starting | Bug | Open | Critical | T.1 | Blocks daemon-dependent features |
| [#182](https://github.com/randlee/agent-team-mail/issues/182) | Agent roster not seeded from `config.json` | Bug | Open | Critical | T.2 | Daemon can start with empty roster |
| [#183](https://github.com/randlee/agent-team-mail/issues/183) | Agent state never transitions | Bug | Open | Critical | T.3 | State tracking broken |
| [#184](https://github.com/randlee/agent-team-mail/issues/184) | TUI right panel contradicts left panel | Bug | Open | High | T.4 | UX/state consistency |
| [#185](https://github.com/randlee/agent-team-mail/issues/185) | No message viewing in TUI | Enhancement | Open | Medium | T.5 | Feature gap |
| [#187](https://github.com/randlee/agent-team-mail/issues/187) | TUI header missing version number | Bug | Open | Low | T.6 | Quick fix |
| [#45](https://github.com/randlee/agent-team-mail/issues/45) | Tmux Sentinel Injection | Enhancement | Open | Medium | T.11 | Runtime signaling improvement |
| [#46](https://github.com/randlee/agent-team-mail/issues/46) | Codex Idle Detection via Notify Hook | Enhancement | Open | Medium | T.12 | Availability detection |
| [#47](https://github.com/randlee/agent-team-mail/issues/47) | Ephemeral Pub/Sub for Agent Availability | Enhancement | Open | Medium | T.13 | Availability transport |
| [#281](https://github.com/randlee/agent-team-mail/issues/281) | Gemini resume flag drift | Bug | Open | High | T.14 | Runtime resume correctness |
| [#282](https://github.com/randlee/agent-team-mail/issues/282) | Gemini end-to-end spawn wiring | Enhancement | Open | High | T.15 | Runtime integration completeness |
| [#283](https://github.com/randlee/agent-team-mail/issues/283) | S.2a/S.1 plan deliverable accuracy | Documentation | Open | Medium | T.16 | Planning/doc alignment |
| [#284](https://github.com/randlee/agent-team-mail/issues/284) | CLI crate fails to publish (`include_str!` paths outside crate) | Bug | Open | High | Unassigned | Crates.io publish gap for `agent-team-mail` CLI |

## Closed / Superseded (Tracked for Context)

| Issue | Status | Priority | Notes |
|---|---|---|---|
| [#186](https://github.com/randlee/agent-team-mail/issues/186) | Closed (superseded) | N/A | Superseded by Phase L unified logging |
| [#188](https://github.com/randlee/agent-team-mail/issues/188) | Closed (superseded) | N/A | Superseded by Phase L logging overhaul |

## Non-GitHub Planning Gap

| Item | Type | Status | Priority | Notes |
|---|---|---|---|---|
| `docs/project-plan.md` sprint mapping mismatch for #45/#46/#47 | Documentation | Open | Medium | Phase T sprint table maps to T.11/T.12/T.13, but Open Issues Tracker maps to T.10/T.11/T.12 |

## Recently Resolved (No Longer Open)

| Item | Status | Notes |
|---|---|---|
| PR #278 QA/CI blockers (`/home/tester` hardcode + Windows PID test) | Resolved | Fixed and merged; removed from open-issues set |

