---
name: sprint-report
description: Generate a sprint status report for the current phase. Use --detailed for a full per-sprint breakdown or --table for a condensed summary table.
---

# Sprint Report Skill

Generate a formatted status report for all sprints in the current phase.

## Usage

```
/sprint-report [--detailed | --table]
```

- `--detailed` — Full per-sprint breakdown with CI, QA, DEV, and agent detail
- `--table` — Condensed single-table view for quick status scan
- Default (no flag): `--table`

Render using the Jinja2 template at `.claude/skills/sprint-report/report.md.j2`.

---

## Data Model

Populate a context dict before rendering:

```python
{
  "phase": "AF",
  "sprints": [
    {
      "id": "AF.1",
      "description": "Session + PID liveness correctness",
      "pr": 524,
      "dev": None,                        # None or {"status": "...", "agent": "arch-ctm"}
      "qa": {
        "status": "PASS",                 # PASS | FAIL | IN_PROGRESS | PENDING
        "iteration": None,                # int or None
        "deferred_issues": []             # list of GH issue numbers
      },
      "ci": {
        "status": "MERGED",               # MERGED | READY | GREEN | RUNNING | FAILING | FLAKE_RERUN
        "target": "integrate/phase-AF",   # merge target (when MERGED)
        "pending": 0,
        "failing_test": None,             # test name (when FAILING)
        "failing_platforms": [],          # e.g. ["macos", "windows"]
        "fix_agent": None,                # agent name (when FAILING)
        "fix_acked": False
      }
    }
  ],
  "integration": {
    "branch": "integrate/phase-AF",
    "pr": 530,
    "ci_status": "RUNNING",              # RUNNING | GREEN | FAILING | READY | MERGED
    "pending": 3,
    "notes": "Pending AF.3 + AF.5 sprint merges"
  }
}
```

---

## --detailed Field Rules

**DEV field** (show only when arch-ctm has an active dev assignment)
- `Doc fixes in progress [arch-ctm]`
- `CI fix in progress [arch-ctm] (acked)`
- Omit the DEV line entirely if no active dev work

**QA field**
- `PASS ✓` — QA complete, no findings
- `PASS ✓  (deferred: #123)` — QA complete, non-blocking findings in GH issue
- `In progress (iteration {N})` — QA agents running
- `FAIL — {summary}` — blocking findings open
- `Pending` — QA not yet started

**CI field**
- `Merged to {target} ✓` — sprint PR merged (no Notes line needed)
- `Ready to Merge` — all CI green + QA PASS, awaiting user approval
- `Green ✓` — all checks passing, not yet merged
- `Running ({N} pending)` — CI in progress, no failures
- `Failing — {test} [{platforms}] — Fix assigned: {agent} (acked)` — CI failure
- `Flake rerun in progress` — infrastructure flake, rerun triggered

**Notes field** — omit when CI line already captures the key info (e.g. when Merged)

---

## --detailed Example

```
Sprint: AF.1  Session + PID liveness correctness
PR: #524
QA: PASS ✓
CI: Merged to integrate/phase-AF ✓
────────────────────────────────────────
Sprint: AF.2  Spawn authorization + preview UX
PR: #526
QA: PASS ✓
CI: Merged to integrate/phase-AF ✓
────────────────────────────────────────
Sprint: AF.3  Transient non-member registration blocking
PR: #527
QA: PASS ✓  (iteration 4)
CI: Failing — test_gh_status_preflight [ubuntu, macos] — Fix assigned: arch-ctm (acked)
────────────────────────────────────────
Sprint: AF.4  Cleanup sentinel + external agent staleness TTL
PR: #528
QA: PASS ✓
CI: Merged to integrate/phase-AF ✓
────────────────────────────────────────
Sprint: AF.5  Reliability closeout + socket test hardening
PR: #529
DEV: Doc fixes in progress [arch-ctm]
QA: In progress (iteration 2)
CI: Running (1 pending)
────────────────────────────────────────
Integration: integrate/phase-AF → develop
PR: #530
CI: Running — pending AF.3 + AF.5 merges
```

---

## --table Example

```
| Sprint | Description                        | PR   | DEV              | QA              | CI                          |
|--------|------------------------------------|------|------------------|-----------------|-----------------------------|
| AF.1   | Session + PID liveness             | #524 |                  | PASS ✓          | Merged to integrate/phase-AF ✓ |
| AF.2   | Spawn auth + preview UX            | #526 |                  | PASS ✓          | Merged to integrate/phase-AF ✓ |
| AF.3   | Transient non-member blocking      | #527 |                  | PASS ✓ (iter 4) | Failing (arch-ctm, acked)   |
| AF.4   | Cleanup sentinel + TTL             | #528 |                  | PASS ✓          | Merged to integrate/phase-AF ✓ |
| AF.5   | Reliability + socket hardening     | #529 | Doc fixes [arch] | In progress (2) | Running (1 pending)         |
| **integrate** | phase-AF → develop          | #530 |                  | —               | Running                     |
```
