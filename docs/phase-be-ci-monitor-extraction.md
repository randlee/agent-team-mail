# Phase BE: CI-Monitor Extraction

**Version**: 0.1
**Date**: 2026-03-21
**Status**: PLANNED
**Integration branch**: `integrate/phase-BE`
**Prerequisites**: `develop` at Phase BB completion or later
**Dependency graph**: `BE.1 -> BE.2 -> BE.3 -> BE.4` (serial, BE.2 + BE.3 parallelizable)
**Planning worktree**: `planning/sc-observability-extraction`

## Goal

Extract CI monitoring out of the agent-team-mail workspace entirely. After
this phase, neither `atm` (CLI) nor `atm-daemon` reference `atm-ci-monitor`
in any way. CI monitoring becomes a standalone service with its own plugin
trait system.

## Design Decisions

### Two-crate target architecture

| Crate | Role |
|---|---|
| **`ci-monitor`** | Pure framework: `CiProvider` plugin trait, CI domain types, polling engine, base command implementations, `AlertSink` + `EventSink` abstraction traits, registry. **Zero knowledge of GitHub.** |
| **`gh-monitor`** | Dynamically loadable plugin: implements `CiProvider` for GitHub. Exports `atm_create_ci_provider_factory` C-ABI symbol. **Zero knowledge of ATM.** |

### Full abstraction stack — no layer knows its neighbors

- `ci-monitor` knows nothing about GitHub and nothing about ATM
- `gh-monitor` knows about GitHub and about `ci-monitor` traits — nothing about ATM
- `atm-daemon` knows about its plugin host interface — nothing about `ci-monitor` or `gh-monitor`
- `atm` CLI knows about the daemon client API — nothing about ci-monitor

Each layer communicates only through the abstraction it owns:
- ci-monitor provides base logic + plugin trait → gh-monitor implements it
- atm-daemon advertises a plugin service slot → gh-monitor registers via C-ABI
- atm CLI queries the daemon socket → daemon proxies plugin state

### Post-extraction architecture

The daemon is a **generic plugin host**. It advertises a plugin service
interface (via the existing C-ABI `CiProviderLoader` in `loader.rs`). It has
no compile-time knowledge of what plugin is installed — it discovers and loads
plugins at runtime.

```
Standalone repo: github.com/randlee/ci-monitor
  crates/
    ci-monitor/          (provider trait, domain types, polling engine)
      - CiProvider trait (plugin trait for providers)
      - ErasedCiProvider
      - CiRun, CiJob, CiStep, CiFilter, CiRunStatus, CiRunConclusion
      - CiProviderRegistry, CiProviderFactory
      - AlertSink trait    (abstracts alert output — no atm dep)
      - EventSink trait    (abstracts event logging — no atm dep)
      - Service/polling loop
      - MockCiProvider (feature = "test-support")

    gh-monitor/          (GitHub implementation, loadable plugin)
      - GitHubActionsProvider (implements CiProvider)
      - exports `atm_create_ci_provider_factory` C-ABI symbol
        → registered via daemon's CiProviderLoader at runtime
      - GhCliObserver + GhCliObserverContext (configurable paths)
      - GhLedger, GhRepoState, GhRateLimitTracker
      - Rate limit constants (moved from atm-core::consts)

ATM workspace after extraction:
  atm-daemon/
    - Plugin host: discovers + loads plugins via C-ABI (CiProviderLoader)
    - No hardcoded CiMonitorPlugin, no compile-time knowledge of ci-monitor or gh-monitor
    - Advertises plugin service slot; runtime config specifies which .dylib to load
    - No reference to ci-monitor or gh-monitor crates in Cargo.toml
  atm/ (CLI)
    - doctor.rs: GH state queries via daemon client API only
    - No direct ci-monitor dependency
```

The daemon already has `CiProviderLoader` (`loader.rs`) which loads
`.dylib/.so/.dll` files via `libloading` expecting the symbol
`atm_create_ci_provider_factory`. This is the correct interface —
`gh-monitor` exports this symbol, the daemon loads it. The daemon never
needs to know it's talking to a GitHub implementation.

---

## Current Coupling (from analysis)

### `atm-ci-monitor` → `agent-team-mail-core` (HIGH — all in `observability.rs:10-17`)

| Import | Source | Purpose |
|---|---|---|
| `GH_ACTIVE_POLL_INTERVAL_SECS` + 4 more constants | `atm-core::consts` | Default intervals/budgets |
| `EventFields`, `emit_event_best_effort` | `atm-core::event_log` | Event emission |
| `inbox_append` | `atm-core::io::inbox` | Budget warning alerts to team lead |
| `InboxMessage` | `atm-core::schema` | Message construction |
| `TeamConfigStore` | `atm-core::team_config_store` | Discover team lead agent |

