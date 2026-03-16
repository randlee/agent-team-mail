# Phase AN/AO/AP/AQ Dev-Daemon Smoke Protocol

**Author**: arch-ctm
**Purpose**: Manual smoke test for dev-installed daemon before publishing
**Estimated time**: 15–20 minutes

---

## Pre-Flight Checklist

1. Confirm you are on the intended branch/build and the dev install was refreshed from that build.
2. Confirm no rogue dev/test daemons are running before the smoke begins: `pgrep -fl atm-daemon` should show no stray `~/.local/atm-dev/bin/atm-daemon` or worktree `target/*/atm-daemon` processes.
3. If a production daemon is running, record it and do not reuse its `ATM_HOME` for the smoke.
4. Use the shared dev home for shared-runtime checks: `ATM_HOME=$HOME/.local/share/atm-dev/home`.
5. Use a fresh temp `ATM_HOME` for isolated-runtime checks.
6. Confirm the repo under test has a valid `.atm.toml` / team config for `atm-dev`.
7. Confirm GitHub auth is available: `gh auth status`.
8. Capture starting GH core quota:
   ```bash
   gh api rate_limit | python3 -c "import json,sys; d=json.load(sys.stdin); c=d['resources']['core']; print(c['remaining'], c['limit'])"
   ```
9. **PASS precondition**: no rogue dev/test daemons, GH auth working, clean runtime homes.
   **FAIL precondition**: rogue dev/test daemons, stale shared-runtime state that cannot be cleared, or GH already exhausted.

> Important: for daemon-lifecycle smoke cases that intentionally use an `ATM_HOME`
> without team config, do **not** use `atm status --team ...` as the primary
> success probe. That command requires a team config directory under the target
> `ATM_HOME` and can return exit `1` even after daemon autostart succeeded.
> Use `atm gh --team atm-dev status --json` to trigger autostart and
> `atm daemon status --json` / runtime files to verify daemon state.

---

## Phase AN — CI Monitor

### AN.1 — GH Monitor Start / Status Round Trip
**Purpose**: verify `atm gh monitor` starts cleanly and reports structured state.
**Preconditions**: clean `ATM_HOME`, repo with GH monitor config.
**Steps**:
1. `atm gh --team atm-dev status --json`
2. If lifecycle state is `stopped` from prior cleanup, run `atm gh --team atm-dev monitor restart --json` first.
3. `atm gh --team atm-dev monitor pr <pr-number> --json`
4. `atm gh --team atm-dev status --json` again

**Expected**: initial status is idle/untracked, monitor command succeeds, final status shows active monitor state with repo/team metadata.
**Pass**: monitor starts and status becomes actionable JSON without fallback errors.
**Fail**: daemon unreachable, repo scope missing, or status schema inconsistent.

### AN.2 — GH Monitor Lifecycle Control
**Purpose**: verify monitor stop / restart lifecycle commands.
**Preconditions**: AN.1 monitor active.
**Steps**:
1. `atm gh --team atm-dev monitor stop --json`
2. `atm gh --team atm-dev status --json`
3. `atm gh --team atm-dev monitor restart --json`
4. `atm gh --team atm-dev status --json`

**Expected**: stop returns stopped lifecycle state, restart returns running state, status tracks each change.
**Pass**: lifecycle transitions are explicit and status tracks them.
**Fail**: commands hang, silently no-op, or leave inconsistent lifecycle state.

### AN.3 — Multi-Repo Repo-Scope Resolution
**Purpose**: verify repo detection / override works correctly.
**Preconditions**: repo with valid Git remote; optional second repo path for override.
**Steps**:
1. In repo A: `atm gh --team atm-dev monitor run 42 --json`
2. `atm gh --team atm-dev --repo owner/other-repo monitor run 42 --json`
3. `atm gh pr list --team atm-dev --json`

**Expected**: default resolves repo A from git remote; `--repo` overrides cleanly.
**Pass**: repo scope is correct in both cases.
**Fail**: repo scope missing, stale, or cross-wired.

---

## Phase AO — Runtime Lifecycle

