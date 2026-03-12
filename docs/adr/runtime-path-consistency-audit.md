---
title: "Runtime Path Consistency Audit — SSoT Helper Coverage"
date: 2026-03-12
worktree: fix/runtime-path-consistency-audit
status: phase-2-implemented
---

# Runtime Path Consistency Audit

## Existing SSoT Layers

**Tier 1 — Home resolution** (`atm-core/src/home.rs`):
- `get_home_dir()` — ATM_HOME env → `dirs::home_dir()` fallback
- `get_os_home_dir()` — always `dirs::home_dir()` (daemon socket pointer)

**Tier 2 — Daemon runtime** (`atm-core/src/daemon_client.rs:361-451`):
- `daemon_runtime_dir()`, `daemon_socket_path()`, `daemon_pid_path()`, `daemon_status_path()`,
  `daemon_lock_path()`, `daemon_start_lock_path()`, `daemon_dedup_path()`,
  `daemon_gh_monitor_health_path()` — all in `.atm/daemon/`

**Tier 2 — Log/spool** (`atm-core/src/logging_event.rs:602-668`):
- `configured_log_path_for_tool()`, `configured_spool_dir()`, etc.

**Enforcement**: `home_dir_audit.rs` catches raw `dirs::home_dir()` calls — but NOT raw `HOME`/`USERPROFILE` env var usage.

**What is MISSING** as centralized helpers (currently constructed ad-hoc throughout codebase):
- `claude_root(home)`, `teams_root(home)`, `team_dir(home, team)`, `team_config_path(home, team)`
- `inbox_path(home, team, agent)`, `claude_settings_path(home)`, `claude_scripts_dir(home)`, `claude_agents_dir(home)`
- `atm_config_dir(home)`, `sessions_dir(home)`

---

## BLOCKING Violations

### B1 — `version.rs:101-108` bypasses ATM_HOME entirely
**Current**: `std::env::var("HOME").or_else(|_| std::env::var("USERPROFILE"))` then `.join(".config/atm/claude-version.json")`
**Fix**: Use `get_home_dir()?.join(".config/atm/claude-version.json")`
**Impact**: Breaks in tests and custom deployments where ATM_HOME is set

### B2 — `spool.rs:325` uses stale spool path
**Current**: `home.join(".config/atm/spool").join(subdir)`
**Fix**: Use `configured_spool_dir(home)` → canonical `~/.config/atm/logs/atm/spool`
**Impact**: Spool files written to wrong location, never merged

### H18 — `ci_monitor/plugin.rs:729-733` uses `.claude/daemon/` (wrong namespace)
**Current**: `ctx.system.claude_root.join("daemon").join("gh-monitor-state.json")`
**Expected**: `~/.atm/daemon/gh-monitor-state.json` per daemon_runtime_dir pattern
**Action needed**: Investigate if `daemon_gh_monitor_health_path()` and this path are the same concept. If yes, fix to use the helper. Document if intentionally separate.

---

## HIGH — Missing helpers used ad-hoc (18 call sites)

| ID | File:Line | Pattern | Needed Helper |
|----|-----------|---------|---------------|
| H1 | `atm-daemon/src/main.rs:157` | `home_dir.join(".claude")` | `claude_root(home)` |
| H2 | `atm-daemon/src/main.rs:170` | `claude_root.join("teams")` | `teams_root(home)` |
| H3 | `event_loop.rs:568` | `claude_root.join("teams")` | `teams_root(home)` |
| H4 | `socket.rs:933-935` | `home_dir.join(".claude/teams/{team}/config.json")` | `team_config_path(home, team)` |
| H5 | `spawn.rs:36-38` | `home_dir.join(".claude").join("agents")` | `claude_agents_dir(home)` |
| H7 | `init.rs:188` | `home_dir.join(".claude").join("scripts")` | `claude_scripts_dir(home)` |
| H9 | `init.rs:912` | `home.join(".claude").join("settings.json")` | `claude_settings_path(home)` |
| H10 | `hook_identity.rs:181` | `home.join(".claude").join("teams")...` | `team_dir(home, team)` |
| H11 | `caller_identity.rs:687` | `.join(".claude").join("teams")...` | `team_dir(home, team)` |
| H12 | `atm_tools.rs:126-127` | `home.join(".claude").join("teams")...` | `inbox_path(home, team, agent)` |
| H13 | `atm_tools.rs:934` | `home.join(".claude").join("teams").join(team)` | `team_dir(home, team)` |
| H14 | `mail_inject.rs:265-268` | `home.join(".claude").join("teams")...` | `inbox_path(home, team, identity)` |
| H15 | `worker_adapter/plugin.rs:175-182` | `claude_root.parent()...join(".atm/daemon/hooks")` | `daemon_runtime_dir_for(home).join("hooks/events.jsonl")` |
| H17 | `hook_watcher.rs:683` | `claude_root.join("teams/{team}/config.json")` | `team_config_path(home, team)` |

