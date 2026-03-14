# CI Monitoring Architecture

> **Status**: Canonical architecture document
> **Scope**: subsystem structure, provider boundaries, notification flow, and
> daemon integration for CI monitoring
>
> Normative companions:
> - `docs/ci-monitoring/requirements.md`
> - `docs/ci-monitoring/adr.md`
> - `docs/requirements.md`
>
> Naming lock: `gh_monitor` is the concrete GitHub config key, while
> `ci_monitor` is the shared contract/interface label.

## Scope and Goals
This document defines how the CI monitoring subsystem integrates into ATM and
how the GitHub implementation fits that architecture. It focuses on:
- Configuration via `.atm.toml`
- Notification flow to team agents
- Replacing manual CI polling
- Daemon deployment expectations
- Gaps between current implementation and required behavior
- subsystem decomposition and provider boundaries
- runtime guardrails for shared vs isolated polling
- cached observability for GitHub API usage

Out of scope: adding new CI providers beyond GitHub Actions (unless required for local projects).

---

## 1. Configuration (.atm.toml)
The GitHub CI Monitor plugin is configured under `[plugins.gh_monitor]`. Current fields (per `crates/atm-daemon/src/plugins/ci_monitor/config.rs`):

```toml
[plugins.gh_monitor]
enabled = true
provider = "github"              # built-in provider (gh CLI)
poll_interval_secs = 60           # minimum 10
team = "atm-phase8"              # target team for notifications
agent = "ci-monitor"             # synthetic sender name
watched_branches = ["main"]      # empty = all
notify_on = ["failure", "timed_out"]
# Optional overrides (auto-detected from git remote if missing)
owner = "randlee"
repo = "agent-team-mail"
# Provider config / extension
providers = { github = "" }       # external libraries if any
# Dedup
# per_commit => notify once per commit+conclusion
# per_run => notify once per run_id+conclusion
# Default per_commit
# dedup_strategy = "per_commit"
# dedup_ttl_hours = 24
# Reports
report_dir = "temp/atm/ci-monitor"
```

Required additions in Phase 9:
- `notify_target`: explicit routing target(s) for CI alerts (team lead and/or CI agent)
- Branch matching: support client-side glob/wildcard filtering via `globset` or `wildmatch`

Recommended additions for our repo use:
- `watched_branches`: include `develop`, `main`, `feature/*` if we want branch-wide coverage.
- `notify_on`: add `cancelled` and `action_required` if team wants notification for those outcomes.
- `dedup_strategy = "per_run"` for noisy CI jobs with frequent retries.

Notes:
- The plugin auto-detects owner/repo from git remote; these should not be required for standard usage.
- `team` is required and is the target team for the notifications.
- `agent` should map to a synthetic member in team config (RosterService already handles synthetic members).

---

## 2. Notification Flow (Agent Teams)

High-level flow:

1. `atm-daemon` runs the GitHub CI Monitor plugin.
2. Plugin polls CI provider on interval.
3. On matching event (`notify_on`), plugin creates an `InboxMessage` with:
   - `from = <ci-monitor agent name>`
   - summary like `[ci:<run_id>] <repo> <branch> failed`
   - `text` containing details and report location
4. Message is written to the team’s inbox directory.

Routing requirement:

- Current plugin sends to its own inbox. Add `notify_target` to route to team lead and/or CI agent.
- `notify_target` type: allow string or array of strings. Strings use `agent@team` format; if only `agent` provided, use configured `team`.
- Error handling: invalid target => config error; empty list => default to team lead.

---

## 3. Subsystem Boundary

The CI-monitoring subsystem should separate:

- provider-agnostic CI policy
- provider-specific fetch/translation logic
- daemon plugin lifecycle wiring
- routing/notification shaping
- health and status persistence

The subsystem should move toward:

- core CI service logic in one place
- GitHub adapter behind a provider boundary
- thin `socket.rs` dispatch for CI monitor commands

---

## 4. Provider Direction

Recommended split:

- provider-neutral CI service/core
- GitHub adapter
- future Azure adapter

The provider adapter should own:

- external CLI/API calls
- provider payload parsing
- provider-specific URL/status extraction

The provider-neutral core should own:

