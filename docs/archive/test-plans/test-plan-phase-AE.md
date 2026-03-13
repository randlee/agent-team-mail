# Phase AE Test Plan: GH Monitor Reliability + Daemon Observability

Last updated: 2026-03-08

## Goal

Provide explicit issue-to-test coverage and closeout disposition for Phase AE.
This plan covers:

- #499, #500 (config discovery/init contracts)
- #502, #503, #504, #505 (monitor status/reload/output consistency)
- #506 (same-identity concurrent-session send behavior)
- #472, #473, #474 (daemon observability/init isolation)

## Issue-to-Coverage Matrix

| Issue | Sprint | Requirement Contract | Implementation PR | Automated Coverage |
|---|---|---|---|---|
| [#499](https://github.com/randlee/agent-team-mail/issues/499) | AE.1 | `docs/ci-monitoring/requirements.md` GH-CI-FR-19 | [#518](https://github.com/randlee/agent-team-mail/pull/518) | `crates/atm-daemon/src/daemon/socket.rs::test_gh_monitor_uses_repo_config_source_from_payload_cwd`; `test_gh_status_uses_global_config_source_when_repo_missing`; `test_gh_monitor_health_reports_global_config_source` |
| [#500](https://github.com/randlee/agent-team-mail/issues/500) | AE.1 | GH-CI-FR-20..23 | [#518](https://github.com/randlee/agent-team-mail/pull/518) | `crates/atm/tests/integration_gh.rs::test_gh_init_dry_run_does_not_write_config`; `test_gh_init_writes_plugin_config`; `test_gh_monitor_fails_with_actionable_guidance_when_plugin_unconfigured` |
| [#502](https://github.com/randlee/agent-team-mail/issues/502) | AE.3 | Phase AE reload semantics (`docs/phase-ae-planning.md`) | [#521](https://github.com/randlee/agent-team-mail/pull/521) | `crates/atm-daemon/src/daemon/socket.rs::test_gh_monitor_restart_reloads_updated_config_without_daemon_restart`; `test_gh_monitor_invalid_config_transitions_to_disabled_config_error` |
| [#503](https://github.com/randlee/agent-team-mail/issues/503) | AE.2 | Phase AE live-status contract (`docs/phase-ae-planning.md`) | [#519](https://github.com/randlee/agent-team-mail/pull/519) | `crates/atm/tests/integration_gh.rs::test_gh_monitor_workflow_roundtrip_json`; `test_gh_monitor_lifecycle_status_roundtrip_json`; `test_gh_namespace_status_no_subcommand_returns_json_status` |
| [#504](https://github.com/randlee/agent-team-mail/issues/504) | AE.2 | GH-CI-FR-24 + JSON/status output contract | [#519](https://github.com/randlee/agent-team-mail/pull/519) | `crates/atm/tests/integration_gh.rs::test_gh_namespace_status_no_subcommand_returns_json_status`; `test_gh_monitor_status_json_has_stable_schema`; `test_gh_monitor_json_unavailable_emits_structured_error` |
| [#505](https://github.com/randlee/agent-team-mail/issues/505) | AE.2 | Phase AE status/reachability consistency contract (`docs/phase-ae-planning.md`) | [#519](https://github.com/randlee/agent-team-mail/pull/519) | `crates/atm/tests/integration_gh.rs::test_gh_status_surfaces_consistent_when_daemon_unreachable`; `test_gh_namespace_status_no_subcommand_returns_json_status` |
| [#506](https://github.com/randlee/agent-team-mail/issues/506) | AE.5 | `docs/requirements.md` (`atm send`, same-identity concurrent-session behavior) | [#523](https://github.com/randlee/agent-team-mail/pull/523) | `crates/atm/src/commands/send.rs::test_resolve_sender_session_id_errors_on_ambiguous_session_files_without_env`; `test_should_warn_self_send_true_when_same_session_owned`; `test_should_warn_self_send_false_when_different_active_session_owns_identity`; `test_should_warn_self_send_true_when_sender_session_unknown` |
| [#472](https://github.com/randlee/agent-team-mail/issues/472) | AE.4 | Phase AE daemon observability contract (`docs/phase-ae-planning.md`) | [#522](https://github.com/randlee/agent-team-mail/pull/522) | `crates/atm-core/tests/daemon_writer_fan_in.rs::daemon_writer_mode_wires_producer_sender_and_spools_emitted_event` |
| [#473](https://github.com/randlee/agent-team-mail/issues/473) | AE.4 | Phase AE autostart observability contract (`docs/phase-ae-planning.md`) | [#522](https://github.com/randlee/agent-team-mail/pull/522) | `crates/atm-core/tests/daemon_autostart_observability.rs::autostart_failure_logs_structured_event_with_stderr_tail_context`; `crates/atm-core/src/daemon_client.rs::test_ensure_daemon_running_includes_stderr_tail_on_startup_exit` |
| [#474](https://github.com/randlee/agent-team-mail/issues/474) | AE.4 | Fail-open plugin init contract (`docs/requirements.md` daemon plugin init) | [#522](https://github.com/randlee/agent-team-mail/pull/522) | `crates/atm-daemon/src/plugin/registry.rs::test_init_all_isolates_failed_plugins`; `test_repeated_init_faults_remain_bounded_to_plugin_count`; `test_plugin_recovery_after_config_correction_and_reload`; `crates/atm/src/commands/doctor.rs::check_plugin_init_failures_reports_disabled_init_error` |

## Issue Disposition Record

| Issue | Disposition | PR | Status |
|---|---|---|---|
| #499 | Fixed via daemon+CLI config-source parity | #518 | Complete |
| #500 | Fixed via `atm gh init` contract and actionable guidance | #518 | Complete |
| #502 | Fixed via restart reload semantics + invalid-config transition coverage | #521 | Complete |
| #503 | Fixed via live status query path and roundtrip status coverage | #519 | Complete |
| #504 | Fixed via JSON/status schema + unavailable JSON contract | #519 | Complete |
| #505 | Fixed via status/reachability consistency checks across `gh`, `gh status`, `gh monitor status` | #519 | Complete |
| #506 | Fixed via session-aware self-send logic and deterministic sender-session resolution | #523 | Complete |
| #472 | Fixed via daemon writer fan-in producer wiring regression test | #522 | Complete |
| #473 | Fixed via autostart stderr-tail observability surfacing and regression test | #522 | Complete |
| #474 | Fixed via fail-open plugin init isolation + doctor visibility | #522 | Complete |

## Validation Commands

```bash
cargo test -p agent-team-mail --test integration_gh -- --nocapture
cargo test -p agent-team-mail commands::send::tests:: -- --nocapture
cargo test -p agent-team-mail-daemon test_init_all_isolates_failed_plugins -- --nocapture
cargo test -p agent-team-mail check_plugin_init_failures_reports_disabled_init_error -- --nocapture
```
