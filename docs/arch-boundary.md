# ARCH-BOUNDARY-001 Crate Boundary Reference

This document defines the crate-boundary contract behind
`ARCH-BOUNDARY-001` and records the current audited exceptions.

## Boundary Contract

The GitHub stack is split into three layers:

| Layer | Responsibility | GitHub-specific code allowed? |
|---|---|---|
| `atm-core` | core file I/O, config, daemon client payloads, shared schemas | `No` |
| `atm-ci-monitor` | provider-agnostic CI-monitor abstractions: observer, ledger, firewall, budget/freshness interfaces | `No` |
| gh-monitor provider layer | provider implementation, polling, GitHub-specific firewall/execution, API mapping | `Yes` |

Supporting layers:

| Crate / Area | Responsibility | Boundary note |
|---|---|---|
| `atm` | CLI command surface | may call daemon/provider abstractions, must not execute raw `gh` |
| `atm-daemon` issues plugin | non-monitor plugin behavior | may call gh plugin/provider helpers, must not execute raw `gh` |
| `scripts/dev-daemon-smoke.py` | manual smoke harness | must use ATM-owned surfaces only; no direct `gh` shell calls |

## ARCH-BOUNDARY-002 Observability Import Boundary

The observability stack is split into three layers:

| Layer | Responsibility | OTel transport-specific code allowed? |
|---|---|---|
| `sc-observability` | generic event schema, validation, redaction, local logging, neutral `OtelRecord`, exporter traits | `No` |
| `sc-observability-otlp` | collector transport adapter, OTLP protocol/client wiring, auth/TLS, batching/retry | `Yes` |
| entry-point binaries/modules | process-level logger initialization and wiring | `Limited to facade use only` |

### Allowed Dependency Direction

| From | Allowed imports / dependencies | Forbidden imports / dependencies |
|---|---|---|
| `sc-observability` | shared Rust deps, neutral exporter traits, the dedicated `sc-observability-otlp` adapter seam | direct OTLP SDK/client dependencies except through `sc-observability-otlp` |
| `sc-observability-otlp` | `sc-observability`, OTLP/collector SDKs | reverse dependency from `sc-observability` back into entry-point crates |
| entry-point crates/modules | `sc-observability`; logger-init wiring that may call the shared adapter seam | direct imports of `sc-observability-otlp` from non-entry-point modules; ad hoc OTLP exporter construction |
| internal feature modules/helpers/libraries | `sc-observability` facade only | `sc-observability-otlp`, collector SDKs, exporter construction |

### What Counts As A Boundary Violation

Examples of blocking violations:
- direct `sc-observability` imports from modules that should consume a local/shared facade instead of the entry-point wiring path
- direct `sc-observability-otlp` import outside approved entry-point modules
- any non-entry-point import of `opentelemetry*` or `opentelemetry-otlp`
- constructing collector exporters inside CLI/daemon/feature modules instead of the dedicated transport adapter

### AV Cleanup Gate

- AV.0 is the mandatory cleanup sprint for the currently known direct
  `sc-observability` import violations.
- Crate-local facades are allowed only if they are transport-neutral shims
  (for example, storing an injected hook or trait object). A crate-local daemon
  facade must not import `sc-observability` itself; the real exporter wiring
  belongs in the entry-point binary. In other words: daemon-local facade types
  and hook slots are allowed, but the facade implementation must be injected
  from `crates/atm-daemon/src/main.rs` rather than importing `sc_observability`
  inside daemon internals.
- AV.1 must deliver `scripts/ci/observability_boundary_check.sh` plus a CI
  workflow step that runs it before AV.2 begins.
- QA/CI should use this section as the enforcement reference for the
  observability boundary in the same way `ARCH-BOUNDARY-001` governs GitHub
  subprocess ownership.

### Approved Entry-Point Files

The following files are the allowlisted entry-point wiring surfaces for
`ARCH-BOUNDARY-002`:

| File | Rationale |
|---|---|
| `crates/atm/src/main.rs` | CLI process entry point; allowed to initialize generic observability and process-level command lifecycle wiring. |
| `crates/atm-daemon/src/main.rs` | Daemon process entry point; owns process-level observability initialization and injected export-hook wiring. |
| `crates/sc-compose/src/main.rs` | Standalone binary entry point; allowed to initialize generic observability for the process. |
| Integration test files under `crates/*/tests/` | Test-only harness entry points that may validate facade wiring behavior without turning internal library modules into transport owners. |

All other non-entry-point modules must stay on the generic facade side of the
boundary and must not import `sc-observability-otlp` or raw `opentelemetry*`
symbols directly.

## Allowed Dependency Direction

| From | Allowed imports / dependencies | Forbidden imports / dependencies |
|---|---|---|
| `atm-core` | shared Rust deps, core-only modules | plugin crates, provider crates, GitHub-specific execution helpers |
| `atm-ci-monitor` | `atm-core`, provider-agnostic CI-monitor types | plugin crates, direct product-layer GitHub runtime wiring |
| gh-monitor provider layer | `atm-core`, `atm-ci-monitor` | reverse dependency from `atm-core` or `atm-ci-monitor` back into providers |
| CLI / plugin code | `atm-core`, `atm-ci-monitor`, local crate modules | new raw `gh` subprocess launchers outside approved provider layer |

