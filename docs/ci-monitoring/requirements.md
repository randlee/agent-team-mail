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

### GH-CI-FR-14 Repo context requirement

CI monitor requires repo context:
- if repo context missing, plugin must disable gracefully with actionable warning
- plugin must not crash daemon

### GH-CI-FR-15 Daemon safety

Plugin init/runtime failure must never crash daemon or block unrelated plugins.

---

## 9. Runtime Drift Alerts (Optional Enhancement)

### GH-CI-FR-16 Historical runtime baseline (`SHOULD`)

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

### GH-CI-FR-17 Pre-run DIRTY preflight

For `atm gh monitor pr <number>`:
- daemon must query PR `mergeStateStatus` before CI start-window polling begins
- if `mergeStateStatus=DIRTY`:
  - emit merge-conflict alert (`classification=merge_conflict`, `status=merge_conflict`)
  - include `pr_url` and `merge_state_status` in alert/log payload
  - persist monitor state as `merge_conflict`
  - skip CI start-window polling
  - skip `ci_not_started` alert for that invocation

### GH-CI-FR-18 Post-completion DIRTY re-check

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

### GH-CI-FR-19 Config discovery parity (CLI and daemon)

- CLI command paths (`atm gh`, `atm gh status`, `atm gh monitor ...`) and daemon
  plugin bootstrap must resolve `gh_monitor` configuration from the same
  location precedence and same team scope.
- Status surfaces must report `configured`, `enabled`, `config_source`, and
  `config_path` from that canonical resolution result.

### GH-CI-FR-20 `atm gh init` config file selection

- `atm gh init` must write to the canonical plugin config location:
  - existing plugin config file when already present
  - else repo `.atm.toml` at git root when available
  - else existing global config (`~/.config/atm/config.toml`) when present
  - else local `.atm.toml` in current directory
- Command must create parent directories as needed.
