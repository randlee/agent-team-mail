# Issue #539 Fix Plan: Stale Daemon Shutdown and Recovery

Date: 2026-03-11  
Status: revised after partial delivery  
Related Issues: #539, #669

## 1. Problem Statement

Daemon restart reliability work under issue `#539` was originally described as a
single Layer 2 shutdown-hardening bucket. QA correctly called out that the
implementation delivered in `#669` only covers part of that scope.

We now split Layer 2 into delivered vs deferred items so the ADR matches the
actual code in `develop`.

## 2. Layer 2 Scope Split

### 2.1 Delivered in `#669`

Delivered behavior:
- daemon shutdown waits for internal background tasks with a bounded timeout
- timed-out tasks are explicitly aborted after the cooperative wait expires
- plugin `run()` tasks now use the same bounded wait-and-abort behavior during
  daemon shutdown

Delivered rationale:
- SIGTERM shutdown was previously allowed to log timeouts and continue without
  aborting stuck tasks
- that left hung tasks alive during teardown and contributed to restart flake

Delivered verification:
- `event_loop::tests::test_wait_for_shutdown_task_aborts_timed_out_task`
- `event_loop::tests::test_wait_for_shutdown_task_allows_completed_task`

### 2.2 Deferred from original Layer 2

Deferred items:
- watchdog thread/process that escalates when the main shutdown path wedges
- explicit double-signal exit flow for repeated termination requests

Deferred status:
- not implemented by `#669`
- remain tracked as follow-up work under issue `#539`

Reason for deferral:
- the immediate blocker was unbounded task lifetime after SIGTERM
- abort-on-timeout is the smallest change that restores bounded stop-path
  behavior without mixing in a second supervisor design

## 3. Updated Layer 2 Data Flow

```
SIGTERM received
  -> cancel shared CancellationToken
  -> wait for daemon background tasks (bounded)
     -> abort timed-out daemon tasks
  -> graceful_shutdown(plugins, timeout per plugin)
  -> wait for plugin run() tasks (bounded)
     -> abort timed-out plugin run() tasks
  -> final status/log flush best effort
  -> process exit
```

The `plugin run()` task wait occurs after `graceful_shutdown(...)` because the
plugin first gets a cooperative shutdown callback and then its long-running
task is given a bounded window to observe cancellation and exit.

## 4. Open Items

Still open after `#669`:
- design watchdog ownership and escalation rules
- define repeated-signal behavior (`SIGTERM`/second signal/forced exit)
- decide whether plugin shutdown should become parallel instead of sequential

## 5. Consequences

- The ADR now matches the code that actually shipped.
- Reviewers can distinguish the delivered bounded-abort work from the deferred
  watchdog/escalation work.
- Future shutdown-hardening PRs can target the remaining Layer 2 items without
  reopening already-delivered task-abort behavior.
