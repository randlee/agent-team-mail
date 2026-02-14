# Phase 6 Review — CI Monitor Plugin (ARCH-CTM)

**Worktree**: `planning/phase-7`  
**Date**: 2026-02-14

## Summary
Phase 6 delivers the CI Monitor plugin with GitHub Actions provider, mock provider, and an Azure external provider stub. Core functionality is present and tested. A set of review fixes were applied in the `review/arch-ctm-phase-6` branch to address dynamic provider loading, reporting, dedup behavior, config alignment, test isolation, and dedup eviction. Several design gaps remain for the post-Phase 6 planning session (multi-repo daemon model, repo vs root distinction, and repo-required plugins).

## Background / Use Cases Observed
- **Multi-repo workstation**: Single machine hosting many repos across multiple GitHub/Azure accounts; desire to monitor CI for all of them from one daemon.
- **Project-root ownership**: Team-lead or CI-monitor agent operates at project root (not necessarily identical to a git repo root).
- **Cross-repo monitoring**: CI for a repo may need to be monitored from a different project root (e.g., umbrella project with nested repos).
- **Multi-agent notifications**: Multiple agents may subscribe to the same repo (e.g., per-branch owners). Notifications should include co-recipient info for triage coordination.
- **Multi-repo config layout**: Mono-repo uses `config.atm.toml`; multi-repo uses machine-level repo list + per-repo `<repo>.config.atm.toml`.
  - Proposed paths: `~/.config/atm/daemon.toml` for machine-level daemon config; `<repo>/.atm/config.toml` for repo-level settings.

## Items Discussed This Session (Included in Review)

### Implemented Fixes (Phase 6 Review)
- **Dynamic CI provider loading wired**: Added CI provider loader, loads from provider directory and explicit libraries; keeps libraries alive to prevent unload.
- **Failure report generation**: JSON + Markdown reports generated under `report_dir` (default `temp/atm/ci-monitor/`).
- **Dedup strategy**: Default per-commit dedup with configurable per-run option; message IDs use dedup key.
- **Config alignment**: Updated config format and Azure provider README; added provider_config pass-through to external providers.
- **Test isolation**: Avoid per-test `ATM_HOME` mutation in CI monitor integration tests.
- **Tilde expansion**: `~/...` paths expanded for provider libraries.
- **Dedup cache eviction**: TTL-based eviction to prevent unbounded growth.

### Additional Fixes Applied After Review
- **Report path scoping**: Relative `report_dir` is resolved against repo root at init time (to avoid CWD-dependent output).

### Open Gaps / Design Issues
- **Multi-repo daemon model**: Current design assumes one daemon per repo. Paths, caches, and plugin state are repo-scoped. Needs a clear multi-repo model and repo/root scoping rules.
- **Root vs repo distinction**: There is always a workspace root, but repo is optional. Requirements and design should clearly separate root vs repo usage.
- **Repo-required plugins**: CI Monitor requires `SystemContext.repo`. In non-repo contexts, the plugin should disable itself with a warning or degrade gracefully.

## Gap Analysis
- **Configuration model**: Today config is repo-scoped. A root-level config that lists multiple repos and per-repo CI settings is required for multi-repo daemon mode.
- **Daemon lifecycle**: No explicit policy for single daemon per machine; startup/activation behavior needs to be defined.
- **Routing & subscriptions**: CI Monitor needs explicit per-repo/per-branch subscription metadata; agent settings must be separated from plugin settings.
- **Notification semantics**: Co-recipient warnings are not implemented; needed for multi-agent coordination.
- **Filter intent metadata**: Subscriptions lack a lightweight “reason/justification” field (optionally with expiry) to capture why a filter exists and for how long.

## Evidence and Notes
- CI Monitor plugin is present under `crates/atm-daemon/src/plugins/ci_monitor/`.
- External provider loading uses a new CI provider loader; Azure stub exists at `examples/ci-provider-azdo/`.
- Tests for CI Monitor cover failure notifications, dedup, branch filtering, error scenarios, and dynamic loading.

## Recommendations / Next Steps
1. **Phase 6.4 Design Reconciliation**: Run a focused planning session to reconcile root vs repo semantics and define multi-repo daemon support.
2. **Document repo-required plugins**: Ensure CI Monitor and similar plugins explicitly require repo context and define behavior when repo is absent.
3. **Plan path scoping**: Decide how plugin outputs and caches should be scoped across repos/roots under a single daemon.
