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

Final AT.5 audit result: zero remaining GitHub boundary violations.

| Violation | Crate / Area | Former File:Line | Issue | Final status |
|---|---|---|---|---|
| Remove `atm-core` non-dev dependency on `agent-team-mail-ci-monitor` and core-owned GH observability path | `atm-core` | `crates/atm-core/Cargo.toml`, `crates/atm-core/src/gh_monitor_observability.rs` | [#808](https://github.com/randlee/agent-team-mail/issues/808) | removed in AT.1 |
| Raw `gh` subprocess in provider-agnostic crate | `atm-ci-monitor` | `crates/atm-ci-monitor/src/github_provider.rs:173` | [#809](https://github.com/randlee/agent-team-mail/issues/809) | removed in AT.2 |
| Raw `gh --version` and `gh auth status` bootstrap probes plus CLI-owned GitHub command semantics | `atm` CLI | `crates/atm/src/commands/gh.rs:2154`, `crates/atm/src/commands/gh.rs:2167` | [#811](https://github.com/randlee/agent-team-mail/issues/811) | removed in AT.3 |
| Raw `gh` subprocess in issues plugin | `atm-daemon` issues plugin | `crates/atm-daemon/src/plugins/issues/github.rs:31` | [#812](https://github.com/randlee/agent-team-mail/issues/812) | removed in AT.4 |
| Direct `gh` shell calls in smoke harness | manual smoke harness | `scripts/dev-daemon-smoke.py:117`, `scripts/dev-daemon-smoke.py:131`, `scripts/dev-daemon-smoke.py:145` | [#813](https://github.com/randlee/agent-team-mail/issues/813) | removed in AT.4 |

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
