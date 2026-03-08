# Phase AD Planning: Cross-Platform Script Standardization

## Goal

Eliminate product/runtime dependence on shell scripts (`bash`/`pwsh`) and
standardize ATM runtime scripting on Python for cross-platform behavior.

## Requirements Lock (Phase AD Scope)

1. Product/runtime script paths are Python-only.
2. Shell scripts are dev/CI-only exceptions unless explicitly approved.
3. `atm init` auto-installs runtime hook/config wiring for all detected runtimes:
   Claude Code, Codex CLI, Gemini CLI.
4. Runtime install behavior is per-runtime idempotent and reports status per runtime.
5. Hook/script behavior is covered by pytest and included in required CI checks.

## Requirements Cross-References

1. `docs/requirements.md` §4.9.3a (Python-only runtime scripts; shell exceptions policy).
2. `docs/requirements.md` §4.9.5 (`atm init` runtime detection + idempotent install contract).
3. `docs/plugins/ci-monitor/requirements.md` GH-CI-FR-19..24 and GH-CI-TR-7
   (dogfood gaps discovered during AD planning).
4. `docs/issues.md` section "Phase AD Dogfood Blockers" (`DG-001..DG-011`).

## Pre-AD Hotfix Scope (Out-of-Phase AD Sprint Work)

These are release-blocking bugs being fixed separately from AD implementation sprints:

