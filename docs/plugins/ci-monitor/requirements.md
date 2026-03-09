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
- `atm gh` (namespace status summary; no subcommand required)
- `atm gh init` (configure/enable plugin prerequisites)
- `atm gh monitor pr <number>`
- `atm gh monitor workflow <name> --ref <branch|sha|pr>` (`--ref` required)
- `atm gh monitor run <run-id>`
- `atm gh monitor list [--json] [--limit <N>]` (default limit: 20)
- `atm gh monitor report <pr-number> [--json] [--template <path>]`
- `atm gh monitor init-report [--output <path>]`
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

### GH-CI-FR-21 `atm gh init` prerequisites and failure contract

- `atm gh init` must validate:
  - `gh --version` is executable
  - `gh auth status` succeeds
- If prerequisites fail, command must exit with actionable remediation (install
  `gh` or run `gh auth login`).

### GH-CI-FR-22 `atm gh init` write contract

- Non-dry-run init must ensure `[plugins.gh_monitor]` exists and write/retain:
  - `enabled = true`
  - `provider = "github"`
  - `team = <team>`
  - `agent = "gh-monitor"`
  - `repo = <repo>`
  - optional `owner = <owner>`
  - default `poll_interval_secs = 60` when absent
  - default `notify_target = "team-lead"` when absent
- `--dry-run` must not mutate filesystem.

### GH-CI-FR-23 `atm gh init` output contract

- Text and JSON outputs must include deterministic setup summary:
  - `team`, `config_path`, `dry_run`, `created`, `gh_installed`,
    `gh_authenticated`, `owner`, `repo`, `notify_target`, `next_steps`
- JSON output must be machine-readable and stable for automation.

### GH-CI-FR-24 Plugin unavailability JSON error contract

- For `--json` invocations of unavailable operations (for example
  `atm gh monitor ... --json` when plugin is disabled/unconfigured), command
  failure output must be structured JSON on stderr:
  - `error_code = "PLUGIN_UNAVAILABLE"`
  - `message` (specific unavailability reason)
  - `hint` (must include `atm gh init`)

### GH-CI-FR-25 Report and template rendering contracts

`atm gh monitor report <pr-number>` one-shot reporting:
- In text mode: prints a human-readable PR status summary to stdout.
- In `--json` mode: outputs a `GhMonitorReportSummary` JSON object to stdout
  with top-level `schema_version` field (current: `"1.0.0"`).
- `--template <path>` renders the report using the specified Jinja2 template
  file with the same payload schema as `--json`.
- `--template` and `--json` are mutually exclusive; combining them is an error.
- When template file is missing or rendering fails (text mode): non-zero exit
  with human-readable error on stderr.
- `list` and `report` are one-shot read/report commands requiring no daemon.

`atm gh monitor init-report [--output <path>]`:
- Writes a starter report template to the specified path (default:
  `gh-monitor-report.md.j2` in current directory).
- If the output file already exists, the command fails with a non-zero exit and
  a message indicating the path; the file is not overwritten.

## 12. Test Requirements

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
- `atm gh` and `atm gh status` (no target) return non-error health output and
  explicitly surface configured/enabled/availability state
- when plugin is disabled/unconfigured:
  - `atm gh monitor ...` fails with actionable init guidance
  - `atm gh status <target>` fails with actionable init guidance
  - `atm gh init` remains available and succeeds/fails deterministically
- `status` output coherence during active and terminal runs

### GH-CI-TR-3 Reporting payload

Test:
- 1/minute progress throttle
- immediate terminal update
- final summary table fields
- required failure URLs and metadata fields

### GH-CI-TR-4 Merge-conflict detection

Test:
- preflight DIRTY PR emits merge-conflict alert and skips CI polling
- clean PR preflight proceeds to CI polling (no merge-conflict alert)
- post-completion DIRTY re-check emits merge-conflict alert with run conclusion
- clean terminal PRs are unaffected

### GH-CI-TR-5 Failure isolation

Test:
- plugin init failure does not crash daemon startup
- plugin runtime failure does not terminate daemon process
- unrelated plugins continue running when `gh_monitor` fails

### GH-CI-TR-6 Runtime drift baselines

Test:
- deterministic drift alert emission for a run exceeding configured threshold
- baseline/history persistence across plugin restart
- run dedup persistence across restart (same run ID is not reprocessed)

### GH-CI-TR-7 Config/init and JSON error contract

Test:
- CLI/daemon config-source parity for no-target status surfaces.
- `atm gh init --dry-run` leaves config files unchanged.
- `atm gh init` writes expected `gh_monitor` keys, including `notify_target`.
- `--json` unavailable monitor/status operations emit structured
  `PLUGIN_UNAVAILABLE` error payload on stderr.
