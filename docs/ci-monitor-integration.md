# GitHub CI Monitor Integration Design (Phase 9 Proposal)

## Scope and Goals
This document defines how the existing GitHub CI Monitor plugin integrates into team workflows and how Phase 9 delivers required system improvements. It focuses on:
- Configuration via `.atm.toml`
- Notification flow to team agents
- Replacing manual CI polling
- Daemon deployment expectations
- Gaps between current implementation and required behavior
- Phase 9 sprint plan (expanded to cover all required work areas)

Out of scope: adding new CI providers beyond GitHub Actions (unless required for local projects).

---

## 1. Configuration (.atm.toml)
The GitHub CI Monitor plugin is configured under `[plugins.ci_monitor]`. Current fields (per `crates/atm-daemon/src/plugins/ci_monitor/config.rs`):

```toml
[plugins.ci_monitor]
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
**High-level flow**:
1. `atm-daemon` runs the GitHub CI Monitor plugin.
2. Plugin polls CI provider on interval.
3. On matching event (`notify_on`), plugin creates an `InboxMessage` with:
   - `from = <ci-monitor agent name>`
   - summary like `[ci:<run_id>] <repo> <branch> failed` (current format in plugin)
   - `text` containing details and report location
4. Message is written to the team’s inbox directory.

**Routing requirement** (Phase 9):
- Current plugin sends to its own inbox. Add `notify_target` to route to team lead and/or CI agent.
- `notify_target` type: allow string or array of strings. Strings use `agent@team` format; if only `agent` provided, use configured `team`.
- Error handling: invalid target => config error; empty list => default to team lead.

---

## 3. Replacing Manual CI Polling
Current manual workflow relies on an agent to poll `gh` or GitHub UI for CI status.
The GitHub CI Monitor plugin replaces this with:
- daemon-driven polling
- automatic dedup (per run or per commit)
- report generation for diagnostics

**Expected outcomes**:
- no more manual “check CI” tasks
- failure notifications in near real time (per `poll_interval_secs`)
- single notification per failure (dedup)

**Baseline configuration**:
- poll interval: 60–120s
- notify_on: failure + timed_out

---

## 4. Daemon Deployment
**Goal**: CI monitor should run continuously in the background.

**Deployment model**:
- `atm-daemon` is started by CLI when needed or at system login.
- Should run once per machine (multi-repo support is deferred).
- Daemon should be resilient: auto-restart on failure (service templates deferred).

**Operational assumptions**:
- `atm-daemon` is responsible for GitHub CI Monitor polling.
- Team agents do not need to poll CI directly once daemon is running.

---

## 5. Gaps vs Current Implementation
**Known gaps**:
1. **Branch pattern semantics**: GitHub API does not support branch glob filtering; implement client-side filtering.
2. **Notification routing**: plugin sends to its own inbox; add `notify_target`.
3. **Daemon lifecycle**: no explicit doc on daemon startup or managed service integration.
4. **CI report retention**: report files are stored, but retention policy is not specified.

**Minor**:
- If `gh` is missing or auth fails, plugin logs errors but no explicit user-facing notification to team lead.

---

## 6. Phase 9 Sprint Structure (Expanded)
Phase 9 must cover five work areas and be ordered for stability. Foundational work ships first.

### Sprint 9.0: Phase 8.6 Verification Gate (NEW)
- Verify all Phase 8.6 fixes merged and green across CI
- Audit outstanding Phase 8 CI failures and close
- Exit criteria: PR(s) merged, CI green, no open P8 blocking issues

### Sprint 9.1: CI/Tooling Stabilization
- Commit `rust-toolchain.toml` to main repo
- Separate clippy into its own CI job with `needs: [clippy]`
- QA agent clippy gate stays enforced

Tests:
- 1 CI validation
- 1 QA gate

### Sprint 9.2: Home Dir Resolution (Cross-Platform)
- Create canonical `get_home_dir()` in `atm-core`
- Replace ALL call sites:
  - `crates/atm/src/util/settings.rs:14`
  - `crates/atm/src/util/state.rs:62`
  - `crates/atm-core/src/retention.rs:210-214`
  - `crates/atm-core/src/io/spool.rs:303-306`
  - `crates/atm-daemon/src/main.rs:60-64`
  - `crates/atm-daemon/src/plugins/ci_monitor/plugin.rs:366-369`
  - `crates/atm-daemon/src/plugins/ci_monitor/loader.rs:130-134`
  - `crates/atm-daemon/src/plugins/issues/plugin.rs:299-302`
  - `crates/atm-daemon/src/plugins/worker_adapter/config.rs:342-346`
  - `crates/atm-daemon/src/plugins/worker_adapter/config.rs:449-452`
  - `crates/atm-daemon/src/plugins/bridge/ssh.rs:148` (currently no ATM_HOME fallback)
- Precedence: ATM_HOME → platform default

Tests:
- 8 unit
- 3 integration (per OS)
- 1 audit script

### Parallel Track A — GitHub CI Monitor Integration

**Sprint 9.3: CI Config & Routing**
- Add client-side branch glob matching (`globset` or `wildmatch`)
- Add `notify_target` config field
- Routing: CI agent -> team lead, optional scrum-master

Tests:
- 10 branch matching
- 5 routing
- 5 config validation
- 2 E2E

**Sprint 9.4: Daemon Operationalization**
- Add daemon status JSON file
- Location: `${ATM_HOME}/daemon/status.json` (via new `get_home_dir()`), not config_dir
- Schema (minimum): `{ timestamp, pid, version, uptime_secs, plugins: [{name, enabled, status, last_error, last_run}], teams: [<team>] }`
- CLI command: `atm daemon status` reads this file and prints JSON/human summary (no IPC)
- Defer service templates (launchd/systemd)

Tests:
- 5 daemon status
- 1 startup hint

### Parallel Track B — Worker Handle Enhancement

**Sprint 9.5: WorkerHandle Backend Payload**
- Keep `WorkerAdapter` trait intact
- Add `backend_id: String` and `payload: Box<dyn Any + Send + Sync>` to `WorkerHandle`
- Provide downcast helper methods (e.g. `payload_ref<T>() -> Option<&T>`) to avoid unsafe casts
- Update registry/adapter to pass payload

Tests:
- 8 trait
- 5 registry
- 35 existing must pass (regression risk)

### Parallel Track C — Daemon Retention

**Sprint 9.6: Daemon Retention Tasks**
- Add periodic inbox trimming in daemon loop
- Include CI monitor report file retention in `report_dir`
- Use `tokio::spawn` to avoid blocking plugins or bridge sync

Tests:
- 10 daemon integration
- 5 concurrency
- 3 cross-platform

---

## Dependency Diagram

```
Sprint 9.0 (Phase 8.6 Verification Gate)
    │
    └── Sprint 9.1 (CI/Tooling Stabilization)
            │
            └── Sprint 9.2 (Home Dir Resolution)
                    │
                    ├── Sprint 9.3 (CI Config & Routing)
                    │       └── Sprint 9.4 (Daemon Operationalization)
                    │
                    ├── Sprint 9.5 (WorkerHandle Backend Payload)
                    │
                    └── Sprint 9.6 (Daemon Retention Tasks)

Note: if needed, Sprint 9.0 + 9.1 can be merged into a single “CI Stabilization” sprint to reduce overhead.
```

---

## Measurable Exit Criteria
- All 667 existing tests must pass
- Phase 9 target test count: 750–780
- New code: minimum 80% coverage (per sprint)
- Zero clippy warnings

## Branching
- Integration branch: `integrate/phase-9` (create from `develop` at phase start)

---

## Appendix: Config Example for This Repo

```toml
[plugins.ci_monitor]
enabled = true
provider = "github"
poll_interval_secs = 60
team = "atm-phase8"
agent = "ci-monitor"
notify_target = "team-lead"        # new field in Phase 9
watched_branches = ["develop", "main", "feature/*"]
notify_on = ["failure", "timed_out", "cancelled"]
report_dir = "temp/atm/ci-monitor"
```
