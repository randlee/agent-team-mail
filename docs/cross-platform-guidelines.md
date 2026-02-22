# Cross-Platform Guidelines

Rules and patterns for ensuring atm works correctly on Ubuntu, macOS, and Windows CI.

## Home Directory Resolution

**Problem**: `dirs::home_dir()` on Windows uses the Windows API (`SHGetKnownFolderPath`), which ignores both `HOME` and `USERPROFILE` environment variables. This breaks integration tests that set `HOME` to a temp directory.

**Solution**: Application code uses `get_home_dir()` from `crate::util::settings`, which checks `ATM_HOME` first:

```rust
pub fn get_home_dir() -> Result<PathBuf> {
    if let Ok(home) = std::env::var("ATM_HOME") {
        return Ok(PathBuf::from(home));
    }
    dirs::home_dir().ok_or_else(|| anyhow::anyhow!("Could not determine home directory"))
}
```

### Integration Test Pattern (MANDATORY)

Every integration test file MUST use this helper:

```rust
fn set_home_env(cmd: &mut assert_cmd::Command, temp_dir: &TempDir) {
    cmd.env("ATM_HOME", temp_dir.path());
}
```

**NEVER** use `.env("HOME", ...)` or `.env("USERPROFILE", ...)` in tests. These do not work on Windows.

### Verification

Before declaring dev work complete, grep all integration test files:
```bash
grep -rn 'env("HOME"' crates/atm/tests/ && echo "FAIL: Found HOME env usage" || echo "OK"
grep -rn 'env("USERPROFILE"' crates/atm/tests/ && echo "FAIL: Found USERPROFILE env usage" || echo "OK"
```

## Clippy Compliance

CI runs Rust 1.93 clippy with `-D warnings`. Local toolchains may be older and miss lints.

### Known Strict Lints

- **`collapsible_if`**: Nested `if`/`if let` chains must be collapsed using let chain syntax (stable since Rust 1.87):
  ```rust
  // BAD: nested if
  if path.is_file() {
      if let Ok(content) = fs::read_to_string(&path) {
          // ...
      }
  }

  // GOOD: collapsed with let chain
  if path.is_file()
      && let Ok(content) = fs::read_to_string(&path)
  {
      // ...
  }
  ```

- **Deprecated APIs**: Use `assert_cmd::cargo::cargo_bin_cmd!("atm")` instead of the deprecated `Command::cargo_bin("atm")`.

### Pre-Commit Check

Always run before declaring implementation complete:
```bash
cargo clippy -- -D warnings
```

## Temporary Files and Directories

**Problem**: `/tmp/` is a Unix-only path. Windows has no `/tmp/` directory — hardcoding it causes immediate failure on Windows CI.

**Solution**: Use `std::env::temp_dir()` for any temporary file path in production code. Use `tempfile::TempDir` for test isolation.

```rust
// BAD: Unix-only, fails on Windows
let path = PathBuf::from("/tmp/atm-session-id");

// GOOD: cross-platform
let path = std::env::temp_dir().join("atm-session-id");
```

**In tests**, always use a scoped `TempDir` rather than a fixed temp path — this avoids both the `/tmp` problem and test interference:

```rust
// BAD: hardcoded /tmp path in test
let path = PathBuf::from("/tmp/test-artifact");

// GOOD: temp_env-isolated TempDir
let dir = tempfile::tempdir().expect("temp dir");
let path = dir.path().join("test-artifact");
```

### Verification

Before declaring dev work complete, grep for hardcoded `/tmp`:
```bash
grep -rn '"/tmp/' crates/ && echo "FAIL: Found /tmp hardcoding" || echo "OK"
grep -rn "'/tmp/" crates/ && echo "FAIL: Found /tmp hardcoding" || echo "OK"
```

## File Paths

- Use `std::path::Path` and `PathBuf` for all file operations (not string concatenation).
- Use `path.join()` for path construction (handles separators cross-platform).
- Never hardcode `/` or `\` as path separators.

## Environment Variables

- Check env vars with `std::env::var()`, not by reading `/proc` or shell config files.
- For test isolation, set env vars per-command with `cmd.env("KEY", "value")` rather than `std::env::set_var()` which is global and causes race conditions in parallel tests.

## Line Endings

- Rust's `fs::read_to_string()` returns platform-native line endings.
- When comparing file content in tests, avoid hardcoding `\n`. Use `.contains()` or `.lines()` for line-by-line comparison.
- The `.gitattributes` file should enforce consistent line endings for source files.
