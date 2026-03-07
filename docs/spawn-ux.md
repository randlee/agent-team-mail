# atm spawn — Interactive UX Design

**Status**: Design complete, prototype validated (2026-03-06)
**Prototype**: `scripts/spawn-demo.sh` (on `develop`)
**Sprint**: AC.3

---

## Overview

`atm spawn` currently requires the user to know all parameters upfront and pass them as flags. This is error-prone when spawning new agents. The UX improvement adds:

1. **Interactive review-panel mode** (human use): numbered list, inline editing, dry-run
2. **Multiple pane-placement modes**: new-pane, existing-pane, current-pane
3. **Non-interactive mode** (agent use): unchanged, all params as flags
4. **`--dry-run` flag**: shows the tmux command + launch command without executing

---

## Interactive Mode

Triggered when stdin is a tty and no `--yes` flag is passed. Shows a review panel:

```
  atm spawn — interactive mode
  Spawning agent type: codex

  tmux context: session=work window=2 (dev)

  ──────────────────────────────────────────────
   1. team:         atm-dev
   2. member:       arch-ctm
   3. model:        codex-5.3-high
   4. agent-type:   general-purpose
   5. pane-mode:    new-pane
   6. worktree:     (none)
  ──────────────────────────────────────────────

  Enter to confirm  ·  Esc or q to cancel
  Change items: e.g. 1=atm-dev, 3=codex-5.3-fast, 2=scrum-master,3=claude-haiku-4-5

  >
```

The user types `n=value` pairs separated by commas to update fields, then presses Enter to confirm or Esc/q to cancel.

**Validation**: Each field validates on edit. Errors are shown inline with red marker and valid options listed below the panel. Confirmation is blocked until all errors are resolved.

---

## Pane Modes

| Mode | Description |
|------|-------------|
| `new-pane` | `tmux split-window -h` — creates a new pane in the current window |
| `existing-pane` | User picks from a numbered list of panes in the current window |
| `current-pane` | Sends launch command to the current pane (replaces current shell) |

### Pane selection (existing-pane)

When `pane-mode=existing-pane`, the panel shows available panes:

```
  Available panes in current window:
    pane 0: 'vim' cmd=nvim pid=12345
    pane 1: 'shell' cmd=zsh pid=12346
    pane 2: 'logs' cmd=tail pid=12347

  Enter pane number: 1
```

After selection, confirmation proceeds as normal.

### tmux targeting

All tmux operations target `{session}:{window}.{pane}` format:

```bash
# new-pane
tmux split-window -h -t {session}:{window}

# existing-pane
tmux send-keys -t {session}:{window}.{pane} "cd {worktree} && atm-spawn-cmd" Enter

# current-pane
tmux send-keys -t {session}:{window}.{current} "cd {worktree} && atm-spawn-cmd" Enter
```

---

## Dry-Run Output

`atm spawn codex --dry-run` (or `5=new-pane` + Enter with `--dry-run` flag):

```
  [dry-run] What would happen:

  1. tmux split-window -h  (create new pane in current window)
  2. In new pane, run:

     claude --agent-id arch-ctm@atm-dev --agent-name arch-ctm --team-name atm-dev

  3. Register arch-ctm in team atm-dev config
     model: codex-5.3-high, agent-type: general-purpose

  No changes made (dry-run).
```

---

## Non-Interactive Mode (unchanged)

When stdin is not a tty, or `--yes` is passed, spawn proceeds immediately:

```bash
atm spawn codex --team atm-dev --member arch-ctm --model codex-5.3-high
```

This is the agent-safe path — no blocking for input.

---

## Terminal Guard

Interactive mode opens `/dev/tty` directly for stdin to support pipes:

```rust
if !stdin_is_tty() {
    eprintln!("error: interactive mode requires a terminal (stdin is not a tty)");
    eprintln!("hint:  use 'atm spawn codex --team atm-dev --member arch-ctm' for non-interactive spawn");
    std::process::exit(1);
}
```

---

## CLI Flags

```
atm spawn [agent-type] [options]

Options:
  --team <name>         Team to spawn into (default: from .atm.toml)
  --member <name>       Member name (default: agent-type)
  --model <name>        Model to use
  --agent-type <type>   Agent type preset
  --pane-mode <mode>    new-pane | existing-pane | current-pane (default: new-pane)
  --worktree <path>     Working directory for the agent
  --dry-run             Show what would happen without executing
  --yes                 Skip interactive review, execute immediately
  --tmux-help           Print tmux targeting reference guide
```

---

## Implementation Plan (AC.3 Sprint)

### Rust implementation notes

The prototype (`scripts/spawn-demo.sh`) validates the UX. The Rust implementation should:

1. Use `crossterm` (already in `atm-tui` deps) for terminal detection and raw input
2. Implement the review panel as a simple line-based renderer (not full TUI — keeps it simple)
3. Reuse `clap` for flag parsing
4. The `spawn` command lives in `crates/atm/src/commands/spawn.rs`
5. tmux interaction via `std::process::Command` calling `tmux` binary

### Files to create/modify

| File | Change |
|------|--------|
| `crates/atm/src/commands/spawn.rs` | New file — full spawn command implementation |
| `crates/atm/src/main.rs` | Wire spawn subcommand |
| `crates/atm-core/src/spawn.rs` | Shared spawn logic (pane modes, validation) |
| `crates/atm/Cargo.toml` | Add `crossterm` dependency if not present |

### Test coverage needed

- `test_spawn_non_interactive_with_yes_flag`
- `test_spawn_dry_run_output`
- `test_spawn_tty_guard_rejects_non_tty_stdin`
- `test_spawn_pane_mode_validation`
- `test_spawn_apply_edits_parses_comma_separated`

---

## Prototype Reference

The shell prototype at `scripts/spawn-demo.sh` (on `develop`, commit e8f8cf0) demonstrates the complete UX including:
- Review panel rendering with ANSI colors
- `n=value,m=value2` edit syntax
- Per-field validation with valid-options hints
- `--dry-run` output format
- `--tmux-help` reference guide
- Esc/q cancellation
- Running-member warning

The Rust implementation should match this UX exactly.
