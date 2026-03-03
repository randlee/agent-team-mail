# ATM Quickstart

## Prerequisites

- `atm` CLI installed and on `PATH`
- Claude Code hooks enabled in your environment
- Repo checkout with write access

## One-Command Setup

Run from your repo root:

```bash
atm init <team>
```

What it does (idempotent):
- creates `.atm.toml` when missing (`[core].default_team`, `[core].identity`)
- creates `~/.claude/teams/<team>/config.json` when missing
- installs ATM hook wiring globally in `~/.claude/settings.json`

Common options:

```bash
atm init <team> --local
atm init <team> --identity <name>
atm init <team> --skip-team
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