**Note**: `atm-agent-mcp` has local private `inbox_path()` duplicated in two files — replace both with `home::inbox_path()`.

---

## MEDIUM — Satellite crate duplications

| ID | File | Issue |
|----|------|-------|
| M1 | `atm-agent-mcp/src/lock.rs:77-81` | `home.join(".config/atm/agent-sessions")` → use `sessions_dir(home)` |
| M4 | `sc-observability/src/lib.rs:381-398` | Duplicates log path logic from `configured_log_path_for_tool()` |

---

## New Helpers to Add — `atm-core/src/home.rs`

```rust
pub fn claude_root(home: &Path) -> PathBuf        // {home}/.claude
pub fn teams_root(home: &Path) -> PathBuf          // {home}/.claude/teams
pub fn team_dir(home: &Path, team: &str) -> PathBuf
pub fn team_config_path(home: &Path, team: &str) -> PathBuf
pub fn inbox_path(home: &Path, team: &str, agent: &str) -> PathBuf
pub fn claude_settings_path(home: &Path) -> PathBuf
pub fn claude_scripts_dir(home: &Path) -> PathBuf
pub fn claude_agents_dir(home: &Path) -> PathBuf
pub fn atm_config_dir(home: &Path) -> PathBuf      // {home}/.config/atm
pub fn sessions_dir(home: &Path) -> PathBuf        // {home}/.config/atm/agent-sessions
```

Note: `SystemContext.claude_root` stores a pre-resolved `PathBuf` — daemon code using it
does not need to change. New helpers serve code that currently constructs paths ad-hoc.

---

## Implementation Sequence

**Phase 1 — BLOCKING fixes:**
- [x] B1: `version.rs:101` — use `get_home_dir()` instead of raw env vars
- [x] B2: `spool.rs:325` — use `configured_spool_dir(home)` (verified live call site)
- [x] H18: Investigated `.claude/daemon` vs `.atm/daemon` for gh-monitor-state; fixed CI monitor state to use daemon runtime dir
- [x] Strengthen `home_dir_audit.rs`: also catch `std::env::var("HOME")` / `std::env::var("USERPROFILE")` in non-home.rs production code

**Phase 2 — Add centralized helpers to `atm-core/src/home.rs`:**
- [x] Add the 10 helpers above with unit tests (trivial path construction)
- [x] Re-export from `atm_core::home` (`pub mod home` already exposes the helpers directly; no `pub use home::*` flattening added)

## As-Built Helper Names

The implementation uses explicit `_for` suffixes for helpers that accept a resolved
`home: &Path` input, alongside the existing zero-arg `claude_root_dir()` /
`teams_root_dir()` wrappers that resolve `ATM_HOME` internally. This is intentional:

- `claude_root_dir_for(home)`
- `teams_root_dir_for(home)`
- `team_dir_for(home, team)`
- `team_config_path_for(home, team)`
- `inbox_path_for(home, team, agent)`
- `claude_settings_path_for(home)`
- `claude_scripts_dir_for(home)`
- `claude_agents_dir_for(home)`
- `atm_config_dir_for(home)`
- `sessions_dir_for(home)`

**Phase 3 — Migrate HIGH call sites (H1–H17):**
- [ ] Migrate all 14 call sites listed above (one commit per logical group: daemon init, socket, CLI, MCP, worker-adapter)

**Phase 4 — Migrate MEDIUM (M1, M4)**

**Phase 5 — Add path construction audit test** catching `.join(".claude").join("teams")` outside `home.rs`
