# Phase BB: Daemon Reset

**Version**: 0.2
**Date**: 2026-03-20
**Status**: PLANNED
**Integration branch**: `integrate/phase-BB`
**Prerequisites**: `develop` at `103bd127` or later, with Phases through BA
merged to `develop`
**Dependency graph**: `BB.0 -> BB.1 -> BB.2 -> BB.3 -> BB.4` (serial)
**Supersedes**: older multi-daemon planning assumptions in
`docs/requirements.md`, `docs/project-plan.md`, and the pre-BB daemon/runtime
model

## Goal

Remove multi-daemon support and replace the current daemon/runtime model with a
smaller, deterministic single-daemon design.

This phase is a deletion-heavy reset, not a feature phase.

## Why This Phase Exists

The current daemon model has accumulated:

- multi-daemon/runtime-mode branching,
- `ATM_HOME` coupling for both runtime and config,
- partial startup artifacts,
- asymmetric cleanup,
- hard-to-reason-about identity replacement logic,
- repeated reliability regressions across many follow-up phases.

Phase BB changes the strategy from patching symptoms to reducing the design.

## Canonical Roots for This Phase

Phase BB resolves the open path decision up front:

- **Config root**: `~/.claude`
- **Team state root**: `~/.claude/teams`
- **Runtime root**: `ATM_HOME`

`ATM_HOME` remains available for runtime-state overrides, but it must no longer
redirect team config, inboxes, or other stable team metadata.

## Sprint Plan

### BB.0 Dead Code Cleanup (Pre-Reset Trim)

Trim obviously dead code before the structural daemon reset starts.

Deliverables:

- remove unused `atm-core` schema/version scaffolding that never became part of
  the active runtime model
- remove dead/deprecated constructors and helpers that are provably uncalled
- reduce visibility on helpers that should be test-only or crate-private

Concrete candidates already identified:

- `crates/atm-core/src/schema/version.rs`
- `SystemContext.schema_version` and `with_schema_version()`
- deprecated `logging::init`
- `RetentionResult::new` / `CleanReportResult::new` visibility tightening

Acceptance:

- dead-code cleanup lands with no behavior change
- workspace validation remains green
- BB.1 and later sprints do not have to preserve these obsolete surfaces

Decision:

- `BB.0` is intentionally a pre-phase cleanup sprint because it removes code
  that no longer participates in any daemon-reset design choice.

### BB.1 Path Separation

Define and adopt separate APIs for:

- config root
- runtime root

Deliverables:

- team config/inbox paths stop resolving from `ATM_HOME`
- `ATM_HOME` becomes runtime-state-only
- `~/.claude` becomes the canonical config root and `~/.claude/teams` becomes
  the canonical team-state root
- daemon and CLI call sites migrate to the split path model
- BB.1 includes an explicit migration inventory for the currently known path
  split surface:
  - `crates/atm-core/src/home.rs`
  - `crates/atm-daemon/src/main.rs` config/bootstrap lookup
  - inline `get_home_dir()` / `teams_root_dir_for()` call sites in
    `crates/atm-daemon/src/daemon/socket.rs`
  - plugin init call sites in:
    - `crates/atm-daemon/src/plugins/ci_monitor/plugin.rs`
    - `crates/atm-daemon/src/plugins/issues/plugin.rs`
    - `crates/atm-daemon/src/plugins/worker_adapter/plugin.rs`

Acceptance:

- dev/shared runtime homes no longer break team config lookup
- `~/.claude/teams` is independent of runtime-home overrides
- the BB.1 migration inventory is complete enough that later sprints are not
  forced to rediscover path split call sites during implementation

### BB.2 Single-Daemon Model Collapse

Collapse daemon ownership to one system-daemon model.

Deliverables:

- remove multi-daemon runtime ownership support
- remove daemon runtime-kind arbitration for `release` vs `dev` shared daemons
- simplify admission/ownership checks around one daemon model
- update the cross-crate blast radius together in:
  - `atm-core`
  - `atm-daemon`
  - `atm-daemon-launch`
- replace the current `validate_runtime_admission_input` preconditions with one
  explicit invariant:
  - the daemon may only launch against the canonical runtime root for the user,
    with one approved daemon binary selection policy and no per-home ownership
    competition
