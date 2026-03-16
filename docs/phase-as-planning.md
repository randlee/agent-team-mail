# Phase AS — GitHub API Access Limiting

**Integration branch**: `integrate/phase-AS` off `develop`
**Prerequisites**: Phase AQ merged to `develop`; Phase AR merged to `develop`

## Overview

Phase AS is a focused GH-access control phase. The pre-AQ runaway is eliminated,
but the current system still lacks a hard `gh` execution firewall, complete
token-correlated logging, and lifecycle suppression strong enough to guarantee
bounded GitHub usage during multi-monitor smoke or QA runs.

This phase does not add new monitor features. It tightens the existing
`gh_monitor` control plane so GitHub usage is explainable, enforceable, and
reviewable.

## Goals

1. Enforce a hard firewall between requested GitHub work and the actual `gh`
   subprocess call.
2. Emit a complete execution ledger for every real `gh` call so token movement
   can be correlated locally.
3. Emit a separate info/freshness ledger so operators can tell whether useful
   answers came from cache, live refresh, or degraded policy fallback.
4. Bound shared-poller behavior with explicit per-monitor pacing and global
   token headroom policy.
5. Ensure lifecycle `stop` / `draining` truly suppresses new shared-poller GH
   calls.
6. Recalibrate smoke expectations for single-monitor vs multi-monitor runs.

---

## Sprint Plan

### AS.1 — Hard `gh` Firewall

**Scope**: collapse all permitted `gh_monitor`/`atm gh` execution onto one
mandatory adapter and make bypasses impossible by policy.

**Deliverables**:
- one canonical adapter for real `gh` subprocess execution
  The adapter is the existing `GhCliObserver::before_gh_call` gate paired with
  `GitHubActionsProvider::run_gh_with_metadata` as the only permitted `gh`
  spawn path for `gh_monitor` behavior.
- all `gh_monitor` and `atm gh` execution paths routed through that adapter
- direct `Command::new("gh")` bypasses removed or converted to explicit
  non-monitor exceptions with rationale
- QA rule that any new bypass is a blocking fail

**Acceptance Criteria**:
- no in-scope GitHub monitor/status path can launch `gh` outside the firewall
  (`GH-CI-FR-46`)
- blocked calls fail with a structured reason (`GH-CI-FR-46`)
- review/QA can identify the single canonical execution surface
  (`GH-CI-FR-45`, `GH-CI-FR-46`)

### AS.2 — Two-Layer GitHub Observability

**Scope**: add two structured ledgers:
- execution/token ledger
- info/freshness ledger

**Deliverables**:
- `gh_call_blocked`, `gh_call_started`, `gh_call_finished` event family
- `gh_info_requested`, `gh_info_served_from_cache`, `gh_info_live_refresh`,
  `gh_info_degraded`, `gh_info_denied` event family
- correlation via UUID v4 `call_id` / `request_id`
- stable JSONL/event fields for QA and operator inspection
- ownership model:
  - owning crate: `crates/atm-ci-monitor`
  - storage: append-only on-disk JSONL ledgers under the runtime directory
  - sync model: all producers send records through a channel to one writer task;
    the writer task is the only component that mutates the ledger files
- a single unified record schema with a discriminating kind field is acceptable
  if it preserves the execution-layer vs freshness-layer separation in queries

**Acceptance Criteria**:
- every real `gh` subprocess call is represented in the execution ledger
  (`GH-CI-FR-47`, `GH-CI-FR-51`)
- every GH-backed status/info request is represented in the freshness ledger
  (`GH-CI-FR-48`, `GH-CI-FR-52`)
- the two ledgers can be correlated for one request path end-to-end
  (`GH-CI-FR-47`, `GH-CI-FR-48`)

### AS.3 — Budgeting, Headroom, and Lifecycle Suppression

**Scope**: make the shared poller respect lifecycle and bounded budget policy.

**Deliverables**:
- per-active-monitor call cadence/budget cap
- global headroom floor that pauses or degrades polling before token exhaustion
- lifecycle `stopped` / `draining` suppression of new poll cycles
- active monitor record handling that cannot leave a stopped lifecycle polling
  indefinitely

**Acceptance Criteria**:
- stop/restart no longer leave a shared poller continuing to spend tokens
  (`GH-CI-FR-49`)
- active monitor count and `in_flight` reflect only pollable work
  (`GH-CI-FR-26`, `GH-CI-FR-49`)
- budget enforcement is visible in logs and status surfaces
  (`GH-CI-FR-22`, `GH-CI-FR-50`)

### AS.4 — Smoke Thresholds and Verification

**Scope**: update smoke expectations and verification around bounded GitHub use.

**Deliverables**:
- update `docs/smoke-test-an-ao-ap-aq.md` with calibrated single-monitor and
  multi-monitor thresholds
- explicit single-monitor threshold guidance
- explicit multi-monitor/shared-poller threshold guidance
- smoke/test plan for verifying both the firewall and the budget model
- QA guidance for reading the two observability layers

**Acceptance Criteria**:
- smoke protocol distinguishes single fresh monitor from multi-monitor runs
  (`GH-CI-FR-50`)
- the expected budget envelope is documented and reviewable
  (`GH-CI-FR-50`)
- verification plan proves both “no bypass” and “useful degraded answers”
  (`GH-CI-FR-46`, `GH-CI-FR-48`, `GH-CI-FR-50`)

---

## Proposed Constants / Config Surface

Initial default constants/config fields, owned by `crates/atm-ci-monitor`:
- `GH_MONITOR_PER_ACTIVE_MONITOR_MAX_CALLS = 6`
- `GH_MONITOR_HEADROOM_FLOOR = 200`
- `GH_MONITOR_HEADROOM_RECOVERY_FLOOR = 300`
- `GH_MONITOR_SINGLE_SMOKE_MAX_CALLS = 10`
- `GH_MONITOR_MULTI_SMOKE_MAX_CALLS = 30`
- `GH_MONITOR_POST_STOP_MAX_DELTA = 5`

The daemon plugin consumes these through provider/observer wiring; policy
ownership stays in the CI-monitor domain rather than daemon plugin constants.

---

## Test Plan

1. Unit tests proving blocked requests do not spawn `gh`.
2. Integration tests proving all allowed monitor/status paths emit execution
   ledger entries.
3. Integration tests proving cache/freshness responses emit info-layer events
   even when no live `gh` call occurs.
4. Lifecycle tests proving `stop`/`draining` prevent new shared-poller calls.
5. Smoke verification:
   - single-monitor run stays inside the single-monitor threshold
   - multi-monitor run stays inside the calibrated multi-monitor threshold
   - post-stop short-window delta remains bounded

---

## Exit Criteria

Phase AS is complete when:
- the hard firewall is the only in-scope execution path
- execution and freshness ledgers are both present and useful
- shared-poller lifecycle suppression is enforceable
- smoke thresholds are recalibrated and documented
- QA can explain token movement using local logs without guessing
