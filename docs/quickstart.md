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