### AO.1 — Shared Runtime Admission
**Purpose**: verify only one shared dev daemon is admitted.
**Preconditions**: no running dev daemon.
**Steps**:
1. Start the shared dev daemon via `atm gh --team atm-dev status --json`
2. In a second shell with the same shared `ATM_HOME`, run the same command again
3. Inspect `pgrep -fl atm-daemon` and `atm daemon status --json`

**Expected**: first shared runtime owns the daemon; second attempt reuses or is rejected; no second dev daemon appears.
**Pass**: only one shared dev daemon exists.
**Fail**: multiple shared daemons appear or ownership metadata becomes ambiguous.

### AO.2 — Isolated Runtime TTL / Non-Shared State
**Purpose**: verify isolated runtime state stays separate from shared dev runtime.
**Preconditions**: shared dev daemon stopped; separate temporary `ATM_HOME` prepared.
**Steps**:
1. Run `ATM_HOME=<tempdir> atm gh --team atm-dev status --json`
2. Confirm `.atm/daemon/` state is created only under the isolated home
3. Optionally run `ATM_HOME=<tempdir> atm daemon status --json` and confirm the runtime is isolated / disabled for live GH polling
4. Stop or kill the isolated daemon and confirm the temp runtime can be cleaned up independently

**Expected**: isolated runtime does not reuse shared dev state and does not leave long-lived shared ownership behind.
**Pass**: isolated probe returns success, isolated runtime files are present under the temp home, and no shared-runtime files are touched.
**Fail**: touches shared runtime state, requires shared team config under the isolated home, or leaves shared daemon metadata behind.

### AO.3 — Repo-State Budget Observability
**Purpose**: verify GH monitor state/budget visibility is present.
**Preconditions**: daemon running with GH monitor enabled.
**Steps**:
1. `atm gh --team atm-dev status --json`
2. Confirm JSON includes freshness/updated timestamps and repo-state details
3. Optionally inspect repo-state cache files under `.atm/daemon/`

**Expected**: output shows updated/freshness info and repo-state tracking.
**Pass**: timestamps/state are visible and coherent.
**Fail**: daemon uses GH without observable freshness/budget state.

### AO.4 — Operator Shutdown / Restart
**Purpose**: verify operator lifecycle commands behave predictably without orphaned state.
**Preconditions**: monitor active.
**Steps**:
1. Run approved stop path for monitor / daemon
2. Confirm daemon status after stop
3. Restart through approved path and confirm health

**Expected**: stop drains cleanly, restart restores healthy state, ownership metadata stays coherent.
**Pass**: state transitions are explicit and no stale lock remains.
**Fail**: restart requires manual cleanup or produces split-brain ownership.

---

## Phase AP — Daemon Spawn Lifecycle

### AP.1 — Clean Start / No Rogue Daemons
**Purpose**: verify daemon lifecycle starts clean and leaves no rogues.
**Preconditions**: no dev/test daemons running.
**Steps**:
1. `pgrep -fl atm-daemon`
2. `atm gh --team atm-dev status --json`
3. `pgrep -fl atm-daemon`

**Expected**: exactly one expected dev daemon appears.
**Pass**: process count 0 → 1 expected dev daemon.
**Fail**: multiple dev/test/install daemons appear.

### AP.2 — Stale Lock / PID Recovery
**Purpose**: verify daemon recovers from stale runtime metadata automatically.
**Preconditions**: stopped daemon; stale PID/lock/status files preserved or simulated.
**Steps**:
1. Create or preserve stale daemon lock/PID metadata under the shared dev `ATM_HOME`
2. Run `atm gh --team atm-dev status --json`
3. Inspect daemon status and process table; confirm stale state is replaced by a live daemon

**Expected**: stale metadata is detected and recovered from automatically.
**Pass**: the probe returns success, a new live daemon replaces stale metadata, and only one dev daemon remains.
**Fail**: startup hangs, loops, requires manual file deletion, or the probe only fails because the shared `ATM_HOME` lacks team config.