### Daemon-specific types living in the wrong crate (`types.rs:42-177`)

`CiMonitorRequest`, `CiMonitorHealth`, `CiMonitorStatus`, `CiMonitorControlRequest`,
`CiMonitorLifecycleAction`, `CiMonitorTargetKind` — ATM-daemon wire types that
reference `team`, `config_cwd`, `actor_team`. These belong in atm-daemon, not
a standalone CI library.

### ATM CLI direct dependency (`atm/Cargo.toml:20`)

`doctor.rs` imports: `emit_gh_info_*`, `read_gh_repo_state`,
`update_gh_repo_state_rate_limit`, `GhCliObserverContext`, `flush_gh_observability_records`.

### Hardcoded `.atm/daemon/` paths

- `repo_state.rs:8`: `home.join(".atm/daemon/gh-monitor-repo-state.json")`
- `gh_ledger.rs:10`: `".atm/daemon/gh-observability.jsonl"`
- `observability.rs:707`: `home.join(".atm/daemon")`

---

## Sprint BE.1 — Observability Decoupling

**Goal**: Break `observability.rs`'s dependency on atm-core. Introduce
`AlertSink` and `EventSink` abstraction traits so ci-monitor emits alerts and
events without knowing about ATM's inbox or event log system.

### Deliverables

1. **Move GH constants** from `crates/atm-core/src/consts.rs` into
   `crates/atm-ci-monitor/src/consts.rs`. Add re-export shims in atm-core for
   backward compat.
   - Constants: `GH_ACTIVE_POLL_INTERVAL_SECS`, `GH_BUDGET_LIMIT_PER_HOUR`,
     `GH_IDLE_POLL_INTERVAL_SECS`, `GH_REPO_STATE_TTL_SECS`, `GH_WARNING_THRESHOLD`

2. **Introduce `AlertSink` trait** in `crates/atm-ci-monitor/src/alert_sink.rs`:
   ```rust
   pub trait AlertSink: Send + Sync {
       fn emit_budget_warning(&self, team: &str, repo: &str, used: u64, limit: u64, action: &str);
   }
   pub struct NoopAlertSink;
   impl AlertSink for NoopAlertSink { ... }
   ```

3. **Introduce `EventSink` trait** in `crates/atm-ci-monitor/src/event_sink.rs`:
   ```rust
   pub trait EventSink: Send + Sync {
       fn emit_event(&self, fields: &[(&str, &str)]);
   }
   pub struct NoopEventSink;
   impl EventSink for NoopEventSink { ... }
   ```

4. **Refactor `observability.rs`**: Replace all direct calls to
   `emit_event_best_effort`, `inbox_append`, `InboxMessage`, and
   `TeamConfigStore` with calls through `Arc<dyn AlertSink>` and
   `Arc<dyn EventSink>` stored in `GhCliObserverContext`.

5. **Make paths configurable**: Replace all hardcoded `.atm/daemon/` prefix
   references in `repo_state.rs`, `gh_ledger.rs`, and `observability.rs` with
   a `base_dir: PathBuf` parameter passed at construction time.

6. **Remove `agent-team-mail-core` dep** from `crates/atm-ci-monitor/Cargo.toml`.

7. **ATM implementations stay in atm-daemon**: Create
   `crates/atm-daemon/src/plugins/ci_monitor/atm_sinks.rs` with:
   - `AtmAlertSink` implementing `AlertSink` using `inbox_append` + `TeamConfigStore`
   - `AtmEventSink` implementing `EventSink` using `emit_event_best_effort`

### Acceptance Criteria

- `cargo tree -p agent-team-mail-ci-monitor` shows no `agent-team-mail-core` entries
- `cargo test --workspace` passes
- `cargo clippy --all-targets -- -D warnings` passes
- GH constants re-exported from atm-core compile cleanly for existing consumers

### Files Changed

| File | Action |
|---|---|
| `crates/atm-core/src/consts.rs` | Add re-export shims for 5 GH constants |
| `crates/atm-ci-monitor/src/consts.rs` | Add 5 GH constants (moved from atm-core) |
| `crates/atm-ci-monitor/src/alert_sink.rs` | New: `AlertSink` trait + `NoopAlertSink` |
| `crates/atm-ci-monitor/src/event_sink.rs` | New: `EventSink` trait + `NoopEventSink` |
| `crates/atm-ci-monitor/src/observability.rs` | Replace atm-core calls with trait objects |
| `crates/atm-ci-monitor/src/repo_state.rs` | Replace hardcoded path with `base_dir` param |
| `crates/atm-ci-monitor/src/gh_ledger.rs` | Replace hardcoded path with `base_dir` param |
| `crates/atm-ci-monitor/Cargo.toml` | Remove `agent-team-mail-core` dep |
| `crates/atm-daemon/src/plugins/ci_monitor/atm_sinks.rs` | New: `AtmAlertSink`, `AtmEventSink` |

