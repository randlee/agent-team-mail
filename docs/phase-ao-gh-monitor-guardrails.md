# Phase AO Guardrails Sprint Notes

> **Status**: Draft planning note
> **Scope**: daemon runtime guardrails and `gh_monitor` observability follow-up

This note captures the current decision for the next short follow-on sprints.
It is intentionally narrow: stop accidental shared-runtime launches first, then
make GitHub query sources visible and stoppable.

## AO.1 Shared Runtime Admission Guard

Goal: prevent accidental extra daemons from joining shared runtimes.

Integration branch: `integrate/phase-AO`

Rules:
- ATM supports exactly two shared runtimes: `release` and `dev`.
- Both shared runtimes must use release-built binaries.
- `release` must run only from the approved installed release location.
- `dev` must run only from the approved installed dev location.
- A normal shared-runtime launch must hard-stop unless all of these are true:
  - release build
  - approved binary location for the target runtime
  - no other daemon already owns that shared runtime

Acceptance:
- Requirements covered: `GH-CI-FR-35`, `GH-CI-FR-36`
- shared runtime lease acquisition uses `acquire_lock` + re-read + fsync +
  `atomic_swap` discipline (`GH-CI-FR-35`)
- repo/worktree binaries cannot start against shared `release` or `dev`
- second daemon for shared `release` or `dev` fails loudly
- daemon status records runtime owner metadata sufficient to explain refusal

## AO.2 Isolated Runtime Creation and TTL

Goal: make testing/smoke/debug runs explicit, short-lived, and harmless.

Integration branch: `integrate/phase-AO`

Rules:
- Anything that is not the approved shared `release` or `dev` runtime is
  classified as `isolated`.
- Isolated runtime creation must be explicit through ATM tooling.
- Each isolated runtime gets its own `ATM_HOME`, socket, lock, and status path.
- Default isolated-runtime TTL is `10 minutes`.
- Expired isolated runtimes are cleanup-eligible immediately.
- Isolated runtimes must not use shared GitHub polling/account access by
  default.
- `GhMonitorHealthRecord.in_flight` must be wired to the real in-flight request
  count instead of remaining hardcoded to `0`.

Acceptance:
- Requirements covered: `GH-CI-FR-26`, `GH-CI-FR-37`, `GH-CI-FR-38`
- ATM can create an isolated runtime root with runtime metadata
- runtime metadata includes `created_at` and `expires_at`
- expired + dead isolated runtimes are automatically reaped or flagged for
  immediate cleanup
- isolated runtimes do not enable live `gh_monitor` polling unless explicitly
  allowed

## AO.3 Shared Repo-State, Budgeting, and Observability

Goal: make GitHub query sources attributable without spending extra API budget
in `doctor`.

Integration branch: `integrate/phase-AO`

Rules:
- Each team gets a fixed GitHub budget of `100 calls/hour`.
- Team-lead for the affected team is warned at `50%` budget usage.
- The monitor hard-blocks further GitHub calls for that team at `100%`.
- All `gh` calls for a `(team, repo)` flow through one shared gate for:
  - budget
  - cadence
  - authorization
  - lease ownership
- Shared repo-state is a short-lived cache, not a direct GitHub query surface.
- Shared repo-state TTL is `5 minutes`.
- If CLI demand arrives and the shared repo-state is older than `1 minute`,
  refresh is allowed if the gate permits it.
- Stale repo-state entries must be evicted after TTL.
- Every `gh_monitor` GitHub CLI call must be attributed locally to:
  - team
  - repo
  - branch/ref when known
  - daemon/runtime owner
  - action
  - duration_ms
  - success/failure
- Per-team API call counts must be maintained locally, with repo and branch/ref
  breakdown when known.
- `atm doctor` and `atm gh status` must read cached local counters and cached
  repo-state rather than issuing normal live GH queries on demand.
- `atm doctor` makes exactly one live rate-limit audit call and compares that
  result to ATM's internal count/estimate.
- `sc-observability` events for this sprint must include planned
  `action` values:
  - `gh_api_call`
  - `rate_limit_warning`
  - `rate_limit_critical`

Acceptance:
- Requirements covered: `GH-CI-FR-10a`, `GH-CI-FR-10b`, `GH-CI-FR-10c`, `GH-CI-FR-22`, `GH-CI-FR-23`, `GH-CI-FR-24`, `GH-CI-FR-25`, `GH-CI-FR-26` (in_flight wiring prerequisite closed in AO.2; AO.3 completes full observability surface), `GH-CI-FR-27`, `GH-CI-FR-28`, `GH-CI-FR-39`, `GH-CI-FR-41`, `GH-CI-FR-42`, `GH-CI-FR-43`, `GH-CI-FR-44`, `GH-CI-FR-45`
- all monitor subscriptions attach to the same `(team, repo)` shared poller (`GH-CI-FR-10a`)
- primary poll surface is the repo-wide PR list view (`GH-CI-FR-10b`)
- idle `(team, repo)` polling occurs at most once per 5 minutes (`GH-CI-FR-10c`)
- active `(team, repo)` polling occurs at most once per 1 minute (`GH-CI-FR-10c`)
- `atm gh status` shows freshness metadata (`updated_at`) for GH-derived data
- `atm doctor` shows cached GH call counts and one live rate-limit audit sample
- operators can identify which runtime owns active polling
- operators can identify call volume by team, repo, and branch/ref when known
- pre-run and post-completion DIRTY merge-conflict checks stay on the attributed
  polling path and surface canonical merge-conflict alerts (`GH-CI-FR-41`,
  `GH-CI-FR-42`)
- CLI/daemon config discovery parity and `atm gh init` config file selection are
  preserved under the AO.3 attributed `gh` path (`GH-CI-FR-43`,
  `GH-CI-FR-44`)
- legacy direct `gh` helper paths (`run_gh_command`,
  `run_gh_command_for_repo` in `gh_monitor.rs`) are eliminated or rerouted
  through the attributed provider `run_gh()` path (`GH-CI-FR-45`)

## AO.4 Operator Shutdown and Lease Control

Goal: make runaway polling stoppable without granting ordinary teams cross-team
authority.

Integration branch: `integrate/phase-AO`

Rules:
- Only one active `gh_monitor` lease may exist per `(team, repo)`.
- Team-local stop/disable is allowed for that team.
- Cross-team stop/disable is CLI-only and hidden from normal usage/help.
- Cross-team stop/disable requires explicit human authorization intent, e.g.
  `--user-authorized`.
- If one team disables another team's monitor, the affected team's lead must be
  notified with actor and reason.
- All operator shutdown actions must be auditable.

Acceptance:
- Requirements covered: `GH-CI-FR-20`, `GH-CI-FR-21`, `GH-CI-FR-29`
- duplicate `(team, repo)` monitor ownership fails loudly
- operator-facing status shows `runtime_kind`, `PID`, `binary_path`,
  `ATM_HOME`, `team`, `repo`, and `poll_interval` for the active `(team, repo)`
  polling owner (`GH-CI-FR-21`)
- hidden operator stop path exists for emergency shutdown
- affected team lead receives notification identifying actor and reason
