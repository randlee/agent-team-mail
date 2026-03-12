# Flaky Hook Auth Fixture — Root Cause & Fix Design

**Status**: Hotfix recommended (apply to develop before v0.38.0 release)
**Author**: rust-architect (Opus)
**Date**: 2026-03-07

## Root Cause

The race lives in `authorize_hook_event` (socket.rs ~line 921):

```rust
let content = std::fs::read_to_string(&config_path)
    .map_err(|_| format!("team config not found: {}", config_path.display()))?;
```

`setup_hook_auth_fixture` creates a TempDir, sets `ATM_HOME` via `EnvGuard`, and calls `write_hook_auth_team_config` using a plain `std::fs::write()`. On macOS (APFS + `/private/var/folders` temp dirs), `std::fs::write` closes the file handle but does **not** guarantee cross-thread visibility without `fsync`. When the async hook handler runs on a different tokio thread, it may read before the write is visible.

## Scope: 26 Tests Vulnerable

| Category | Count |
|---|---|
| Tests using `setup_hook_auth_fixture` | 29 |
| With retry protection (`handle_hook_event_with_transient_retry`) | 3 |
| **Vulnerable (no protection)** | **26** |
| Failing on CI right now | 1 (`test_hook_event_duplicate_request_id_is_deduped_before_state_mutation`) |

## Fix: `File::sync_all()` in write functions

**Minimum change — 2 write sites in `#[cfg(test)]` module:**

### 1. `write_hook_auth_team_config` (~line 5828)

Replace:
```rust
std::fs::write(
    team_dir.join("config.json"),
    serde_json::to_string_pretty(&config).unwrap(),
).unwrap();
```

With:
```rust
let config_path = team_dir.join("config.json");
let config_bytes = serde_json::to_string_pretty(&config).unwrap();
{
    use std::io::Write;
    let file = std::fs::File::create(&config_path).unwrap();
    let mut writer = std::io::BufWriter::new(&file);
    writer.write_all(config_bytes.as_bytes()).unwrap();
    writer.flush().unwrap();
    file.sync_all().unwrap();
}
```

### 2. `set_member_backend` (~line 5873)

Replace:
```rust
std::fs::write(&cfg_path, serde_json::to_string_pretty(&cfg).unwrap()).unwrap();
```

With:
```rust
let cfg_bytes = serde_json::to_string_pretty(&cfg).unwrap();
{
    use std::io::Write;
    let file = std::fs::File::create(&cfg_path).unwrap();
    let mut writer = std::io::BufWriter::new(&file);
    writer.write_all(cfg_bytes.as_bytes()).unwrap();
    writer.flush().unwrap();
    file.sync_all().unwrap();
}
```

### 3. Optional: Add read-back assertion to `setup_hook_auth_fixture`

```rust
// Belt-and-suspenders: verify config is readable after sync
let config_path = temp.path()
    .join(".claude/teams")
    .join(team)
    .join("config.json");
assert!(
    config_path.exists() && std::fs::read_to_string(&config_path).is_ok(),
    "fixture config must be readable immediately after write+sync"
);
```

### 4. Cleanup: Remove `handle_hook_event_with_transient_retry` (if present)

If `handle_hook_event_with_transient_retry` exists in socket.rs, remove it and revert its 3 callers to `handle_hook_event_command`. The fsync fix makes the retry wrapper unnecessary.

## Impact

- **26 tests fixed** (all vulnerable tests)
- **Zero production code changes** (test-only `#[cfg(test)]` module)
- **Net ~20 lines removed** if retry helper also cleaned up

## Why Hotfix vs Phase AD Sprint

1. Test infrastructure bug, not a feature
2. Blocks CI reliability for all future PRs
3. Minimal change (2-function modification, test-only code)
4. Shipping v0.38.0 with known flaky CI undermines release confidence
5. Can be committed on a `fix/` branch → develop before the develop→main PR merges

## Key File

`crates/atm-daemon/src/daemon/socket.rs` — all changes in `#[cfg(test)]` module
