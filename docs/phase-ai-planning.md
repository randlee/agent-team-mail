# Phase AI Planning — GH Monitor Dashboard + Detailed PR Reporting

## Goal

Add operator-facing GH monitor reporting surfaces for fast triage:
- `atm gh pr list`
- `atm gh pr report <PR>`

## Delivery Target

- Target version: `v0.44.0`
- Integration branch: `integrate/phase-AI`

## Phase Fit Decision

- #560 and #561 are classified as **Phase AI** work, not AH.
- AH remains focused on observability unification (`sc-observability`) and AG deferred closure.
- AI consumes AH outputs but should not expand AH scope/risk.

## Inputs

- Issue #564: `gh_monitor init` failure on daemon cold start (bug fix prerequisite).
- Issue #560: `atm gh pr list` one-line PR dashboard with CI/merge/review roll-up.
- Issue #561: `atm gh pr report <PR#>` detailed per-check report with matrix/timing/review/merge.
- Issue #582: report readiness semantics hardening (`SKIPPED` handling, review
  none vs unknown, transient mergeability retries, hard vs advisory reasons).

## Sprint Sizing

| Sprint | Scope | Issues | Rough Size |
|---|---|---|---|
| AI.0 | Cold-start init bug fix prerequisite (`gh_monitor init`) | #564 | S |
| AI.1 | `atm gh pr list` (human + `--json`), stable rollups | #560 | M |
| AI.2 | `atm gh pr report <PR>` (built-in formatter + `--json`) | #561 | M |
| AI.3 | Template/report customization (`--template`) + optional `init-report` scaffold | #561 follow-up | M/L |

## Sprint Dependencies

- AI.1 depends on AI.0.
- AI.2 depends on AI.1 data contracts.
- AI.3 depends on AI.2 report payload schema stabilization.

## Design Notes

- One-shot commands (`list`, `report`) are read/report only and should not emit notifications.
- Status-change notifications remain daemon monitor-loop behavior.
- Mergeability can be transient (`UNKNOWN`); treat as pending until stable.
- Matrix grouping should be deterministic with fallback to flat check list when grouping is ambiguous.
- Report payload schema should be versioned before exposing template extension points.
- Follow-up semantics hardening (#582):
  - classify pass+skip (no fail/pending) as aggregate pass
  - report no-review as `none`
  - separate hard blocking reasons from advisory/transient reasons
  - retry mergeability briefly before final report classification

## Acceptance Targets

1. `atm gh pr list [--json]` returns accurate CI/merge/review rollups for open PRs.
2. `atm gh pr report <PR> [--json]` returns detailed check/report output with links/timing.
3. No daemon requirement for one-shot reporting commands.
4. Notification behavior is unchanged for daemon monitor mode.
5. `atm gh pr report <PR> --template <path>` renders using the same payload schema as `--json`.
6. `atm gh pr init-report [--output <path>]` writes a usable starter template for report customization.
7. `atm gh pr report <PR> --json` includes top-level `schema_version`.
8. `atm gh pr report <PR>` reports pass+skip as CI pass and includes skip
   count explicitly.
9. No-review state is emitted as `review_decision = none` and does not create a
   hard merge blocker by itself.
10. Report output separates hard blockers from advisory/transient reasons.