## What Counts As A Boundary Violation

Examples of blocking violations:
- `Command::new("gh")` outside the gh-monitor provider layer
- `agent-team-mail-ci-monitor` in `crates/atm-core/Cargo.toml` non-dev dependencies
- GitHub-specific provider types or API parsing in `atm-core`
- plugin/provider imports flowing back into `atm-core`

Temporary audited exceptions were allowed only during AS/AT transition work.
AT.5 removes the final exception file and returns the repository to a
zero-exception state.

## New Provider Checklist

When adding or refactoring a provider:
1. Keep provider-neutral traits and ledger types in `atm-ci-monitor`.
2. Put provider-specific API execution and translation in the provider layer.
3. Keep `atm-core` free of provider-specific types and subprocess execution.
4. Do not add `Command::new("gh")` outside the provider layer.
5. If transition work temporarily introduces a violation, it must be captured in
   the active phase plan with file-level ownership and removed before the final
   phase audit lands.
6. Run `scripts/ci/gh_boundary_check.sh` before pushing.

## Audit Results

Final AT audit result: AT.5 closed the planned exception list, and AT.6
resolved the two IMPORTANT findings from the Phase AT ending review (#822 and
#823) before the phase could be considered fully clean.
AT.6 follow-up (`feature/pAT-s6-gh-findings-fix`): removed dead
`run_plugin_owned_gh_subprocess` re-export; zero-violation state maintained.

| Violation | Crate / Area | Former File:Line | Issue | Final status |
|---|---|---|---|---|
| Root tracking issue for GitHub boundary elimination plan and enforcement follow-up | planning / governance | `docs/requirements.md` | [#807](https://github.com/randlee/agent-team-mail/issues/807) | root tracking issue |
| Remove `atm-core` non-dev dependency on `agent-team-mail-ci-monitor` and core-owned GH observability path | `atm-core` | `crates/atm-core/Cargo.toml`, `crates/atm-core/src/gh_monitor_observability.rs` | [#808](https://github.com/randlee/agent-team-mail/issues/808) | removed in AT.1 (`feature/pAT-s1-atm-core-isolation`) |
| Raw `gh` subprocess in provider-agnostic crate | `atm-ci-monitor` | `crates/atm-ci-monitor/src/github_provider.rs:173` | [#809](https://github.com/randlee/agent-team-mail/issues/809) | removed in AT.2 (`feature/pAT-s2-ci-monitor-provider-extraction`) |
| Raw `gh --version` and `gh auth status` bootstrap probes plus CLI-owned GitHub command semantics | `atm` CLI | `crates/atm/src/commands/gh.rs:2154`, `crates/atm/src/commands/gh.rs:2167` | [#811](https://github.com/randlee/agent-team-mail/issues/811) | removed in AT.3 (`feature/pAT-s3-gh-command-routing`) |
| Raw `gh` subprocess in issues plugin | `atm-daemon` issues plugin | `crates/atm-daemon/src/plugins/issues/github.rs:31` | [#812](https://github.com/randlee/agent-team-mail/issues/812) | removed in AT.4 (`feature/pAT-s4-daemon-issues-boundary`) |
| Direct `gh` shell calls in smoke harness | manual smoke harness | `scripts/dev-daemon-smoke.py:117`, `scripts/dev-daemon-smoke.py:131`, `scripts/dev-daemon-smoke.py:145` | [#813](https://github.com/randlee/agent-team-mail/issues/813) | removed in AT.4 (`feature/pAT-s4-daemon-issues-boundary`) |
| Issues plugin still used the un-attributed provider helper instead of the request/call-ID-attributed provider entrypoint | `atm-daemon` issues plugin | `crates/atm-daemon/src/plugins/issues/github.rs:25` | [#822](https://github.com/randlee/agent-team-mail/issues/822) | resolved in AT.6 (`feature/pAT-s6-gh-findings-fix`) |
| Boundary grep gate missed the confirmed `tokio::process::Command` variant and only scanned a single smoke script instead of all script targets | CI boundary gate | `scripts/ci/gh_boundary_check.sh:8`, `scripts/dev-daemon-smoke.py` | [#823](https://github.com/randlee/agent-team-mail/issues/823) | resolved in AT.6 (`feature/pAT-s6-gh-findings-fix`) |

Additional notes:
- The final `scripts/ci/gh_boundary_check.sh` acceptance gate now runs without
  any exception file.
- The only remaining `Command::new("gh")` callsites live inside the owning gh
  plugin/provider layer (`github_provider.rs`, `gh_command_routing.rs`), which
  is allowed by `ARCH-BOUNDARY-001`.
- `crates/atm-daemon/src/plugins/ci_monitor/test_support.rs` writes fake `gh`
  scripts for tests but does not launch the real GitHub CLI; it is not counted
  as a boundary violation.

## CI Gate

`scripts/ci/gh_boundary_check.sh` is the enforcement entrypoint.

It must:
- fail on any new raw `gh` subprocess path outside the provider layer
- fail on any new non-dev `atm-core -> atm-ci-monitor` dependency
- print file, line, and `ARCH-BOUNDARY-001` in failure output
- run with zero exception entries in the repository
