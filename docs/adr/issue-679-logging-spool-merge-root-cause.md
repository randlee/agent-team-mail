---
issue: 679
title: "degraded_spooling: LogWriterConfig default path diverges from configured_log_path SSoT"
date: 2026-03-12
worktree: fix/issue-679-spool-merge
status: ready-to-implement
---

# Issue #679: degraded_spooling Root Cause & Fix Blueprint

## Root Cause

**Path mismatch** between two independent defaults for the canonical log location:

| Location | Default path produced |
|----------|-----------------------|
| `log_writer.rs:148-151` (`LogWriterConfig::from_env`) | `{home}/.config/atm/atm.log.jsonl` |
| `logging_event.rs:620` (`configured_log_path`) | `{home}/.config/atm/logs/atm/atm.log.jsonl` |

`merge_spool_on_startup` and `run_log_writer_task` use `LogWriterConfig` → write to the **wrong path**. The health check (`build_logging_health_snapshot`) uses `configured_log_path` → reads from the **correct path** and finds no file.

Secondary: `spool_metrics` counts ALL files in the spool directory including `.claiming` transient lock files, inflating the pending count.

## Failure Sequence

1. Producer writes spool files to `~/.config/atm/logs/atm/spool/*.jsonl` while daemon offline.
2. Daemon starts. `merge_spool_on_startup` claims files (→ `.claiming`), reads events.
3. `append_events_to_log` writes to wrong path `~/.config/atm/atm.log.jsonl` — succeeds but misplaced.
4. `.claiming` files deleted. Spool dir empty briefly.
5. Any new producer activity → new spool files. No periodic merge → accumulate indefinitely.
6. `spool_metrics` sees pending files → `degraded_spooling` forever.

## Fixes

### Change 1 (Primary) — `log_writer.rs:148`

```rust
// BEFORE:
.unwrap_or_else(|_| home_dir.join(".config/atm/atm.log.jsonl"));

// AFTER:
.unwrap_or_else(|_| {
    agent_team_mail_core::logging_event::configured_log_path(home_dir)
});
```

No `Cargo.toml` changes needed — `agent_team_mail_core` already a dependency of `atm-daemon`.

### Change 2 (Secondary) — `event_loop.rs:1467` in `spool_metrics`

After the `is_file()` check, add `.jsonl` extension filter:

```rust
if !entry.file_name().to_str().map(|n| n.ends_with(".jsonl")).unwrap_or(false) {
    continue;
}
```

### Change 3 (Defensive) — `main.rs:112`

Use `configured_spool_dir` instead of `spool_dir` for explicit SSoT alignment (functionally equivalent when no env vars set, but makes intent clear).

## Tests to Add

**Test 1** (`log_writer.rs` test module):
```rust
fn log_writer_config_default_matches_configured_log_path()
// Assert: LogWriterConfig::from_env(home).log_path == configured_log_path(home)
```

**Test 2** (`event_loop.rs` test module):
```rust
fn test_spool_metrics_ignores_claiming_files()
// Setup: 1 .jsonl + 1 .claiming in spool dir
// Assert: snapshot.spool_count == 1
```

**Test 3** (`tests/test_spool_merge.rs`):
```rust
fn test_merge_clears_degraded_spooling_state()
// Full end-to-end: write spool file → merge → assert canonical log at configured_log_path → spool empty
```

## Implementation Checklist

- [ ] Change 1: `log_writer.rs:148` — use `configured_log_path(home_dir)`
- [ ] Change 2: `event_loop.rs` `spool_metrics` — filter `.jsonl` only
- [ ] Change 3: `main.rs:112` — use `configured_spool_dir`
- [ ] Test 1: path alignment unit test
- [ ] Test 2: `.claiming` filter unit test
- [ ] Test 3: end-to-end integration test
- [ ] `cargo test --workspace` — no regressions
- [ ] Manual verify: `atm doctor --json | jq '.logging_health.state'` → `"healthy"`

## Backward Compatibility

Old canonical log at `~/.config/atm/atm.log.jsonl` is orphaned (no tool ever read it via `configured_log_path`). Harmless. Changelog note recommended.

## Self-Healing

`degraded_spooling` state is derived on every 5s status tick. Once spool files are cleared, state self-heals on the next tick — no explicit reset needed.
