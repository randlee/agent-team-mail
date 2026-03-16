# GitHub CI Monitor Plugin Requirements

> **Status**: Locked (AB.1)
> **Scope**: GitHub CI monitoring behavior, plugin-owned `atm gh` command namespace, and observability contract
>
> Canonical companions:
> - `docs/ci-monitoring/architecture.md`
> - `docs/ci-monitoring/adr.md`

---

## 1. Purpose

Define complete requirements for GitHub CI monitoring as a daemon plugin, separated
from core ATM requirements.

This document is the normative source for:
- monitor lifecycle state
- monitor command behavior
- failure/progress payload content
- connectivity and recovery signaling
- shared-runtime polling guardrails
- cached GitHub API usage observability

---

## 2. Dependencies and Authority

- Platform contracts still apply from `docs/requirements.md` (daemon lifecycle,
  plugin isolation, logging, roster/mail invariants).
- If requirements conflict, platform safety invariants apply first, then this
  plugin contract.

---

## 3. Configuration

### GH-CI-FR-1 Canonical config key

- Canonical key for the GitHub implementation is `[plugins.gh_monitor]`.
- The same key must be used by parser, daemon plugin registration, examples,
  and tests.
- `ci_monitor` is the shared contract/interface name used for cross-provider
  behavior and schema, not the GitHub concrete plugin key.

### GH-CI-FR-2 Required baseline config

Minimum valid configuration must include:
- target `team`
- synthetic sender `agent`
- provider (`github` default allowed)
- designated monitor recipient(s); when omitted, default is `team-lead@<team>`

### GH-CI-FR-3 Invalid configuration behavior

If configuration is invalid:
- plugin enters `disabled_config_error`
- plugin emits operator-visible warning/error (structured log + status surface)
- plugin does not run polling loop (zero steady-state polling CPU)

---

## 4. Availability and Connectivity

### GH-CI-FR-4 Availability states

The plugin must expose one canonical state:
- `healthy`
- `degraded`
- `disabled_config_error`

### GH-CI-FR-5 Transient failure behavior

Connectivity/auth/rate-limit/provider transient failures must:
- transition state `healthy -> degraded`
- emit structured log event
- send ATM mail to monitor recipient(s)
- continue retrying with bounded backoff

### GH-CI-FR-6 Recovery behavior

When failures clear:
- transition state `degraded -> healthy`
- emit structured log event
- send ATM mail recovery notification to monitor recipient(s)

---

## 5. CLI Namespace and Commands

### GH-CI-FR-7 Plugin-owned namespace

GitHub CI monitor exclusively owns:
- `atm gh`

### GH-CI-FR-8 Command set

Required commands:
- `atm gh` (namespace status summary; no subcommand required)
- `atm gh init` (configure/enable plugin prerequisites)
- `atm gh monitor pr <number>`
- `atm gh monitor workflow <name> --ref <branch|sha|pr>` (`--ref` required)
- `atm gh monitor run <run-id>`
- `atm gh pr list [--json] [--limit <N>]` (default limit: 20)
- `atm gh pr report <pr-number> [--json] [--template <path>]`
- `atm gh pr init-report [--output <path>]`
- `atm gh status` (team/plugin health status; no target required)
- `atm gh status <pr|run|workflow> <value>` (target-specific monitor state)

No-target status requirements:
- `atm gh` and `atm gh status` must report the same canonical enablement and
  availability surface for `gh_monitor`.
- If plugin is unconfigured/disabled, commands must return actionable status
  output (not argument errors), including explicit disabled state and setup hint.
- JSON mode must include machine-readable `configured`, `enabled`, and
  `availability_state` fields.
- If plugin is unconfigured/disabled, command availability is restricted to:
  - `atm gh`
  - `atm gh init`
  - help output under `atm gh`
  All other `atm gh ...` operations must fail with explicit guidance to run
  `atm gh init`.
- When plugin is enabled, `atm gh` must include configuration summary, runtime
  availability summary, and current issue note (when present).
- This behavior must conform to the global plugin namespace gating contract in
  `docs/requirements.md` §5.8.

---

## 6. Monitor Semantics

### GH-CI-FR-9 PR start timeout

For `atm gh monitor pr <number>`:
- default start timeout is `2m` (override via `--start-timeout`)
- if no matching workflow run starts in window:
  - emit structured log event (`ci_not_started`)
  - send ATM notification to monitor recipient(s)