---

## Sprint BE.2 — Type Migration

**Goal**: Move daemon-specific wire types out of `atm-ci-monitor` into
`atm-daemon`. After this sprint, `atm-ci-monitor` contains only generic CI
monitoring types and traits.

### Deliverables

1. **Move daemon wire types** from `crates/atm-ci-monitor/src/types.rs:42-177`
   to `crates/atm-daemon/src/plugins/ci_monitor/daemon_types.rs`:
   - `CiMonitorRequest`, `CiMonitorHealth`, `CiMonitorStatus`
   - `CiMonitorControlRequest`, `CiMonitorStatusRequest`
   - `CiMonitorLifecycleAction`, `CiMonitorTargetKind`

2. **Add re-export shim** in `atm-ci-monitor/src/types.rs` pointing to the
   moved daemon types — or simply update all callers in atm-daemon directly
   (preferred: update callers, remove shim).

3. **Verify `atm-ci-monitor` types.rs** contains only generic CI domain types:
   `CiRun`, `CiJob`, `CiStep`, `CiFilter`, `CiRunStatus`, `CiRunConclusion`,
   `CiProviderError`, `CiPullRequest`, raw GH types.

### Acceptance Criteria

- `cargo test --workspace` passes
- No ATM-specific field names (`actor_team`, `config_cwd`) remain in `atm-ci-monitor/src/types.rs`
- All daemon wire type imports in atm-daemon point to `crate::plugins::ci_monitor::daemon_types`

### Files Changed

| File | Action |
|---|---|
| `crates/atm-ci-monitor/src/types.rs` | Remove daemon wire types (lines 42-177) |
| `crates/atm-daemon/src/plugins/ci_monitor/daemon_types.rs` | New: moved daemon wire types |
| `crates/atm-daemon/src/plugins/ci_monitor/` (all files) | Update imports to local `daemon_types` |

---

## Sprint BE.3 — Plugin Command Extension Protocol

**Goal**: Define and implement a plugin command registration protocol so that
the atm CLI can discover and proxy commands advertised by loaded plugins. The
`atm gh` subcommand is removed from the CLI codebase entirely — it only exists
when gh-monitor is loaded and registered with the daemon.

### Design

Plugins advertise CLI commands to the daemon via a registration interface. The
atm CLI queries the daemon for available plugin commands and proxies them. If
no plugin advertising `gh` is loaded, `atm gh` does not exist.

```
atm gh pr list
  → atm CLI: query daemon socket for "gh" command handler
  → daemon: route to gh-monitor plugin's command handler
  → gh-monitor: execute, return result
  → atm CLI: print result
```

### Deliverables

1. **Define `PluginCommand` trait** in `atm-daemon` (or `atm-core` if the CLI
   also needs it at compile time for the proxy layer):
   ```rust
   pub trait PluginCommand: Send + Sync {
       fn namespace(&self) -> &str;          // e.g. "gh"
       fn handle(&self, args: &[&str]) -> CommandResult;
   }
   ```

2. **Define daemon socket message types** for plugin command discovery and
   dispatch:
   - `DaemonRequest::ListPluginCommands` → `Vec<PluginCommandDescriptor>`
     (name, namespace, subcommands, help text)
   - `DaemonRequest::PluginCommand { namespace, args }` → `PluginCommandResult`

3. **Remove hardcoded `atm gh` module** from `crates/atm/src/commands/`:
   - Delete `crates/atm/src/commands/gh.rs` (or equivalent)
   - Remove `gh` subcommand registration from `crates/atm/src/main.rs`

4. **Add dynamic plugin command proxy** to the atm CLI:
   - On `atm <unknown-subcommand>`: query daemon for registered plugin commands
   - If a plugin has registered the namespace, proxy args and stream output
   - If no plugin is registered: print "command not available (no plugin loaded)"
   - This replaces all hardcoded `atm gh` routing

5. **Remove `flush_gh_observability_records` from `atm/src/main.rs`** and
   **remove all ci-monitor imports from `doctor.rs`** — GH state is now
   accessible only via plugin command proxy.

6. **Remove `agent-team-mail-ci-monitor` from `crates/atm/Cargo.toml`**.

7. **Delete `crates/atm-daemon/src/plugins/ci_monitor/` module** and
   generalize the daemon plugin host:
   - Remove hardcoded `CiMonitorPlugin` from `crates/atm-daemon/src/main.rs`
   - Plugin loading becomes config-driven: `.atm.toml` lists plugin `.dylib`
     paths; daemon loads them at startup via existing `CiProviderLoader`
   - Remove `agent-team-mail-ci-monitor` from `crates/atm-daemon/Cargo.toml`

