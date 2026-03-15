# ADR: Phase U U.6 Verification Evidence (CLI Publishability + `atm monitor`)

## Status
Accepted

## Date
2026-03-02

## Context
Phase U sprint U.6 verifies two previously implemented Phase T deliverables against the v0.27.0 release baseline:
- Issue #284: CLI crate publishability
- Issue #286: `atm monitor` operational health monitor behavior

This ADR records command-level evidence that:
1. The `agent-team-mail` CLI crate packages and dry-run publishes cleanly.
2. `atm monitor` is present in the CLI and operational behavior is covered by tests.

## Verification Steps and Results

### 1) CLI package verification (`#284`)

Command:
```bash
cargo package -p agent-team-mail --locked
```

Result:
- Passed.
- Package created and verified successfully for `agent-team-mail v0.27.0`.
- No `include_str!` path failures or path-outside-crate packaging failures observed.

### 2) CLI publish dry-run (`#284`)

Command:
```bash
cargo publish -p agent-team-mail --dry-run --locked
```

Result:
- Passed.
- Packaging and verification succeeded.
- Upload correctly aborted due to dry-run mode.
- crates.io warning indicates `agent-team-mail@0.27.0` already exists, consistent with release baseline.

### 3) CLI monitor subcommand presence (`#286`)

Command:
```bash
cargo run -p agent-team-mail -- monitor --help
```

Result:
- Passed.
- `monitor` subcommand is present and exposes expected options:
  - `--team`
  - `--interval-secs`
  - `--cooldown-secs`
  - `--notify`
  - `--once`

### 4) Monitor behavior + dedupe verification (`#286`)

Command:
```bash
cargo test -p agent-team-mail monitor -- --nocapture
```

Result:
- Passed.
- Unit tests validate extraction/dedupe behavior.
- Integration tests validate:
  - critical finding alerts
  - dedupe suppression within cooldown
  - reintroduced-fault re-alerting
  - polling loop operation
  - daemon-unavailable resilience
  - fault alert emission within two polling intervals

### 5) Published version visibility check (`#284`)

Command:
```bash
cargo search agent-team-mail --limit 5
```

Result:
- Passed.
- crates.io index reports `agent-team-mail = "0.27.0"` (and related crates at `0.27.0`).

## Decision
U.6 verification is complete for the requested checks and supports closing verification scope for #284 and #286 under Phase U.

## Notes
Warnings about integration tests not being included in published package are expected and do not indicate publishability failure.
