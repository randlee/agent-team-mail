# Phase AL — Daemon Modularization

## Overview

Phase AL decomposes `crates/atm-daemon/src/daemon/socket.rs` (11,415 lines) into well-bounded sub-modules. The goal is zero behavioral changes — pure mechanical refactoring that moves code, adjusts visibility, and updates imports. Every sprint must produce a green CI and pass the full test suite.

The existing test suite (~5,382 lines in `socket.rs::tests`) provides a strong safety net. Tests move alongside the code they exercise.

## Current State

| File | Lines | Responsibility |
|---|---|---|
| `daemon/socket.rs` | 11,415 | Everything: server, dispatch, 8+ handler domains, all tests |
| `daemon/event_loop.rs` | ~900 | Main run loop, reconciliation |
| `daemon/session_registry.rs` | ~700 | Session tracking |
| `daemon/dedup.rs` | ~630 | Durable dedup store |
| Other daemon modules | <400 each | Well-bounded |

## Target State

`socket.rs` is eliminated entirely, replaced by:
- `daemon/shared_state.rs` (~120 lines) — shared type aliases and constructors
- `daemon/server.rs` (~250 lines) — socket server infrastructure
- `daemon/dispatch.rs` (~150 lines) — command routing
- `daemon/handlers/` (~1,900 lines across 6 files) — one file per handler domain
- `daemon/gh_monitor/` (~1,700 lines across 8 files) — GH monitor subsystem
- `daemon/helpers.rs` (~150 lines) — shared utilities

Tests move into sibling `tests` modules within each new file.

## Identified Functional Domains in socket.rs

1. **Socket server infrastructure** (lines 1–408): Server startup, accept loop, connection handler, platform stubs, shared state type aliases, launch channel types
2. **Command routing / dispatch** (lines 410–524): `handle_connection` dispatcher, `parse_and_dispatch` sync router
3. **Hook event processing** (lines 908–1932): Authorization, dedup, session lifecycle handlers, state transition emission
4. **Stream event handling** (lines 598–816): `stream-subscribe` long-lived connection, `stream-event` handler
5. **Log event handling** (lines 818–906): `log-event` command with validation and queue
6. **GH Monitor subsystem** (lines 2056–4378): Full plugin logic (~1,700 lines production code)
7. **Control command processing** (lines 4380–4849): Stdin enqueue, elicitation response, dedup, liveness
8. **Agent state / query handlers** (lines 4855–5996): `agent-state`, `list-agents`, `register-hint`, canonical member state derivation
9. **Tests** (lines 6033–11415): ~5,382 lines embedded in the module

## Target Module Structure

```
crates/atm-daemon/src/daemon/
  mod.rs                          # re-exports (updated)
  shared_state.rs                 # SharedStateStore, SharedPubSubStore, SharedStreamStateStore,
                                  # SharedStreamEventSender, LaunchSender, LaunchRequest,
                                  # SharedDedupeStore, new_*() constructors (~120 lines)
  server.rs                       # Socket server startup, accept loop, SocketServerHandle,
                                  # handle_connection dispatch, platform stubs (~250 lines)
  dispatch.rs                     # parse_and_dispatch, is_*_command matchers (~150 lines)
  handlers/
    mod.rs                        # sub-module declarations
    hook_event.rs                 # handle_hook_event_command_with_dedup, authorize_hook_event,
                                  # lifecycle event handlers, state transition emission (~600 lines)
    stream.rs                     # handle_stream_subscribe, handle_stream_event_command (~250 lines)
    log_event.rs                  # handle_log_event_command (~100 lines)
    control.rs                    # handle_control_command, process_control_request,
                                  # validate_control_request, content_ref, enqueue (~350 lines)
    agent_query.rs                # handle_agent_state, handle_list_agents, handle_agent_pane,
                                  # handle_session_query, handle_register_hint,
                                  # derive_canonical_member_state (~500 lines)
    pubsub.rs                     # handle_subscribe, handle_unsubscribe (~100 lines)
  gh_monitor/
    mod.rs                        # sub-module declarations, public handler entry points
    types.rs                      # GhRunView, GhRunJob, GhPullRequest, etc. (~100 lines)
    config.rs                     # evaluate_gh_monitor_config, GhMonitorConfigState (~100 lines)
    health.rs                     # health state file CRUD, health transitions (~200 lines)
    status.rs                     # status state file CRUD, upsert, key generation (~100 lines)
    polling.rs                    # monitor_gh_run, wait_for_pr_run_start, fetch_run_view (~300 lines)
    reporting.rs                  # format_progress_message, build_failure_payload,
                                  # classify_failure, is_infra_failure (~200 lines)
    alerts.rs                     # emit_ci_monitor_message, emit_merge_conflict_alert,
                                  # resolve_ci_alert_routing (~250 lines)
    gh_cli.rs                     # run_gh_command, fetch_failed_log_excerpt (~50 lines)
    handlers.rs                   # handle_gh_monitor_command, handle_gh_status_command (~400 lines)
  helpers.rs                      # make_ok_response, make_error_response, load_team_member,
                                  # emit_pid_process_mismatch, runtime_for_member (~150 lines)
  dedup.rs                        # (existing, unchanged)
  event_loop.rs                   # (existing, unchanged)
  log_writer.rs                   # (existing, unchanged)
  pid_backend_validation.rs       # (existing, unchanged)
  session_registry.rs             # (existing, unchanged)
  shutdown.rs                     # (existing, unchanged)
  spool_merge.rs                  # (existing, unchanged)
  spool_task.rs                   # (existing, unchanged)
  status.rs                       # (existing, unchanged)
  watcher.rs                      # (existing, unchanged)
```

