# agent-team-mail (`atm`) — Project Plan

**Version**: 0.6  
**Date**: 2026-03-01  
**Status**: Active execution in Phase T (reliability + bug debt + deferred work)

## Purpose
This document is the concise execution plan.

- Current and upcoming work lives here.
- Full historical sprint detail is archived at:
  - `docs/archive/project-plan-archive-2026-02-28.md`

## Current State

### Completed Phase Summary

| Phase Group | Scope | Status |
|---|---|---|
| 1-10 | Foundation through Codex orchestration baseline | Complete |
| A-E | MCP + session mgmt + observability + TUI streaming + core bug fixes | Complete |
| G | Codex multi-transport runtime hardening | Complete |
| L-P | Logging overhaul through attach path hardening | Complete |
| Q | MCP server setup CLI | Complete |
| R | Session handoff + hook installer foundation | Complete |
| S | Runtime adapters + hook installer | Complete |

## Active Phase

## Phase T — Daemon Reliability + Bug Debt + Deferred Sprints

**Goal**: Resolve critical daemon/TUI bugs, CLI publishability, operational monitoring, Gemini adapter, and test coverage closure.

**Integration branch**: `integrate/phase-T` (off `develop`)

**Execution sequence**: T.1 → T.2 → `[T.5a ‖ T.5b ‖ T.5c]` → T.3 → T.4 → T.7; T.6 independent

### Planning Gates — All Passed

All 7 Phase T planning gates passed. Requirements, test plan, issue ordering, sprint boundaries, and MCP observability readiness criteria are documented in:
- `docs/test-plan-phase-T.md` (acceptance criteria + test matrices, source of truth for sprint scope)
- `docs/requirements.md` (sections 4.3.3a, 4.3.10, 4.6, 4.7, 4.8.6)
- `docs/issues.md` (issue status + priority + sprint mapping)

### Phase T Sprint Execution Status

| Sprint | Title | Issue(s) | Priority | Status | PR |
|---|---|---|---|---|---|
| T.1 | Daemon auto-start + single-instance reliability | #181 | Critical | In progress (fixes) | #288 |
| T.2 | Roster seeding + config watcher + state transitions | #182, #183 | Critical | In progress (QA review) | #289 |
| T.5a | CLI crate publishability | #284 | High | In progress (fixes) | #290 |
| T.5b | `atm-monitor` operational health monitor | #286 | High | Planned | — |
| T.5c | Availability signaling clarification | #46, #47 | Medium | Planned | — |
| T.3 | Gemini end-to-end spawn wiring | #282 | High | Planned | — |
| T.4 | Gemini resume correctness | #281 | High | Planned | — |
| T.6 | Test coverage closure (U.1–U.4) | #184, #185, #187 | Medium | Independent/deferred | — |
| T.7 | Permanent publishing process + strengthened `publisher` role | — | High | Planned (after T.5a) | — |

### Execution Notes

- T.5a/T.5b/T.5c are a parallel tranche (designed for concurrent execution; sequenced for single developer).
- T.6 is independent of T.1–T.5* and may be scheduled at any point once acceptance criteria are fully scoped.
- T.7 runs after T.5a (publishability fixes) and before final phase release; becomes permanent default for all subsequent phases via `.claude/agents/publisher.md`.
- Sprint details, acceptance criteria, and test matrices: `docs/test-plan-phase-T.md`.

## Backlog (Post-Phase T)

| Item | Issue | Notes |
|---|---|---|
| Tmux Sentinel Injection | #45 | Runtime signaling improvement |
| `atm teams resume` session handoff | — | Deferred from Phase S |
| OpenCode baseline adapter | — | Deferred from Phase S |
| S.2a/S.1 deliverable accuracy | #283 | Doc alignment |

## Open Issues Source Of Truth

- `docs/issues.md` (status + priority + sprint mapping)

## Closed / Superseded Context

| Issue | Status | Notes |
|---|---|---|
| [#186](https://github.com/randlee/agent-team-mail/issues/186) | Closed (superseded) | Replaced by unified logging in Phase L |
| [#188](https://github.com/randlee/agent-team-mail/issues/188) | Closed (superseded) | Logging prerequisite superseded by Phase L work |

## Planning Hygiene

- Keep sprint mappings synchronized across `docs/test-plan-phase-T.md`, `docs/project-plan.md`, and `docs/issues.md`.
- Keep `docs/issues.md` synchronized with actual issue status/priority changes.
- Keep `DoctorReport` JSON contract aligned with requirements (`docs/requirements.md` sections 4.3.3 and 4.6).

## References

- Requirements: `docs/requirements.md`
- Test plan: `docs/test-plan-phase-T.md`
- Issue tracker: `docs/issues.md`
- Historical archive: `docs/archive/project-plan-archive-2026-02-28.md`
