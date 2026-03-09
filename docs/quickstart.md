# ATM Quickstart

## Prerequisites

- `atm` CLI installed and on `PATH`
- Repo checkout with write access

## Install

Homebrew:

```bash
brew tap randlee/tap
brew install agent-team-mail
```

Cargo:

```bash
cargo install agent-team-mail --locked
```

## One-Command Setup

Run from your repo root:

```bash
atm init <team>
```

What it does (idempotent):
- creates `.atm.toml` when missing (`[core].default_team`, `[core].identity`)
- creates `~/.claude/teams/<team>/config.json` when missing
- installs ATM hook wiring globally in `~/.claude/settings.json`

Then bind your session (required once per Claude Code session):

```bash
atm teams resume <team>
```

Common options:

```bash
atm init <team> --local          # project-scoped hooks instead of global
atm init <team> --identity <name>
atm init <team> --skip-team      # skip team creation (join existing)
```

## First Send/Read Flow

From a configured teammate shell/session:

```bash
atm send <teammate> "hello"
atm read --team <team> --timeout 60
```

## Global vs Local Hooks (Worktree Rationale)

Default `atm init` installs hooks globally so every worktree/session gets the
same ATM behavior without per-worktree duplication.

Use `--local` only when you explicitly need project-scoped hook settings in
`.claude/settings.json`.

## Team Protocol

ATM team communication must follow the mandatory protocol in:

- `docs/team-protocol.md`

## Upgrading

Homebrew upgrade:

```bash
brew upgrade randlee/tap/agent-team-mail
```

The formula post-install step terminates any stale `atm-daemon` process
automatically (`pkill -x atm-daemon || true`), so the next `atm` command starts
the upgraded daemon binary.

Cargo/manual binary upgrade:

```bash
pkill -x atm-daemon || true
```

Run the command after installing new binaries so a stale daemon process does
not keep serving the old version. The daemon auto-starts on the next daemon-
backed `atm` invocation.

Note: there is currently no dedicated `atm daemon stop` command; use the
`pkill` approach above for explicit manual restart during upgrades.

### Update Hook Scripts After Upgrading

ATM hook scripts (session-start, session-end, teammate-idle, etc.) are embedded in
the `atm` binary at compile time. When you upgrade `atm`, the on-disk hook scripts
in `~/.claude/scripts/` are stale until refreshed. Always re-run `atm init` after
upgrading:

```bash
atm init <team>
```

This is idempotent — it overwrites only the ATM-managed hook scripts and leaves
all other settings intact. Failure to re-run `atm init` after an upgrade may result
in outdated hook behavior (for example incorrect PID reporting or missing lifecycle events).

## sc-compose: Structured Compose Logging

`sc-compose` is the shared composition library used by ATM tools for templated
output generation. It ships as part of the `agent-team-mail` workspace and is
available after the standard install steps above.

### Install

`sc-compose` is included in the `agent-team-mail` Cargo workspace. No separate
install step is required — it is available once `agent-team-mail` is installed.

If using `sc-compose` as a library dependency in your own crate:

```toml
[dependencies]
sc-compose = { version = "0.42", registry = "crates-io" }
```

### Validate, Render, and Write Workflow

```bash
# Validate a template (dry-run, no output written)
atm compose validate <template.md.j2>

# Render a template to stdout
atm compose render <template.md.j2> --vars key=value

# Render and write output to a file
atm compose write <template.md.j2> --output <out.md> --vars key=value
```

All three commands accept `--vars key=value` pairs (repeatable) or a JSON vars
file via `--vars-file <path>`.

### Environment Variables

| Variable | Default | Description |
|---|---|---|
| `SC_COMPOSE_LOG_LEVEL` | `info` | Log verbosity: `trace`, `debug`, `info`, `warn`, `error` |
| `SC_COMPOSE_LOG_FORMAT` | `jsonl` | Output format for log lines: `jsonl` or `human` |
| `SC_COMPOSE_LOG_FILE` | _(stderr)_ | Redirect sc-compose logs to a file path |

To disable sc-compose structured log emission entirely:

```bash
SC_COMPOSE_LOG_LEVEL=off atm compose render <template>
```
