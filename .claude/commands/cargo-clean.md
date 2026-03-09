---
name: cargo-clean
description: Run cargo clean on the main repo and all registered git worktrees to free up Rust build artifacts.
version: 1.0.0
options:
  - name: --worktrees-only
    description: Skip the main repo; only clean registered worktrees.
---

Run `cargo clean` on the main repo and all registered git worktrees to free up Rust build artifacts.

## Usage

```
/cargo-clean [--worktrees-only]
```

- No flags: cleans the main repo **and** all worktrees
- `--worktrees-only`: skips the main repo, only cleans worktrees

## Instructions

1. Parse the user's invocation for the `--worktrees-only` flag.

2. Determine the main repo root by running:
   ```bash
   git worktree list | head -1 | awk '{print $1}'
   ```

3. Get all registered worktrees by running:
   ```bash
   git -C <main_repo_path> worktree list
   ```

4. Parse the output into a list of paths. The first entry is always the main repo.

5. Build the target list:
   - If `--worktrees-only`: exclude the first entry (main repo path)
   - Otherwise: include all entries

6. For each path in the target list, check if `<path>/Cargo.toml` exists. If it does, run:
   ```bash
   cargo clean --manifest-path "<path>/Cargo.toml"
   ```
   Print the path being cleaned before each run, and the cargo output (files removed, GiB freed) after.
   Skip paths that don't have a Cargo.toml (non-Rust worktrees).

7. Print a summary table showing each worktree, files removed, and space freed.
   Include a total row summing files and GiB across all cleaned worktrees.