- monitor orchestration
- dedup strategy
- progress/failure classification
- notification/report decisions

---

## 5. Daemon Deployment

Goal: CI monitor should run continuously in the background.

Deployment model:

- `atm-daemon` is started by CLI when needed or at system login.
- Should run once per machine.
- Daemon should be resilient: auto-restart on failure.

Operational assumptions:

- `atm-daemon` is responsible for GitHub CI Monitor polling.
- Team agents do not need to poll CI directly once daemon is running.

### 5.1 Shared runtime policy

ATM has exactly two shared runtimes:
- `release`
- `dev`

Both shared runtimes are expected to run release-built binaries from approved
installed locations. Repo/worktree binaries are not valid shared-runtime
owners.

Shared-runtime admission should be enforced before daemon startup becomes live:
- validate runtime kind
- validate binary location/build class
- validate that no other daemon already owns the shared runtime

### 5.2 Isolated runtime policy

Any runtime outside the approved shared `release` or `dev` locations is
`isolated`.

Isolated runtimes should be created explicitly by ATM tooling with:
- separate `ATM_HOME`
- separate lock/socket/status paths
- runtime metadata including `created_at` and `expires_at`
- default TTL of `10 minutes`

Isolated runtimes are for smoke/debug/test use only and should not enable live
shared GitHub polling by default.

### 5.3 Operator shutdown model

Operator-facing status must identify:
- which runtime owns polling
- which repo/branch or target is being polled
- the daemon PID/binary/`ATM_HOME`
- the configured poll interval

This allows operators to stop the correct source without guessing.

---

## 6. GitHub API Observability

### 6.1 Call attribution

All GitHub CLI/API calls for `gh_monitor` should funnel through one attributed
call path.

The current natural funnel is `run_gh()` in the GitHub provider. That path
should attach local accounting metadata for:
- team
- repo
- branch or target ref when known
- action/arguments
- daemon/runtime owner
- duration
- success/failure

The steady-state polling path should use the repo-wide PR list surface as the
shared source of truth for monitor subscriptions. Teammate requests attach to
that shared poller instead of creating parallel GitHub query loops.

### 6.2 Local counters and cached rate-limit state

`atm doctor` should not issue its own live GitHub API calls to answer "who is
using GitHub?".

Instead, the monitor should maintain cached local state containing:
- fixed team budget state (initial default `100 calls/hour`)
- API calls this accounting window
- counts by repo and branch/ref when known
- `in_flight` calls
- cached `rate_limit` snapshot (`remaining`, `limit`, `reset_at`)
- current polling owner/lease

The polling loop may refresh `gh api rate_limit` at a bounded cadence and cache
the result for later reporting.

Recommended cadence:
- idle team/repo poller: at most once every `5 minutes`
- active team/repo poller with subscriptions: at most once every `1 minute`
- immediate refresh only when a new subscription arrives and the active-window
  cache is stale

The shared repo-state cache should have:
- TTL of `5 minutes`
- freshness timestamp (`updated_at`)
- eviction of stale entries after TTL
- demand-triggered refresh only when cache age exceeds `1 minute` and the
  shared gate permits the call

All GitHub-monitor-related queries, including workflow/run-specific lookups,
must pass through the same team/repo budget and authorization gate even when
their underlying GitHub query differs from the repo-wide PR list poll.

### 6.3 Status surfaces

`atm gh status` and `atm doctor` should read the cached monitor state and show:
- current poll interval
- active polling owner
- API call count for the window
- counts by repo and branch/ref when known
- cached rate-limit remaining/reset
- freshness timestamp for GH-derived state
- any duplicate-runtime or duplicate-lease conflict

`atm doctor` may perform one live `gh api rate_limit` audit call so the operator
can compare the live account state with ATM's internal counter/estimate.

---

## 7. Known Gaps

Known gaps:

1. Branch pattern semantics: GitHub API does not support branch glob filtering; implement client-side filtering.
2. Notification routing: plugin sends to its own inbox; add `notify_target`.
3. Daemon lifecycle: no explicit doc on daemon startup or managed service integration.
4. CI report retention: report files are stored, but retention policy is not specified.

Minor:

- If `gh` is missing or auth fails, plugin logs errors but no explicit user-facing notification to team lead.