### GH-CI-FR-10 Progress cadence

While monitoring active runs:
- progress updates must be rate-limited to no faster than 1/minute
- update should include all job completions since last report
- terminal completion/failure update must be sent immediately

### GH-CI-FR-10a Shared repo poller

`gh_monitor` must operate as one shared poller per `(team, repo)`, not one
independent poller per CLI request or teammate.

Individual requests such as:
- `atm gh monitor pr <number>`
- `atm gh monitor workflow <name> --ref <ref>`
- `atm gh monitor run <run-id>`

must register monitoring interest/subscriptions against the same shared poller.

### GH-CI-FR-10b Primary polling surface

The primary polling surface for the shared poller must be the repo-wide PR list
view used by `atm gh pr list`.

The poller should prefer one broad repo query that can satisfy multiple active
subscriptions. Narrower follow-up calls are allowed only when additional detail
is required beyond the shared list surface.

### GH-CI-FR-10c Polling cadence by demand level

For each `(team, repo)`:
- when no active monitor subscription exists, polling must be rate-limited to
  no more than once every `5 minutes`
- when one or more active monitor subscriptions exist, polling must be
  rate-limited to no more than once every `1 minute`

If a new monitor request arrives and no poll has occurred within the active
window, the poller may execute an immediate refresh.

If a poll has already occurred within the active window, the request must reuse
cached state instead of triggering an extra GitHub query.

### GH-CI-FR-11 Final completion summary

Terminal report must include table with:
- job/test name
- terminal status
- runtime/duration

---

## 7. Notification Payload Contract

### GH-CI-FR-12 Required failure fields

Failure notifications must include:
- run URL (always)
- failed job URL(s), when available
- PR URL (for PR monitor mode)
- workflow name
- job name(s)
- run id and attempt
- branch
- commit SHA (short + full)
- classification (`test_fail`, `infra`, `timeout`, `cancelled`, `ci_not_started`, `merge_conflict`, etc.)
- first failing step name (if available)
- short bounded log excerpt
- correlation/message id
- next-action hint command

The same structured content must be emitted to logs.

### GH-CI-FR-13 Progress payload

Progress updates must include:
- run/workflow identity
- completed/total job count
- newly completed job names + statuses
- run URL

---

## 8. Repo and Daemon Boundary Rules

### 8.1 Base daemon boundary

### GH-CI-FR-14 Repo context requirement

CI monitor requires repo context:
- if repo context missing, plugin must disable gracefully with actionable warning
- plugin must not crash daemon

### GH-CI-FR-15 Daemon safety

Plugin init/runtime failure must never crash daemon or block unrelated plugins.

### 8.2 Runtime guardrails and operator controls

### GH-CI-FR-20 Single polling owner

Only one active `gh_monitor` polling owner may exist per `(team, repo)` at a
time.

If a second daemon instance attempts to start `gh_monitor` for the same
`(team, repo)`:
- startup/init must fail or defer loudly
- the conflict must be visible in operator status surfaces

### GH-CI-FR-21 Runtime owner visibility

Operator-facing status must show enough metadata to identify the active polling
source, including at minimum:
- runtime kind (`release`, `dev`, `isolated`)
- daemon PID
- binary path
- `ATM_HOME`
- team
- repo
- poll interval

### GH-CI-FR-22 Team budget

Each team must have a fixed local GitHub call budget.

The upstream GitHub token ceiling is shared across all callers using the same
token. ATM must therefore treat the token-level budget as a global shared
resource, not as a per-daemon or per-repo allowance.

Initial default:
- `100 calls/hour` per team

Behavior:
- warn the team's lead at `50%`
- hard-block further `gh_monitor` GitHub calls for that team at `100%`
- budget enforcement must apply to all GitHub-monitor-related calls, not just
  one command family

### GH-CI-FR-23 Shared repo-state cache

For each `(team, repo)`, ATM must maintain a shared repo-state cache that backs
CLI and monitor responses.

Requirements:
- cache TTL is `5 minutes`
- if explicit CLI demand arrives and cache age is greater than `1 minute`, a
  refresh may occur if the shared gate permits it
- if cache age is `1 minute` or less, responses should reuse cached state
- stale entries must be evicted after TTL
- Cache TTL eviction MUST NOT reset or clear accumulated rate-budget state.
  Budget counters are reset only by explicit operator action or hourly rollover
  as defined in `docs/requirements.md` §6.2.

