# Phase AC Test Plan: Daemon Status Convergence + Hook Install Confidence

Last updated: 2026-03-07

## Goal

Define implementation-ready tests for Phase AC.5 through AC.10 so daemon state tracking,
lifecycle handling, hook installation behavior, and release-confidence guardrails are
provably correct before global hook rollout.

## Scope

- Canonical daemon snapshot consistency across `atm doctor`, `atm status`, `atm members`
- Lifecycle transition correctness for Claude hook/event signals
- Idempotent/no-op handling for replayed or invalid session-end events
- Hook artifact parity between repo-local and embedded install script sources
- `atm init` local/global hook install confidence matrix
- Multi-team isolation + daemon restart recovery behavior

## Sprint Mapping

| Sprint | Focus | Notes |
|---|---|---|
| AC.5 | Daemon status convergence + lifecycle validation | Current branch work |
| AC.6 | Hook install confidence + multi-team recovery matrix | Next sprint |
| AC.7 | Hook lifecycle + restart convergence hardening | Branch `feature/pAC-s7-hook-lifecycle-coverage` |
| AC.8 | Init install matrix QA blocker closure | Branch `feature/pAC-s8-init-install-matrix` |
| AC.9 | Multi-team recovery determinism | Branch `feature/pAC-s9-multiteam-recovery` |
| AC.10 | Release confidence + regression guardrail pack | Branch `feature/pAC-s10-release-confidence` |

## AC.5 Test Matrix

### 1) Canonical Snapshot Consistency

Targets:
- `crates/atm/src/commands/doctor.rs`
- `crates/atm/src/commands/status.rs`
- `crates/atm/src/commands/members.rs`
- daemon `list-agents` handlers

Cases:
1. Same daemon state produces consistent liveness/activity render in all three commands.
2. `isActive=false` does not imply offline/dead in command output.
3. `isActive=true` without a daemon-backed live session does not imply online; render `Unknown` until liveness is confirmed.
4. Team-scoped run (`--team`) excludes agents from other teams.
5. Daemon-unreachable path renders `Unknown` (not offline/dead) with actionable finding.

AC.10 audit mapping (section 1 requirements -> tests):
1. Case 1 covered by:
   `crates/atm/tests/integration_conflict_tests.rs::test_doctor_status_members_consistent_unknown_when_daemon_unreachable`
2. Case 2 covered by:
   `crates/atm/tests/integration_conflict_tests.rs::test_doctor_status_members_consistent_unknown_when_daemon_unreachable`
3. Case 3 covered by:
   `crates/atm/tests/integration_conflict_tests.rs::test_doctor_status_members_consistent_unknown_when_daemon_unreachable`
4. Case 4 covered by:
   `crates/atm/tests/integration_external_member.rs::test_members_team_flag`
   `crates/atm/tests/integration_external_member.rs::test_status_team_flag`
   `crates/atm/src/commands/doctor.rs::tests::check_pid_session_reconciliation_ignores_foreign_state_entries`
5. Case 5 covered by:
   `crates/atm/tests/integration_conflict_tests.rs::test_doctor_status_members_consistent_unknown_when_daemon_unreachable`
   `crates/atm/src/commands/doctor.rs::tests::check_pid_session_reconciliation_query_none_maps_to_daemon_unreachable`
   `crates/atm/src/commands/doctor.rs::tests::build_recommendations_includes_daemon_start_for_unreachable`

### 2) Lifecycle Transition Coverage

Targets:
- `crates/atm-daemon/src/daemon/socket.rs`
- hook relay script handlers via `hook-event`

Cases:
1. `session_start` registers/activates session with correct team/agent/session/pid.
2. `permission_request` marks activity as blocked-permission (busy-equivalent state).
3. `stop` and `notification_idle_prompt` transition activity back to idle without changing liveness incorrectly.
4. `teammate_idle` remains supported and transitions activity to idle.
5. `session_end` for the active tracked session transitions the member to dead/offline on the canonical path.

