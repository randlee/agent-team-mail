# Testing Strategy

This document defines the default testing strategy for the `agent-team-mail`
workspace. The goal is to keep required CI tests deterministic, fast enough to
run on every PR, and explicit about the small set of checks that are allowed to
remain manual smoke coverage.

## Goals

- Keep `cargo test` reliable on Ubuntu, macOS, and Windows.
- Prefer deterministic proofs of correctness over real-process timing tests.
- Reserve `#[ignore]` for tests that are intentionally manual smoke coverage or
  require infrastructure that standard CI does not provide.
- Make test scope obvious: unit, integration, parity/golden, or manual smoke.

## Test Layers

Use the shallowest layer that can prove the behavior.

### 1. Unit Tests

Unit tests are the default.

- Location: inline `#[cfg(test)]` modules in `src`.
- Scope: pure logic, parsers, state transitions, config shaping, rendering,
  routing decisions, and protocol validation.
- Requirement: no wall-clock sleeps and no dependence on external binaries,
  sockets, tmux, or filesystem event delivery.

Examples already in the repo:

- `atm-tui` adapter/renderer parity helpers live near the implementation.
- `atm-core` daemon decision helpers now cover PID mismatch behavior without
  requiring a subprocess race.

### 2. Integration Tests

Integration tests verify crate boundaries, CLI behavior, and filesystem layout.

- Location: crate-level `tests/` directories.
- Preferred harness: `assert_cmd`, `tempfile::TempDir`, helper modules under
  `tests/support`, and per-command environment setup.
- Allowed dependencies: local filesystem, spawned binaries built from the
  workspace, and Tokio tasks when the test can prove readiness explicitly.

Integration tests should still be deterministic. If a test only passes by
sleeping and hoping another process is ready, it should be refactored before it
becomes required CI coverage.

### 3. Parity and Golden Tests

Parity tests validate stable output contracts.

- Store fixtures under `tests/fixtures/...`.
- Load fixtures relative to `env!("CARGO_MANIFEST_DIR")`.
- Normalize line endings before comparison.
- Prefer full-output comparisons with scenario metadata rather than ad hoc
  string fragments.

Current repo patterns worth keeping:

- Scenario directories with `meta.toml`.
- Snapshot-style comparisons for adapter/renderer output.
- Clear mismatch messages that explain which scenario drifted.

### 4. Manual Smoke Tests

Smoke tests are broad end-to-end checks. They are not the primary correctness
proof for the system.

- Use `#[ignore = "..."]` only when the test needs real infrastructure or is
  intentionally retained as manual validation.
- Manual smoke tests may run locally, in QA, or in a dedicated non-blocking CI
  lane.
- A smoke test must explain why it is ignored and what deterministic coverage
  already exists.

Examples:

- Real tmux end-to-end worker tests.
- Live Codex/MCP compatibility checks.
- Real daemon restart/autostart scenarios that depend on subprocess timing.

## Layout Conventions

- Unit tests live in `src` next to the code they exercise.
- Integration suites live in `tests/`.
- Shared integration helpers belong in `tests/support` or local helper
  functions, not duplicated across files.
- Scenario fixtures belong in `tests/fixtures`.

## Writing Deterministic Tests

### Prefer extraction over orchestration

When a test becomes timing-sensitive, split the code into:

- a pure decision function or state reducer
- a thin side-effect layer that performs I/O

Test the pure function exhaustively. Keep any real-process coverage as smoke
only.

This is the preferred fix for:

- daemon bootstrap decisions
- PID/backend reconciliation
- routing/concurrency decisions
- command shaping and protocol formatting

### Use dependency injection for external systems

If behavior currently depends on tmux, a child process, or a live MCP binary,
prefer a fake backend over a real one in required CI.

Examples:

- use `MockTmuxBackend` for worker routing and launch behavior
- use `MockTransport` for MCP transport and JSON-RPC plumbing
- use test doubles for metadata readers, pid probes, or spawners

### Use readiness polling, not fixed sleeps

When a real subprocess is unavoidable, poll for a concrete readiness condition:

- socket file exists and accepts connections
- marker file or status file contains the expected value
- child emits an explicit ready line

Do not use fixed `sleep(...)` as the proof that another component is ready.

### Keep process-global environment changes rare

Prefer per-command environment injection such as `cmd.env(...)`.

If a code path must read environment variables in-process:

- isolate the test
- serialize it with `serial_test` when necessary
- use an RAII guard to restore the previous environment explicitly

Process-global `std::env::set_var` / `remove_var` calls are prohibited in tests
unless they are wrapped by a restore guard and the test is serialized.

## Cross-Platform Requirements

Follow [docs/cross-platform-guidelines.md](cross-platform-guidelines.md).

Non-negotiable rules:

- use `ATM_HOME`, not `HOME` or `USERPROFILE`, for test isolation
- use `TempDir`, not `/tmp`
- use `PathBuf` and `join()`
- normalize CRLF when asserting snapshot-like output
- gate Unix-only runtime behavior with `#[cfg(unix)]`

Windows compile-only coverage is acceptable when equivalent runtime behavior is
not available on Windows yet, but that exception should be documented in the
test itself.

## When `#[ignore]` Is Acceptable

`#[ignore]` is acceptable only when one of these is true:

1. The test requires infrastructure that standard CI does not provide.
2. The test is intentionally smoke coverage for a real external integration.
3. The test is temporary and has an attached follow-up to make it deterministic.

`#[ignore]` is not acceptable when:

- a mock backend already exists
- the behavior can be covered by a pure helper
- the test is ignored only because it is flaky
- the test is obsolete or duplicates stronger deterministic coverage

Every ignored test should have:

- a specific reason string
- a clear manual invocation hint when relevant
- a note in `docs/test-audit.md`

## CI Expectations

Required PR validation should center on:

- `cargo test`
- `cargo test -- --test-threads=8` for env-sensitive or parallel-stability
  coverage when a change affects global state assumptions
- targeted parity/golden tests
- `cargo clippy -- -D warnings`
- `cargo fmt --check`

Manual or gated validation may include:

- tmux smoke tests
- live MCP/Codex compatibility tests
- real daemon restart/autostart smoke tests

If a test is important but noisy, move the logic proof into deterministic CI and
keep a thin smoke test for local or QA validation.

## Review Checklist

Before adding a new integration test, answer these questions:

1. Can the behavior be proven with a pure helper instead?
2. Can an existing mock backend or fake transport cover the same behavior?
3. If a subprocess is required, what explicit readiness signal will the test
   wait for?
4. Does the test isolate `ATM_HOME`, filesystem paths, and environment?
5. If the test is ignored, is the reason truly infrastructure-bound and
   documented in `docs/test-audit.md`?