- document the replacement for any security checks that currently rely on
  `RuntimeKind` branching or shared-vs-isolated runtime admission heuristics

Acceptance:

- daemon ownership no longer depends on per-home competition
- runtime-kind branching is reduced to what is still required for tests only, or
  removed entirely
- BB.2 is treated as dependent on BB.1 completion; it does not begin on an
  unresolved path model

### BB.3 Artifact Collapse and Transactional Startup

Reduce daemon runtime state to the minimum required artifact set.

Deliverables:

- startup publishes canonical state only after readiness
- stop/restart cleanup removes all daemon-owned runtime artifacts symmetrically
- remove obsolete sidecars and redundant ownership files where possible
- remove daemon/plugin surfaces that are dead in production and only add
  maintenance load to the daemon reset
- include a status schema migration note for `RuntimeOwnerMetadata` in
  `status.json` so `RuntimeKind` removal or collapse cannot become a silent
  deserialization hazard
- carry the BB.4 compatibility work needed for PID-file removal before BB.3 is
  allowed onto `integrate/phase-BB`; BB.3 must not land on the phase branch in
  a state that knowingly breaks the daemon test suite

Primary deletion candidates:

- `atm-daemon.pid`
- `daemon-touch.json`
- unregistered bridge plugin module under `crates/atm-daemon/src/plugins/bridge/`
- dead plugin capabilities and registry helpers with no production callers
- `crates/atm-daemon/src/plugins/issues/` entire module (~9 files): never
  enabled in any `.atm.toml`, `add_comment` returns placeholder data (never
  completed), and `github.rs` imports directly from
  `plugins::ci_monitor::run_attributed_gh_command_with_ids` — will not compile
  after ci_monitor is removed in Phase BE. Delete rather than carry forward.
- `crates/atm-daemon/src/main.rs:321-327`: `IssuesPlugin::new()` registration
  block — must be deleted alongside the module or the crate will not compile:
  ```rust
  if let Some(issues_config) = plugin_ctx.plugin_config("issues")
      && issues_config
      ...
      registry.register(agent_team_mail_daemon::plugins::issues::IssuesPlugin::new())
  ```
- Documentation references to the issues plugin:
  - `docs/requirements.md` lines ~2812 (`GhIssuesPlugin` factory example) and
    ~2843 (`[plugins.issues]` config block) — remove or replace with a note
    that the plugin was deleted
  - `docs/project-plan.md` sprint 5.2 row and AT.4/AT.6 rows — mark as deleted
  - `docs/arch-boundary.md` issues-plugin boundary rule and resolved findings
    rows — remove the rule; keep AT.4/AT.6 resolved entries as historical record
    or remove entirely

Acceptance:

- failed startup leaves no misleading live-daemon state
- stop/restart leaves no ambiguous stale-artifact combinations
- production-dead daemon plugin code is removed rather than carried through the
  reset
- BB.3 changes are integration-safe with the BB.4 test rewrite path; no phase
  branch state is allowed where PID-file deletion is merged but the test/model
  rewrite is absent

### BB.4 Test Model Rewrite and Final Deletion

Move daemon tests to the single-daemon model and delete the old complexity.

Deliverables:

- serialized/shared-fixture daemon test model
- obsolete multi-daemon tests removed
- dead code, env vars, docs, and fallback paths deleted
- the final daemon dogfood gate cites and reuses the canonical manual
  dev-daemon smoke protocol introduced in Phase AR (`docs/project-plan.md`
  section 17.26, plus `scripts/dev-daemon-smoke.py`)
- obsolete daemon/runtime planning docs are deleted or explicitly archived so
  the post-reset documentation surface is smaller and authoritative

Acceptance:

- daemon tests no longer depend on isolated per-test daemon instances
- dogfood start/restart/stop passes repeatedly on a clean machine/home under
  the new model
- deleted code outweighs newly added code for the phase

## Exit Criteria

Phase BB is complete only when:

1. multi-daemon support is removed from the active requirements and
   implementation,
2. config root and runtime root are separate,
3. dead-code cleanup identified in `BB.0` is complete,
4. daemon startup/shutdown behavior is deterministic,
5. stale-artifact ambiguity is removed,
6. dogfood start/restart/stop passes repeatedly on a clean machine/home using
   the canonical Phase AR smoke protocol,
7. the daemon code and documentation are materially smaller than before the
   phase began.