Scope note:
- `hook.pre_compact` and `hook.compact_complete` remain covered by Phase Z.5
  lifecycle logging tests (PR #430) and are not redefined by AC.5.

### 3) Session-End Replay and Mismatch Behavior

Targets:
- `crates/atm-daemon/src/daemon/socket.rs`

Cases:
1. Unknown `(team, agent, session_id)` `session_end` is strict no-op (no new records).
2. Duplicate dead `session_end` replay is strict idempotent no-op.
3. Mismatched-session `session_end` does not terminate active record.

## AC.6 Test Matrix

### 4) Hook Artifact Parity (Local vs Embedded)

Targets:
- `.claude/scripts/*.py`
- `crates/atm/scripts/*.py`
- `tests/hook-scripts/*`

Cases:
1. `permission-request-relay.py` produces equivalent payload semantics from both roots.
2. `stop-relay.py` produces equivalent payload semantics from both roots.
3. `notification-idle-relay.py` produces equivalent payload semantics from both roots.
4. Session scripts (`session-start`, `session-end`) enforce same routing/guard behavior from both roots.
5. Shared helper (`atm_hook_lib.py`) behavior matches across both roots.

Project-only note:
- `gate-named-teammate.py` is intentionally project-local under `.claude/scripts/` and is
  not embedded under `crates/atm/scripts/`; parity checks must not require a crate copy.

### 5) `atm init` Install Matrix

Targets:
- `crates/atm/src/commands/init.rs`
- `crates/atm/tests/integration_init_onboarding.rs`

Cases:
1. Global install writes absolute script paths in hook commands (no `$CLAUDE_PROJECT_DIR` tokens in global-mode command strings).
2. Local install writes `$CLAUDE_PROJECT_DIR` script paths in hook commands (no absolute per-user script paths in local-mode command strings).
3. Re-running init is idempotent (no duplicate hook entries).
4. Existing non-ATM hooks are preserved.

### 6) Multi-Team Isolation + Restart Recovery

Targets:
- daemon integration tests (`crates/atm-daemon/tests/`)
- CLI integration tests (`crates/atm/tests/`)

Extension note:
- This section extends AC.5 section 1 with restart/recovery regressions and
  multi-team stress behavior; it is not a duplicate of the baseline consistency checks.

Cases:
1. Multiple teams active concurrently: no cross-team member/finding bleed.
2. Daemon restart preserves/restores canonical state expected by doctor/status/members.
3. Restart after partial lifecycle signals converges deterministically.

## AC.10 Audit Mapping — AC.6 Hook Artifact Parity

AC.6 parity test implementations (section 4 cases → concrete test paths):

| Case | Description | Test File | Test Function |
|------|-------------|-----------|---------------|
| AC.6-4/5 | Relay scripts exist in both roots (byte-identical) | `tests/hook-scripts/test_state_relay_hooks.py` | `test_parity_set_exists_in_both_script_roots`, `test_parity_set_is_byte_identical_between_roots` |
| AC.6-1/2/3 | Relay scripts send expected event with context | `tests/hook-scripts/test_state_relay_hooks.py` | `test_relay_scripts_send_expected_event_with_toml`, `test_relay_scripts_env_only_context_supported` |
| AC.6-noop | Relay scripts no-op without context | `tests/hook-scripts/test_state_relay_hooks.py` | `test_relay_scripts_no_context_noop` |

Note: `gate-named-teammate.py` is intentionally project-local (`.claude/scripts/`) and is NOT embedded under `crates/atm/scripts/`; parity checks exclude it per the AC.6 scope note.

Run: `python3 -m pytest tests/hook-scripts/ -q`

## AC.10 Audit Mapping — AC.9 Multi-Team Recovery

AC.9 test implementations (section 6 cases → concrete test paths):

| Case | Description | Test File | Test Function |
|------|-------------|-----------|---------------|
| AC.9-1 | No cross-team member/finding bleed | `crates/atm/tests/integration_multiteam_isolation.rs` | `test_cli_team_scoped_commands_do_not_bleed_members_across_teams` |
| AC.9-2 | Daemon restart preserves canonical state | `crates/atm/tests/integration_multiteam_isolation.rs` | `test_status_and_members_preserve_registered_member_state_after_daemon_restart` |
| AC.9-3 | Restart after partial signals converges | Not yet implemented — tracked as known gap (Unix daemon socket required) |

Note: AC.9 tests are gated `#![cfg(unix)]` — daemon socket IPC is Unix-only. Windows coverage gap is a known limitation (issue #372).

## Baseline Commands

```bash
python3 -m pytest tests/hook-scripts/ -q
cargo test -p agent-team-mail test_init_fresh_repo_creates_atm_toml_team_and_global_hooks -- --nocapture
cargo test -p agent-team-mail test_init_local_writes_project_settings_only -- --nocapture
cargo test -p agent-team-mail test_init_is_idempotent_on_rerun -- --nocapture
cargo test -p agent-team-mail-daemon -- --nocapture
cargo test -p agent-team-mail -- --nocapture
```

## Exit Criteria

1. AC.5 and AC.6 each have explicit, automated coverage mapped to acceptance criteria.
2. Canonical snapshot consistency is verified across doctor/status/members.
3. Hook install path confidence is demonstrated for both local and global scopes.
4. Multi-team + restart recovery regressions are covered before release/publish.