### AP.3 — PID Tracking / Teardown Discipline
**Purpose**: verify daemon PID tracking remains coherent through stop/start.
**Preconditions**: daemon running.
**Steps**:
1. Record daemon PID from status file or process table
2. Stop the daemon
3. Confirm PID is gone and no extra daemon remains
4. Start again via `atm gh --team atm-dev status --json` and confirm a new single PID

**Expected**: old PID fully exits before new one takes over.
**Pass**: one PID exits cleanly, one replacement PID appears.
**Fail**: zombies, duplicates, or stale ownership remain.

---

## Phase AQ — Cleanup Verification

### AQ.1 — Daemon Autostart Reliability
**Purpose**: verify autostart works after AQ cleanup and uses the correct binary.
**Preconditions**: no daemon running; dev install on PATH only where intended.
**Steps**:
1. `atm gh --team atm-dev status --json`
2. Confirm daemon starts automatically
3. Confirm spawned binary path matches expected dev install (not a stray installed binary)

**Expected**: autostart starts the correct dev daemon and returns a valid response.
**Pass**: right daemon binary starts once.
**Fail**: autostart fails, chooses wrong binary, or spawns duplicates.

### AQ.2 — Const-Driven Timeout Behavior
**Purpose**: verify AQ timeout/const cleanup did not change observable behavior.
**Preconditions**: daemon running.
**Steps**:
1. `atm gh --team atm-dev monitor run <pr-number> --json`
2. Observe command completion latency
3. One stop/restart cycle; confirm no unexpectedly long wait

**Expected**: commands complete within expected interactive range; no regression toward hang-like behavior.
**Pass**: command returns promptly and consistently.
**Fail**: timeout behavior materially worse than pre-AQ.

### AQ.3 — Rogue Daemon Spawn Elimination
**Purpose**: verify AQ.5 removed the daemon-spawn regression.
**Preconditions**: no dev/test daemon running; clean `ATM_HOME`.
**Steps**:
1. Run one normal daemon-backed workflow via `atm gh --team atm-dev status --json`
2. `pgrep -fl atm-daemon`
3. Stop daemon and recheck

**Expected**: one daemon during active use, zero extra daemons after stop.
**Pass**: no rogue daemons before or after.
**Fail**: extra `atm-daemon` processes remain alive or accumulate.

---

## CI Token Verification — **Critical**

**Purpose**: verify the pre-AQ runaway GH token-consumption regression is eliminated.
**Preconditions**: repo/PR that can be monitored; GH auth available; not already rate-limited.

**Steps**:
1. Record initial quota:
   ```bash
   gh api rate_limit | python3 -c "import json,sys; d=json.load(sys.stdin); c=d['resources']['core']; print('before:', c['remaining'], '/', c['limit'])"
   ```
2. Start one monitor: `atm gh --team atm-dev monitor pr <pr-number> --json`
3. Let it poll for 2–3 cycles (~2–3 minutes at active cadence)
4. Record quota again with the same command
5. Stop the monitor and record a short-window quota sample (~5 seconds later)
6. Confirm no extra dev/test daemons remain

**Expected**: quota decreases by a small bounded amount consistent with one shared poller.

**Pass threshold**: for a 2–3 cycle manual smoke, active-window consumption stays bounded (roughly one shared poller plus startup probes, and no more than about 10 core requests total), and the short post-stop delta stays near zero (0–2 requests).
**Fail**: quota drops materially faster than one shared poller can explain, or continues falling in the short window after the monitor is stopped.

---

## Overall Smoke PASS / FAIL Gate

### PASS requires all of:
- All preflight checks clean
- Every phase area has at least one representative command path that succeeds end-to-end
- No rogue dev/test daemons remain after stop/cleanup
- GH token consumption stays bounded and explainable

### FAIL if any of:
- Any duplicate/rogue daemon appears
- Shared runtime ownership becomes ambiguous
- Monitor lifecycle/status is inconsistent
- Autostart chooses wrong binary or leaves stale lock state
- GH core quota drops at a runaway rate or continues dropping after monitors stop

> If any FAIL condition triggers: do not publish. Capture the failing command, the process list, and the before/after GH quota numbers.
