# ADR: Issue #669 SIGTERM Stop-Path Fix

- Status: Accepted
- Date: 2026-03-11
- Sprint: `fix/issue-669-sigterm-stop-path`
- Related Issues: #669, #539

## Context

`atm daemon restart` could stop the old daemon with SIGTERM, but shutdown still
allowed internal daemon tasks and plugin `run()` tasks to outlive the bounded
wait window. The event loop logged a timeout and then continued, leaving stuck
tasks alive and making restart behavior flaky.

## Root Cause

The daemon shutdown path had a cooperative timeout but no enforced termination
for timed-out tasks:

- internal daemon background tasks were waited with `tokio::time::timeout(...)`
  and then only logged on timeout
- plugin `run()` tasks were joined after `graceful_shutdown(...)`, but a stuck
  task could still extend or wedge total shutdown progress

That meant SIGTERM did not guarantee bounded teardown once a task stopped
observing cancellation.

## Decision

Introduce a shared shutdown helper that:
- waits for a task to finish within a fixed timeout
- aborts the task if the timeout expires
- awaits the post-abort join result for logging/cleanup

Use that helper for:
- spool task
- watcher task
- dispatch task
- retention task
- status writer task
- reconcile task
- plugin `run()` tasks after `graceful_shutdown(...)`

## Timeout Rationale

The chosen timeout is `5s`.

Why `5s`:
- long enough for cooperative cancellation and final flush/cleanup on normal
  shutdown
- short enough to keep restart and operator recovery bounded when a task wedges
- already consistent with the stop-path behavior expected by current dogfood
  restart checks

This is a bounded-shutdown contract, not a promise that every task gets an
unlimited cleanup window.

## Relationship to Issue #539

`#669` delivers only the task-abort portion of the broader stale-daemon/shutdown
hardening work described under `#539`.

Delivered here:
- bounded wait for daemon background tasks
- bounded wait for plugin `run()` tasks
- abort-after-timeout when cooperative cancellation is ignored

Still open under `#539`:
- watchdog thread/process
- double-signal exit/escalation behavior
- possible parallel plugin shutdown

## What Remains Open

- sequential plugin `shutdown()` still has worst-case `N * timeout` latency
- plugin degraded-state reporting still happens in plugin lifecycle/status code,
  not in the generic shutdown helper
- final end-to-end restart verification must still be exercised from a normal
  shell/session because the Codex harness is noisy for detached-child lifetime

## Verification

- `cargo test -p agent-team-mail-daemon event_loop::tests::test_wait_for_shutdown_task_aborts_timed_out_task`
- `cargo test -p agent-team-mail-daemon event_loop::tests::test_wait_for_shutdown_task_allows_completed_task`