`monitor pr`, `monitor workflow`, and `monitor run` must all use the same
shared team/repo gate even when they require different underlying GitHub query
shapes.

### GH-CI-FR-24 Primary repo poll surface

Where applicable, the primary shared poll surface should be the repo-wide PR
list view used by `atm gh pr list`.

Multiple teammate subscriptions must attach to the same underlying team/repo
poller instead of creating parallel GitHub query loops.

### GH-CI-FR-25 Local API call attribution

Every GitHub API/CLI call issued by `gh_monitor` must be attributed locally to:
- team
- repo
- branch or target ref when known
- daemon/runtime owner
- action
- duration
- success/failure

This attribution must be recorded without requiring a separate GitHub query to
reconstruct the source later.

The shared repo poller call must be counted once per `(team, repo)` refresh,
not once per teammate subscription attached to that poller.

### GH-CI-FR-26 Cached doctor/status observability

`atm doctor` and `atm gh status` must report GitHub API usage from locally
cached monitor state, not by issuing live GitHub API calls on demand.

Required cached fields:
- API calls made in the current local accounting window
- counts by repo and branch/ref when known
- `in_flight` call count
- current poll interval
- cached rate-limit snapshot (`remaining`, `limit`, `reset_at`) when available
- active lease/runtime owner
- `updated_at` freshness timestamp for GH-derived state

### GH-CI-FR-27 Doctor audit call

`atm doctor` must make exactly one live GitHub rate-limit call to audit ATM's
internal counter accuracy.

Doctor output must include:
- live `remaining/limit`
- ATM internal counted/estimated usage
- comparison/delta between the two

### GH-CI-FR-28 Bounded rate-limit refresh

If GitHub rate-limit information is refreshed live, it must be refreshed by the
monitor polling path at a bounded cadence and cached for later surfaces.

`atm doctor` itself must not spend additional GitHub API budget merely to report
current API usage health.

### GH-CI-FR-29 Hidden operator control

Cross-team stop/disable control must be:
- CLI-only
- hidden from normal help/usage
- explicitly human-authorized
- auditable

If one team disables another team's monitor, the affected team's lead must be
notified with actor identity and reason.

### 8.3 AO guardrails additions

### GH-CI-FR-35 Shared runtime admission

ATM supports exactly two shared runtimes for live GitHub polling:
- `release`
- `dev`

A normal shared-runtime launch must hard-stop unless all of these are true:
- release-built binary
- approved installed location for the target shared runtime
- no other daemon already owns that shared runtime

Repo/worktree/ad hoc binaries must not start as shared runtime owners.

Shared runtime admission and `(team, repo)` lease acquisition must use
`acquire_lock` plus re-read plus fsync plus `atomic_swap` write discipline to
prevent split-ownership races.

### GH-CI-FR-36 Isolated runtime classification

Any runtime that is not the approved shared `release` or `dev` runtime is
classified as `isolated`.

Isolated runtimes must:
- use their own `ATM_HOME`
- use their own lock/socket/status paths
- be created explicitly through ATM tooling, not by accidental inheritance
- be marked as isolated in runtime metadata

### GH-CI-FR-37 Isolated runtime TTL

Isolated runtimes are short-lived leases, not long-lived shared environments.

Requirements:
- default TTL is `10 minutes`
- runtime metadata must record `created_at` and `expires_at`
- expired isolated runtimes must be cleanup-eligible immediately
- expired + dead isolated runtimes should be automatically reaped or loudly
  surfaced for cleanup

### GH-CI-FR-38 Isolated GitHub policy

Isolated runtimes must not use shared GitHub polling/account access by default.

Specifically:
- `gh_monitor` must start disabled in isolated runtimes unless explicitly
  allowed
- isolated runtimes must not silently inherit live polling authority from the
  shared `release` or `dev` runtime

### GH-CI-FR-39 Observability action contract

GitHub monitor observability must emit `LogEventV1` records using the canonical
stable identifier field `action`, not a separate `event_type` field.

AO.3 reserves these `action` strings:
- `gh_api_call`
- `rate_limit_warning`
- `rate_limit_critical`

Required payload fields:
- `team`
- `repo`
- `branch` or `target_ref` when known
- `runtime_kind`
- `binary_path`
- `poll_interval_secs` when emitted from the shared poller
- `action` containing the `gh` subcommand and arguments (for example `run list`
  or `pr view`)
