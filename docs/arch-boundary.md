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
| `atm-daemon` issues plugin | non-monitor plugin behavior | currently audited exception; must migrate off raw `gh` |
| `scripts/dev-daemon-smoke.py` | manual smoke harness | currently audited exception; must migrate off direct `gh` shell calls |

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

Examples of temporary audited exceptions:
- legacy bootstrap probes in `atm gh init`
- legacy direct `gh` usage in the issues plugin
- manual smoke harness shell calls used only for operator verification

Audited exceptions must carry:
- `TODO(ARCH-BOUNDARY-001)` source annotation
- a linked GitHub issue
- an entry in `scripts/ci/gh_boundary_allowlist.txt`

## New Provider Checklist

When adding or refactoring a provider:
1. Keep provider-neutral traits and ledger types in `atm-ci-monitor`.
2. Put provider-specific API execution and translation in the provider layer.
3. Keep `atm-core` free of provider-specific types and subprocess execution.
4. Do not add `Command::new("gh")` outside the provider layer.
5. If an exception is unavoidable temporarily, add:
   - `TODO(ARCH-BOUNDARY-001)` in source
   - a GitHub issue
   - an allowlist entry
6. Run `scripts/ci/gh_boundary_check.sh` before pushing.

## Audit Results

Current direct `gh` execution / boundary exceptions:

| Violation | Crate / Area | File:Line | Issue | Status |
|---|---|---|---|---|
| Root tracking issue for GitHub boundary elimination plan and enforcement follow-up | planning / governance | `docs/requirements.md` | [#807](https://github.com/randlee/agent-team-mail/issues/807) | root tracking issue |
| Raw `gh` subprocess in provider-agnostic crate | `atm-ci-monitor` | `crates/atm-ci-monitor/src/github_provider.rs:173` | [#809](https://github.com/randlee/agent-team-mail/issues/809) | audited temporary exception |
| Raw `gh --version` bootstrap probe | `atm` CLI | `crates/atm/src/commands/gh.rs:2154` | [#811](https://github.com/randlee/agent-team-mail/issues/811) | audited temporary exception |
| Raw `gh auth status` bootstrap probe | `atm` CLI | `crates/atm/src/commands/gh.rs:2167` | [#811](https://github.com/randlee/agent-team-mail/issues/811) | audited temporary exception |
| Raw `gh` subprocess in issues plugin | `atm-daemon` issues plugin | `crates/atm-daemon/src/plugins/issues/github.rs:31` | [#812](https://github.com/randlee/agent-team-mail/issues/812) | audited temporary exception |
| Direct `gh api rate_limit` shell call | manual smoke harness | `scripts/dev-daemon-smoke.py:117` | [#813](https://github.com/randlee/agent-team-mail/issues/813) | audited temporary exception |
| Direct `gh pr list` shell call | manual smoke harness | `scripts/dev-daemon-smoke.py:131` | [#813](https://github.com/randlee/agent-team-mail/issues/813) | audited temporary exception |
| Direct `gh run list` shell call | manual smoke harness | `scripts/dev-daemon-smoke.py:145` | [#813](https://github.com/randlee/agent-team-mail/issues/813) | audited temporary exception |

Additional notes:
- `atm-core` non-dev dependency on `agent-team-mail-ci-monitor` was removed by
  AS5-ARCH-001 and is no longer a live violation.
- The current audit found no `Command::new("gh")` call already living in the
  gh-monitor provider layer.
- `crates/atm-daemon/src/plugins/ci_monitor/test_support.rs` writes fake `gh`
  scripts for tests but does not launch the real GitHub CLI; it is not counted
  as a boundary violation.

## CI Gate

`scripts/ci/gh_boundary_check.sh` is the enforcement entrypoint.

It must:
- fail on any new raw `gh` subprocess path outside the provider layer
- fail on any new non-dev `atm-core -> atm-ci-monitor` dependency
- permit only the audited exceptions listed in the allowlist
- print file, line, and `ARCH-BOUNDARY-001` in failure output
