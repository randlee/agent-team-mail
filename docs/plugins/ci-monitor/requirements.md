# GitHub CI Monitor Plugin Requirements

> **Status**: Locked (AB.1)
> **Scope**: GitHub CI monitoring behavior, plugin-owned `atm gh` command namespace, and observability contract

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
- `atm gh monitor pr <number>`
- `atm gh monitor workflow <name> --ref <branch|sha|pr>` (`--ref` required)
- `atm gh monitor run <run-id>`
- `atm gh status <pr|run|workflow> <value>`

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
- classification (`test_fail`, `infra`, `timeout`, `cancelled`, `ci_not_started`, etc.)
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

## 10. Test Requirements

### GH-CI-TR-1 Availability transitions

Test:
- `healthy -> degraded`
- `degraded -> healthy`
- `healthy -> disabled_config_error`

Each transition must verify:
- status surface update
- structured log emission
- ATM notification behavior

### GH-CI-TR-2 Command behavior

Test:
- `monitor pr` start-timeout/no-run alert
- `monitor workflow` by name/ref
- `monitor run` by run-id
- `status` output coherence during active and terminal runs

### GH-CI-TR-3 Reporting payload

Test:
- 1/minute progress throttle
- immediate terminal update
- final summary table fields
- required failure URLs and metadata fields

### GH-CI-TR-4 Failure isolation

Test:
- plugin init failure does not crash daemon startup
- plugin runtime failure does not terminate daemon process
- unrelated plugins continue running when `gh_monitor` fails

### GH-CI-TR-5 Runtime drift baselines

Test:
- deterministic drift alert emission for a run exceeding configured threshold
- baseline/history persistence across plugin restart
- run dedup persistence across restart (same run ID is not reprocessed)