## Sprint Breakdown

### Dependencies

```
AL.1 (shared_state + helpers)
  |
  v
AL.2 (gh_monitor extraction)  ----+
  |                                |
  v                                v
AL.3 (handler modules)        AL.4 (server + dispatch)
  |                                |
  +--------------------------------+
  |
  v
AL.5 (eliminate socket.rs, wire mod.rs)
  |
  v
AL.6 (doc + cleanup sweep)
```

AL.1 must go first (foundational types). AL.2 and AL.3 can run in parallel after AL.1. AL.4 depends on AL.3. AL.5 is the final merge that deletes socket.rs. AL.6 is polish.

---

### Sprint AL.1 — Extract Shared State Types and Helpers

**Goal**: Create `shared_state.rs` and `helpers.rs` with all shared type aliases, constructors, and utility functions.

**New files**:
- `daemon/shared_state.rs`: Move `SharedStateStore`, `SharedPubSubStore`, `SharedStreamStateStore`, `SharedStreamEventSender`, `LaunchSender`, `LaunchRequest`, `SharedDedupeStore`, and all `new_*()` constructors from socket.rs lines 56–277.
- `daemon/helpers.rs`: Move `make_ok_response`, `make_error_response`, `format_elapsed_as_iso8601`, `load_team_member`, `load_team_members`, `emit_pid_process_mismatch`, `runtime_for_member`, `bootstrap_session_from_member_hint`.

**Modified files**:
- `daemon/socket.rs`: Replace moved items with `use` imports.
- `daemon/mod.rs`: Add `pub mod shared_state; pub mod helpers;`.
- `daemon/event_loop.rs`: Update imports as needed.

**Estimated reduction**: ~400 lines removed from socket.rs.

**Risk**: Low. Leaf items, no circular dependencies.

**Verification**: `cargo test --workspace`, `cargo clippy --workspace`.

---

### Sprint AL.2 — Extract GH Monitor Subsystem

**Goal**: Move the entire GH Monitor subsystem (~1,700 lines production, ~800 lines tests) into `daemon/gh_monitor/`.

**New files**: `mod.rs`, `types.rs`, `config.rs`, `health.rs`, `status.rs`, `polling.rs`, `reporting.rs`, `alerts.rs`, `gh_cli.rs`, `handlers.rs` (see target structure above).

**Modified files**:
- `daemon/socket.rs`: Remove all GH Monitor code (lines ~2056–4378 + tests). Replace with `use crate::daemon::gh_monitor`.
- `daemon/mod.rs`: Add `pub mod gh_monitor;`.

**Estimated reduction**: ~2,500 lines removed from socket.rs.

**Risk**: Medium. GH monitor uses `helpers.rs` items — must run after AL.1. Test fixtures (fake-gh-script) are tightly coupled to test patterns; must move with the code.

**Verification**: `cargo test --workspace`. Specifically: `cargo test -p agent-team-mail-daemon gh_monitor`.

---

### Sprint AL.3 — Extract Handler Modules

**Goal**: Move each handler domain into `daemon/handlers/`.

**New files**: `mod.rs`, `hook_event.rs`, `stream.rs`, `log_event.rs`, `control.rs`, `agent_query.rs`, `pubsub.rs` (see target structure above).

**Key items moved**:
- `hook_event.rs`: `HookEventAuth`, `authorize_hook_event`, `handle_hook_event_command_with_dedup`, all lifecycle match arms, `TransitionEventSpec`, `collect_member_transition_events`, `HookLogContext`.
- `agent_query.rs`: `handle_register_hint`, `derive_canonical_member_state`, `derive_unregistered_member_state`.

**Modified files**:
- `daemon/socket.rs`: Remove all handler functions.
- `daemon/mod.rs`: Add `pub mod handlers;`.