- `duration_ms` for `gh_api_call`
- `success` for `gh_api_call`
- `remaining`, `limit`, and `reset_at` for rate-limit actions
- `budget_used`, `budget_limit`, and `budget_window` for rate-limit actions

---

## 9. Runtime Drift Alerts (Optional Enhancement)

### GH-CI-FR-40 Historical runtime baseline (`SHOULD`)

Plugin should maintain per-workflow/job timing baselines and alert when a run is
significantly slower than historical norm (policy-configurable).

Runtime drift policy config keys:
- `runtime_drift_enabled` (default `false`)
- `runtime_drift_threshold_percent` (integer > 0, default `50`)
- `runtime_drift_min_samples` (integer >= 1, default `3`)
- `runtime_history_limit` (integer >= 1, default `50`)

Persistence behavior:
- Runtime baselines must persist across plugin restarts.
- Processed run IDs must persist so the same run does not repeatedly mutate
  baselines or spam drift alerts after restart.
- Runtime baseline history file location: `<report_dir>/runtime-history.json`.

---

## 10. PR Merge-Conflict Detection

### GH-CI-FR-41 Pre-run DIRTY preflight

For `atm gh monitor pr <number>`:
- daemon must query PR `mergeStateStatus` before CI start-window polling begins
- if `mergeStateStatus=DIRTY`:
  - emit merge-conflict alert (`classification=merge_conflict`, `status=merge_conflict`)
  - include `pr_url` and `merge_state_status` in alert/log payload
  - persist monitor state as `merge_conflict`
  - skip CI start-window polling
  - skip `ci_not_started` alert for that invocation

### GH-CI-FR-42 Post-completion DIRTY re-check

After a monitored PR run reaches terminal state:
- daemon must re-query PR `mergeStateStatus`
- if `mergeStateStatus=DIRTY`, emit an additional merge-conflict alert
- post-completion alert payload must include:
  - `classification=merge_conflict`
  - `status=merge_conflict`
  - `pr_url`
  - `merge_state_status`
  - `run_conclusion`

---

## 11. Config Discovery and Initialization

### GH-CI-FR-43 Config discovery parity (CLI and daemon)

- CLI command paths (`atm gh`, `atm gh status`, `atm gh monitor ...`) and daemon
  plugin bootstrap must resolve `gh_monitor` configuration from the same
  location precedence and same team scope.
- Status surfaces must report `configured`, `enabled`, `config_source`, and
  `config_path` from that canonical resolution result.

### GH-CI-FR-44 `atm gh init` config file selection

- `atm gh init` must write to the canonical plugin config location:
  - existing plugin config file when already present
  - else repo `.atm.toml` at git root when available
  - else existing global config (`~/.config/atm/config.toml`) when present
  - else local `.atm.toml` in current directory
- Command must create parent directories as needed.

### GH-CI-FR-45 Attributed `gh` invocation path

All `gh` CLI invocations inside the `gh_monitor` subsystem must go through the
attributed `run_gh()` path in the GitHub provider implementation.

Direct `Command::new("gh")` calls outside the `CiProvider` trait
implementation are prohibited for `gh_monitor` behavior because they bypass:
- team budgeting
- repo-state freshness gating
- attribution/log emission
- `in_flight` accounting

Existing bypass helpers must be eliminated or rerouted through the attributed
provider path during Phase AO.

### GH-CI-FR-46 Hard GitHub execution firewall

ATM must enforce one mandatory execution firewall between a requested GitHub
operation and the actual `gh` subprocess call.

Requirements:
- the canonical execution adapter is the existing
  `GhCliObserver::before_gh_call` gate paired with
  `GitHubActionsProvider::run_gh_with_metadata`
- every real `gh` subprocess invocation for `gh_monitor`, `atm gh ...` command
  paths, repo-state refresh, and GitHub budget/rate auditing must pass through
  one canonical adapter
- that adapter must make the allow/deny decision before launching `gh`
- direct `Command::new("gh")` execution outside the canonical adapter is a
  blocking violation for these flows
- blocked requests must fail loudly with a machine-readable reason rather than
  silently degrading into an untracked subprocess path

### GH-CI-FR-47 GitHub execution ledger

ATM must emit a complete local execution ledger for real `gh` subprocess calls.