### Acceptance Criteria

- `atm gh` is not present in `clap` command registration (no hardcoded subcommand)
- `cargo tree -p agent-team-mail` shows no `agent-team-mail-ci-monitor` entries
- `cargo tree -p agent-team-mail-daemon` shows no `agent-team-mail-ci-monitor` entries
- `atm <unrecognized>` queries daemon for plugin commands and routes or fails gracefully
- `cargo test --workspace` passes
- `cargo clippy --all-targets -- -D warnings` passes

### Files Changed

| File | Action |
|---|---|
| `crates/atm/Cargo.toml` | Remove `agent-team-mail-ci-monitor` dep |
| `crates/atm/src/commands/gh.rs` | Delete |
| `crates/atm/src/main.rs` | Remove `gh` subcommand; add plugin command proxy |
| `crates/atm/src/commands/doctor.rs` | Remove all ci-monitor imports |
| `crates/atm-core/src/schema/` | Add `DaemonRequest::ListPluginCommands` + `PluginCommand` variants |
| `crates/atm-daemon/Cargo.toml` | Remove `agent-team-mail-ci-monitor` dep |
| `crates/atm-daemon/src/plugins/ci_monitor/` | Delete entire module |
| `crates/atm-daemon/src/main.rs` | Remove `CiMonitorPlugin`; add config-driven plugin loader |
| `crates/atm-daemon/src/daemon/` | Add plugin command dispatch handler |

---

## Sprint BE.4 — Repo Split and Two-Crate Publish

**Goal**: Create the standalone `ci-monitor` repo with two crates, publish to
crates.io, delete `crates/atm-ci-monitor` from the atm workspace.

### Deliverables

1. **Create `github.com/randlee/ci-monitor` repo**:
   ```
   ci-monitor/
     Cargo.toml            (workspace)
     crates/
       ci-monitor/         (provider trait, domain types, polling, AlertSink, EventSink)
       gh-monitor/         (GitHubActionsProvider, GhCliObserver, GhLedger, GhRepoState)
     examples/
       ci-provider-azdo/   (moved from agent-team-mail/examples/)
     .github/workflows/
       ci.yml
       release.yml
   ```

2. **Split `atm-ci-monitor` into two crates**:
   - `ci-monitor`: `provider.rs`, `registry.rs`, `service.rs`, `consts.rs`,
     `alert_sink.rs`, `event_sink.rs`, generic types from `types.rs`, `mock_provider.rs`
   - `gh-monitor`: `observability.rs`, `repo_state.rs`, `gh_ledger.rs`,
     GH-specific types, `GitHubActionsProvider` (from atm-daemon's `github_provider.rs`)

3. **Publish to crates.io** in order:
   - `ci-monitor@0.1.0`
   - `gh-monitor@0.1.0`

4. **Update atm workspace**:
   - Remove `crates/atm-ci-monitor` from workspace `members`
   - Remove `agent-team-mail-ci-monitor` workspace dep entry
   - Remove `crates/atm-ci-monitor/` directory
   - Remove `examples/ci-provider-azdo/` directory (moved to new repo)
   - Update `release/publish-artifacts.toml`: remove `agent-team-mail-ci-monitor` entry

5. **CI validation**: atm workspace CI passes with no ci-monitor references.

### Acceptance Criteria

- `ci-monitor` and `gh-monitor` visible on crates.io at `0.1.0`
- No `crates/atm-ci-monitor` directory in atm workspace
- `cargo build --workspace` passes in atm repo
- GitHub Actions CI passes on new `ci-monitor` repo

---

## Risk Assessment

| Risk | Severity | Mitigation |
|---|---|---|
| `doctor.rs` GH state display breaks when ci-monitor dep removed | MEDIUM | Replace with daemon client proxy or remove section; document in BE.3 spec |
| Deleting `CiMonitorPlugin` breaks live daemon users | MEDIUM | Phase out over a version boundary; announce in changelog |
| `examples/ci-provider-azdo` relies on workspace version | LOW | Move to new repo with its own version; already excluded from workspace |
| `AlertSink`/`EventSink` trait object sizing | LOW | Use `Arc<dyn AlertSink>` — heap allocation, no size issues |
| publish ordering (ci-monitor before gh-monitor) | LOW | Standard topological order; ci-monitor is leaf |
| crates.io 403 on publish (issue #399) | KNOWN | Manual GitHub Release fallback |

## Out of Scope

- A standalone `ci-monitor` binary/service (useful future work but not required for extraction)
- Replacing daemon CI monitoring with the standalone service (that is a follow-on product decision)
- Azure DevOps provider implementation (example only; follows the same `CiProvider` trait pattern)
- atm-core re-export shim removal for the 5 GH constants (deferred cleanup)