**Estimated reduction**: ~3,500 lines removed from socket.rs.

**Risk**: Medium. Hook event handler is the largest and most complex piece. Test utilities (`HookAuthFixture`, `EnvGuard`, `write_hook_auth_team_config`, `set_member_backend`) must move with the code. Serial test annotations (`#[serial]`) must be preserved.

**Verification**: `cargo test --workspace`. Specifically: `cargo test -p agent-team-mail-daemon hook_event -- --test-threads=1`.

---

### Sprint AL.4 — Extract Server and Dispatch

**Goal**: Move socket server infrastructure and command routing into `daemon/server.rs` and `daemon/dispatch.rs`.

**New files**:
- `daemon/server.rs`: `start_socket_server`, `run_accept_loop`, `handle_connection`, `SocketServerHandle`, `cleanup_socket_files`.
- `daemon/dispatch.rs`: `parse_and_dispatch`, all `is_*_command` functions, `handle_agent_stream_state`.

**Modified files**:
- `daemon/socket.rs`: Should now be a near-empty shim.
- `daemon/mod.rs`: Add `pub mod server; pub mod dispatch;`.

**Estimated reduction**: ~500 lines removed from socket.rs.

**Risk**: Low-Medium. `handle_connection` references all handler modules — ensure all imports resolve.

**Verification**: `cargo test --workspace`. Integration test for daemon start/stop if available.

---

### Sprint AL.5 — Eliminate socket.rs and Finalize Module Wiring

**Goal**: Delete `daemon/socket.rs` entirely. Update `daemon/mod.rs` re-exports to maintain identical public API.

**Actions**:
- Delete `daemon/socket.rs`.
- Update `daemon/mod.rs`: Replace `pub mod socket;` and its re-exports with re-exports from all new modules.
- Audit and update all `use crate::daemon::socket::*` imports across the crate.
- Verify `main.rs` import paths work unchanged.

**Risk**: Medium. The "big bang" deletion step. Mitigated by prior sprints leaving socket.rs as a shrinking re-export shim.

**Verification**: `cargo test --workspace`, `cargo clippy --workspace`, `cargo doc --workspace --no-deps`.

---

### Sprint AL.6 — Documentation and Cleanup Sweep

**Goal**: Add module-level `//!` documentation to every new module. Final polish pass.

**Actions**:
- Add `//!` module docs to all 21 new files.
- Remove any `#[expect(clippy::too_many_arguments)]` no longer needed.
- `cargo fmt --all`, `cargo clippy --workspace`, `cargo test --workspace`.
- Verify no public API surface changes.

**Risk**: Very low. Pure documentation.

---

## Line Count Budget

| Sprint | Lines removed from socket.rs | New files |
|---|---|---|
| AL.1 | ~400 | 2 |
| AL.2 | ~2,500 | 10 |
| AL.3 | ~3,500 | 7 |
| AL.4 | ~500 | 2 |
| AL.5 | ~4,515 (remainder + deletion) | 0 |
| AL.6 | 0 | 0 |
| **Total** | **11,415** | **21 new files** |

Zero behavioral change. 11,415 lines redistributed across 21 focused files.

## Risk Summary

| Risk | Mitigation |
|---|---|
| Broken imports after move | Each sprint leaves socket.rs as a shrinking re-export shim until AL.5 |
| Test isolation (serial tests, env vars) | Tests move with handler code; `#[serial]` annotations preserved |
| Platform-gated code (`#[cfg(unix)]`) | Each handler file inherits same `#[cfg(unix)]` gates |
| Public API breakage | `daemon/mod.rs` re-exports updated to preserve identical paths |
| Merge conflicts with AJ/AK | AL runs after AK merges to develop |

## Integration Branch Strategy

```
develop
  └── integrate/phase-AL
        ├── feature/pAL-s1-shared-state-helpers
        ├── feature/pAL-s2-gh-monitor-extraction
        ├── feature/pAL-s3-handler-modules
        ├── feature/pAL-s4-server-dispatch
        ├── feature/pAL-s5-eliminate-socket
        └── feature/pAL-s6-doc-cleanup
```

AL.2 and AL.3 can be worked in parallel after AL.1 merges to integration branch.

## Acceptance Criteria

- [ ] AL.1: shared_state.rs + helpers.rs extracted, CI green
- [ ] AL.2: daemon/gh_monitor/ tree extracted, CI green
- [ ] AL.3: daemon/handlers/ tree extracted, CI green
- [ ] AL.4: server.rs + dispatch.rs extracted, CI green
- [ ] AL.5: socket.rs deleted, all re-exports wired, CI green, `cargo doc` clean
- [ ] AL.6: Module docs added, final clippy/fmt/doc pass green
- [ ] Phase integration PR: integrate/phase-AL → develop