1. DG-001 / [#497](https://github.com/randlee/agent-team-mail/issues/497):
   daemon process leak (multiple daemon instances).
2. DG-002 / [#498](https://github.com/randlee/agent-team-mail/issues/498):
   daemon socket-path mismatch across contexts.
3. DG-005 / [#501](https://github.com/randlee/agent-team-mail/issues/501):
   missing daemon stop/restart/reload operational controls.

## Violation Inventory (Input to AD Sprints)

### Must Remediate

1. `.claude/settings.json` bash wrapper commands (`bash -c`) in hook wiring paths.
2. `scripts/atm-hook-relay.sh` (Codex relay) shell runtime dependency.
3. `scripts/spawn-teammate.sh` shell launcher dependency.
4. `scripts/launch-worker.sh` shell launcher dependency.

### Review / Absorb (Mapped)

1. `scripts/setup-codex-hooks.sh` must be absorbed into `atm init` runtime install behavior (AD.5).
2. `.github/workflows/*.yml` shell steps remain CI-only dev exception with explicit policy note (AD.1 docs lock).

## Dogfood Findings to Sprint Mapping

| Finding | GitHub Issue | Planned Sprint | Notes |
|---|---|---|---|
| DG-001 | [#497](https://github.com/randlee/agent-team-mail/issues/497) | Pre-AD hotfix | Outside AD sprint scope |
| DG-002 | [#498](https://github.com/randlee/agent-team-mail/issues/498) | Pre-AD hotfix | Outside AD sprint scope |
| DG-003 | [#499](https://github.com/randlee/agent-team-mail/issues/499) | AD.2 | Daemon/CLI config discovery parity |
| DG-004 | [#500](https://github.com/randlee/agent-team-mail/issues/500) | AD.1 | `atm gh init` guided setup contract |
| DG-005 | [#501](https://github.com/randlee/agent-team-mail/issues/501) | Pre-AD hotfix | Outside AD sprint scope |
| DG-006 | [#502](https://github.com/randlee/agent-team-mail/issues/502) | AD.4 | Reload must apply updated config |
| DG-007 | [#503](https://github.com/randlee/agent-team-mail/issues/503) | AD.4 | Status must query live daemon state |
| DG-008 | [#504](https://github.com/randlee/agent-team-mail/issues/504) | AD.3 | `--json` status support |
| DG-009 | [#505](https://github.com/randlee/agent-team-mail/issues/505) | AD.4 | Reachability behavior consistency |
| DG-010 | [#506](https://github.com/randlee/agent-team-mail/issues/506) | AD.1 | Actionable disabled guidance |
| DG-011 | [#507](https://github.com/randlee/agent-team-mail/issues/507) | AD.3 | Remove duplicate status block output |

## AD.1 — Python Runtime Policy + `atm init` Runtime Auto-Install Contract

### Objective

Lock policy + architecture contract before conversion implementation sprints.

### Deliverables

1. Requirements updates for Python-only runtime policy + shell exception boundaries.
2. `atm init` runtime detection contract for Claude/Codex/Gemini:
   - detection criteria (binary reachable OR config location present)
   - per-runtime status output
   - idempotent re-run semantics (no duplicate hook entries)
3. Guided setup requirements for `atm gh init` and actionable disabled-config messaging.
4. Test-plan coverage matrix for runtime detection/install idempotency and pytest CI lane.

### Acceptance Criteria

1. Requirements explicitly prohibit shell as a product runtime dependency.
2. `atm init` contract is fully specified for all supported runtimes.
3. DG-004 and DG-010 requirements are locked with deterministic CLI behavior.
4. Follow-on AD sprint mapping is complete and traceable to issues.

## AD.2 — Runtime Config Discovery Parity

### Objective

Eliminate daemon/CLI config-path drift for repo-local plugin configuration.

### Deliverables

1. Define and implement daemon/CLI shared config-resolution contract.
2. Ensure repo `.atm.toml` and global config precedence is deterministic and documented.
3. Add tests for daemon-start context vs CLI invocation context parity.

### Acceptance Criteria

1. DG-003 is closed: plugin config in repo is visible consistently to daemon + CLI.
2. Status surfaces effective config source/path for diagnostics.
3. No regression in team-scoped runtime behavior.

## AD.3 — GH Status Surface Hardening

### Objective

Make `atm gh`/`atm gh monitor status` machine-consumable and non-ambiguous.

### Deliverables

1. Add JSON status mode for monitor status surfaces.
2. Remove duplicated status blocks and keep one canonical status rendering path.
3. Add tests for human + JSON output consistency.

### Acceptance Criteria

1. DG-008 is closed: `--json` status is supported and stable.
2. DG-011 is closed: no duplicate status output blocks.
3. Status payload fields are deterministic for automation and diagnostics.

## AD.4 — Live State + Reload Reliability

### Objective

Converge monitor runtime behavior on live daemon state and deterministic reload semantics.

### Deliverables

1. Monitor restart/reload must re-apply configuration changes without requiring daemon kill/restart.
2. Status commands must query daemon live state (not stale cache-only path).
3. Reachability semantics must be consistent across `atm gh status` and monitor commands.
4. Add integration tests for reload + live-state + reachability consistency.

### Acceptance Criteria

1. DG-006 is closed: reload path applies config changes.
2. DG-007 is closed: status reflects live daemon truth.
3. DG-009 is closed: consistent reachability results across all `atm gh` command paths.

## AD.5 — Runtime Script Conversion + Install Absorption

### Objective

Convert remaining product shell dependencies to Python and absorb legacy setup scripts into `atm init`.

### Deliverables

1. Replace `scripts/atm-hook-relay.sh` with Python equivalent and update installer/deployment paths.
2. Replace `scripts/spawn-teammate.sh` + `scripts/launch-worker.sh` with Python implementations.
3. Absorb `scripts/setup-codex-hooks.sh` behavior into `atm init` runtime install flow.
4. Add migration behavior for already-installed users to replace shell wrappers safely.

### Acceptance Criteria

1. Product runtime no longer depends on shell scripts for supported user paths.
2. `atm init` is the supported setup entrypoint for hook install across runtimes.
3. Pytest + CI cover migrated Python scripts and idempotent reinstall behavior.

## AD.6 — Remaining Wrapper Cleanup (Candidate Follow-On)

### Objective

Remove any residual product-facing shell wrappers discovered after AD.5 implementation.

### Deliverables

1. Final repo scan for runtime shell dependencies.
2. Replace or formally classify any residual scripts as documented dev exceptions.
3. Close remaining policy/documentation drift.

### Acceptance Criteria

1. No undocumented product runtime shell dependencies remain.
2. Exceptions are explicitly documented with justification.
