# Phase AD Planning

Status: Draft for review
Date: 2026-03-07

## Goal

Stabilize daemon/runtime path coherence and close GitHub monitor dogfood
blockers so `atm gh` workflows are usable without manual daemon surgery.

## Inputs

- Team-lead dogfood findings (`DG-001`..`DG-011`)
- Existing plugin requirements: `docs/ci-monitoring/requirements.md`
- Core daemon contracts: `docs/requirements.md` (single-instance, socket path,
  team isolation)

## Pre-AD Hotfix Track (Out of Phase AD Scope)

Per team-lead guidance, these are active bug-fix work items in a separate
hotfix track (`fix/gh-monitor-daemon-bugs`) and are not Phase AD planning scope:
- `DG-001` daemon process leak
- `DG-002` ATM_HOME/socket-path mismatch
- `DG-005` missing daemon stop/restart/reload UX

Phase AD planning should assume these are tracked/fixed independently and focus
on remaining design/requirements closure items.

## Explicit Non-Scope

- Pre-release flaky test hotfix (`write_hook_auth_team_config` fsync fix, PR #493)
  is tracked separately and is not part of Phase AD implementation scope.

## Design Gaps to Lock in AD.1

1. Runtime detection strategy priority order (for `atm init` hook install matrix):
   - Claude: `~/.claude/settings.json` exists OR `.claude/settings.json` in cwd.
   - Codex: `codex` on PATH OR `~/.config/codex/config.json`.
   - Gemini: `gemini` on PATH OR `~/.gemini/` directory.
   - Rule: detected if binary is reachable OR config directory/file is present.
2. Idempotent hook install definition:
   - "No duplicate hook entries" means command-level dedup (string/payload
     equivalence), not only top-level hook key presence.
3. Migration contract for removing bash wrappers from settings:
   - existing users must have explicit migration path and dry-run preview.
4. `atm gh init` onboarding contract:
   - command exists, validates prerequisites, and prints exact next-step config/actions.
5. Daemon path/config coherence contract:
   - one canonical daemon home/socket resolution per scope
   - repo/global config discovery semantics independent of daemon process cwd.

## Sprint Plan

### AD.1 — Requirements and Contract Lock

Deliverables:
1. Lock daemon home/socket path contract to prevent split-brain (`DG-002`).
2. Lock single-instance/auto-start safety acceptance criteria (`DG-001`).
3. Lock plugin config discovery/reload semantics (`DG-003`, `DG-006`, `DG-009`).
4. Lock `atm gh init` and disabled-config UX contract (`DG-004`, `DG-010`).
5. Lock runtime hook detection + idempotency + migration requirements.

Acceptance:
- Requirements + planning + test-plan updates reviewed and approved before code.

### AD.2 — Daemon Process and Path Coherence Hardening

Targets:
- `DG-003`, `DG-004`

Acceptance:
- `atm gh init` exists and succeeds/fails with actionable output.
- Daemon/plugin sees effective repo/global config according to documented precedence.
- `atm gh` and `atm gh status --json` both expose canonical configured/enabled/availability state.

### AD.3 — GH Plugin Onboarding and Config Visibility

Targets:
- `DG-006`, `DG-007`, `DG-008`, `DG-009`, `DG-010`, `DG-011`

Acceptance:
- Status surfaces query daemon truth (or explicitly label degraded fallback).
- No duplicated status output blocks.
- Restart/reload semantics are deterministic and test-covered.

### AD.4 — Lifecycle/Status Coherence and Operator UX

Targets:
- runtime hook detection/idempotency contract + wrapper migration planning

Acceptance:
- Runtime detection order and hook dedup behavior are testable and explicit.
- Migration path for legacy wrappers is documented with dry-run preview semantics.

### AD.5 — Hook Wrapper Migration and Cleanup

Targets:
- migration path for wrapper removal + idempotent install UX hardening

Acceptance:
- Existing installs migrate safely with `--dry-run` preview and no duplicate entries.
- Runtime-specific hook install/removal is idempotent across repeated runs.

## Test Planning Notes

Phase AD test plan must include:
1. Repo/global config discovery tests with daemon cwd intentionally different
   from repo root (`DG-003`).
2. `atm gh init` UX tests (success, missing gh, missing auth, invalid config).
3. Live-vs-cached status coherency tests (`DG-007`, `DG-009`).
4. JSON status contract tests for `atm gh status --json` (`DG-008`).
5. Runtime-aware hook detection + idempotency tests (`atm init`, repeated runs).
