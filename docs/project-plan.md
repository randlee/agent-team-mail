# agent-team-mail (`atm`) — Project Plan

**Version**: 0.6  
**Date**: 2026-02-28  
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

### Legacy Planning Notes (Pre-Phase T)

| Item | Status | Notes |
|---|---|---|
| `atm init` installer baseline | Complete | Implemented in Phase S.2a; remaining follow-up is `T.7` (`atm init --check` + upgrade validation) |

## Active Phase

## Phase T — Daemon Reliability + Bug Debt + Deferred Sprints

**Goal**: Plan Phase T from current priorities and issue reality before committing sprint scope.

**Integration branch**: `integrate/phase-T` (off `develop`)

### Phase T Planning Status

- No Phase T sprint commitments are finalized yet.
- Candidate work is tracked in `docs/issues.md` and will be selected by priority during planning.
- Phase T kickoff is blocked on unresolved `atm init` design concerns.

### Phase T Planning Gates (Required Before Sprint Start)

1. Resolve `atm init` design concerns and record decisions in requirements/design docs.
2. Confirm top-priority issue ordering from `docs/issues.md` (critical bugs first).
3. Define sprint boundaries with explicit acceptance criteria and test gates.
4. Confirm dependencies/sequence for selected work.
5. Freeze a first execution slice only after the above are complete.
6. Pass requirements/plan consistency review (ATM-QA doc review gate) before build starts.
7. Before MCP testing, pass observability readiness gates defined in `docs/test-plan-phase-T.md`.

### Phase T Candidate Backlog Source

- Source of truth: `docs/issues.md` (issue, status, priority, description).
- Any mapping from issues to T-sprints remains provisional until planning gates pass.

### Uncommitted Candidate Work (Preserved From Prior Draft)

These items were previously mapped to T-sprints but are now intentionally
uncommitted until planning gates are completed:

| Candidate Item | Prior Mapping | Current State |
|---|---|---|
| Daemon auto-start on CLI usage | T.1 / #181 | Candidate only |
| Agent roster seeding + `config.json` watcher | T.2 / #182 | Candidate only |
| Agent state transitions | T.2 / #183 | Candidate only |
| TUI panel consistency | Unscheduled / #184 | Candidate only |
| TUI message viewing | Unscheduled / #185 | Candidate only |
| TUI header version | Unscheduled / #187 | Candidate only |
| `atm init --check` + upgrade validation | T.7 | Candidate only |
| `atm teams resume` session handoff | T.8 | Candidate only |
| OpenCode baseline adapter | T.9 | Candidate only |
| Operational health agent (`atm-monitor`) | T.5b / #286 | Candidate only |
| Tmux Sentinel Injection | T.11 / #45 | Candidate only |
| Codex Idle Detection via Notify Hook | T.5c / #46 | Candidate only |
| Ephemeral Pub/Sub for Agent Availability | T.5c / #47 | Candidate only |
| Gemini resume flag drift | T.4 / #281 | Candidate only |
| Gemini end-to-end spawn wiring | T.3 / #282 | Candidate only |
| S.2a/S.1 deliverable accuracy cleanup | T.16 / #283 | Candidate only |
| CLI crate publish failure (`include_str!` packaging) | T.5a / #284 | Candidate only |
| Test coverage closure for unscheduled backlog (`U.1`-`U.4`) | T.6 | Candidate only |

## Open Issues Source Of Truth

Issue tracking is maintained in:

- `docs/issues.md` (status + priority + description)

This plan references that issue list and uses it to drive Phase T planning decisions.

## Closed / Superseded Context

| Issue | Status | Notes |
|---|---|---|
| [#186](https://github.com/randlee/agent-team-mail/issues/186) | Closed (superseded) | Replaced by unified logging in Phase L |
| [#188](https://github.com/randlee/agent-team-mail/issues/188) | Closed (superseded) | Logging prerequisite superseded by Phase L work |

## Planning Hygiene Items

- Keep provisional sprint mappings synchronized between `docs/test-plan-phase-T.md`,
  `docs/project-plan.md`, and `docs/issues.md`.
- Keep `docs/issues.md` synchronized with actual issue status/priority changes.
- Keep `DoctorReport` JSON contract and logging-health expansion notes aligned
  with requirements (`docs/requirements.md`, sections 4.3.3 and 4.6).

## References

- Requirements: `docs/requirements.md`
- Issue tracker doc: `docs/issues.md`
- Historical plan archive: `docs/archive/project-plan-archive-2026-02-28.md`