The execution ledger is the token-correlated source of truth and must record
every allowed and blocked GitHub call attempt with at least:
- `call_id` (UUID v4)
- timestamp
- team
- repo
- runtime kind
- daemon PID
- executable path
- caller/subsystem
- poller key when applicable
- exact argv / subcommand
- lifecycle state
- `in_flight`
- budget/rate snapshot before and after when available
- duration
- success/failure or block reason

Ownership and write discipline:
- owning crate: `crates/atm-ci-monitor`
- storage format: append-only JSONL under the runtime directory
- synchronization model: all producers send execution records through a channel
  to one writer task; only that writer task may mutate the execution ledger

If GitHub token consumption moves, a matching execution-ledger record must
exist or the call path is considered a firewall bypass defect.

### GH-CI-FR-48 GitHub info/freshness ledger

ATM must also emit a separate information/freshness ledger for GitHub-backed
status requests.

Purpose:
- show what info the caller requested
- show whether ATM answered from cache or required a live refresh
- show how stale the returned data was
- show when policy/budget/lifecycle prevented a live refresh

Required fields:
- `request_id` (UUID v4)
- caller/subsystem
- requested info type
- team/repo/ref scope
- cache hit vs live refresh
- cache age / staleness
- degraded/denied reason when a live refresh is blocked
- linked `call_id` values for any real `gh` subprocesses triggered by that
  request

Ownership and write discipline:
- owning crate: `crates/atm-ci-monitor`
- storage format: append-only JSONL under the runtime directory
- synchronization model: all producers send freshness records through a
  channel to one writer task; only that writer task may mutate the freshness
  ledger

### GH-CI-FR-49 Lifecycle suppression of GitHub polling

The shared repo poller must not continue issuing GitHub calls when monitor
lifecycle state is not actively running.

Requirements:
- lifecycle `stopped` forbids new shared-poller `gh` calls
- lifecycle `draining` forbids starting new poll cycles while existing in-flight
  work drains
- stop/restart control paths must suspend, clear, or otherwise prevent
  lingering active monitor records from keeping the shared poller alive
- `in_flight` accounting must reflect only work that is still permitted to poll

### GH-CI-FR-50 Per-monitor cap and global headroom policy

GitHub monitor budgeting must include both per-monitor pacing and global token
headroom protection.

Requirements:
- define a per-active-monitor maximum call cadence or budget envelope
- define a global headroom floor below which shared polling pauses or degrades
  instead of continuing at normal cadence
- lock the following defaults in `crates/atm-ci-monitor`:
  - `GH_MONITOR_PER_ACTIVE_MONITOR_MAX_CALLS = 6`
  - `GH_MONITOR_HEADROOM_FLOOR = 200`
  - `GH_MONITOR_HEADROOM_RECOVERY_FLOOR = 300`
  - `GH_MONITOR_SINGLE_SMOKE_MAX_CALLS = 10`
  - `GH_MONITOR_MULTI_SMOKE_MAX_CALLS = 30`
  - `GH_MONITOR_POST_STOP_MAX_DELTA = 5`
- the policy must distinguish single-monitor smoke expectations from
  multi-monitor/shared-poller expectations
- the chosen thresholds and constants must be documented and testable

### GH-CI-FR-51 Execution ledger action registry

Reserved execution-ledger `action` strings are:
- `gh_call_blocked`
- `gh_call_started`
- `gh_call_finished`

Required payload behavior:
- `gh_call_blocked` must include requested `argv`, caller/subsystem, runtime
  metadata, and `block_reason`
- `gh_call_started` must include exact `argv`, caller/subsystem, lifecycle
  state, and pre-call budget/rate snapshot when available
- `gh_call_finished` must include duration/result and post-call budget/rate
  snapshot when available

### GH-CI-FR-52 Freshness ledger action registry

Reserved freshness-ledger `action` strings are:
- `gh_info_requested`
- `gh_info_served_from_cache`
- `gh_info_live_refresh`
- `gh_info_degraded`
- `gh_info_denied`

Required payload behavior:
- `gh_info_requested` must identify the caller, requested info type, and repo
  scope
- `gh_info_served_from_cache` must include cache age / freshness metadata
- `gh_info_live_refresh` must link the request to the resulting `call_id`
- `gh_info_degraded` must explain why cached or partial data was returned
- `gh_info_denied` must explain why no useful answer was returned
