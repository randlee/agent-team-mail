# Agent Instructions for agent-team-mail

## CRITICAL: Branch Management Rules

**NEVER switch the main repository branch on disk from `develop`.**

- Main repo at `/Users/randlee/Documents/github/agent-team-mail/` MUST remain on `develop` at all times
- All sprint work happens in worktrees at `../agent-team-mail-worktrees/<branch-name>`
- **All PRs target `develop` branch** (integration branch, not `main`)

---

## Project Overview

**agent-team-mail** (`atm`) is a Rust CLI and daemon for mail-like messaging with Claude agent teams:
- Thin CLI over `~/.claude/teams/` file-based API (send, read, broadcast, inbox)
- Three-crate workspace: `atm-core` (library), `atm` (CLI), `atm-daemon` (plugin host)
- Atomic file I/O with conflict detection and guaranteed delivery
- Trait-based plugin system in daemon for extensibility (Issues, CI Monitor, Bridge, Chat, Beads, MCP)
- Provider-agnostic (GitHub, Azure DevOps, GitLab, Bitbucket)

**Goal**: Build a well-tested Rust CLI for agent team messaging, with a plugin-ready daemon.

---

## Project Plan

**Current Plan**: [`docs/project-plan.md`](./docs/project-plan.md)

**Current Status**: Phase 9 complete. v0.9.0 on develop. 737+ tests.

---

## Key Documentation

- [`docs/requirements.md`](./docs/requirements.md) - System requirements, architecture, plugin design
- [`docs/project-plan.md`](./docs/project-plan.md) - Phased sprint plan with dependency graphs
- [`docs/agent-team-api.md`](./docs/agent-team-api.md) - Claude agent team API reference
- [`docs/cross-platform-guidelines.md`](./docs/cross-platform-guidelines.md) - Windows CI compliance
- `docs/atm-agent-mcp/codex-subagents.md` - Codex CLI now supports native subagents (`spawn_agent`, `wait`, `send_input`, etc.); read this file for operational details and constraints

---

## Agent Team Mail (ATM) — How to Communicate

You are part of the **atm-dev** team. ATM is the messaging system this project builds — and you use it to communicate with your teammates.

### Your Identity

- **Your name**: `arch-ctm`
- **Your team**: `atm-dev`
- **Environment variables** (already set): `ATM_IDENTITY=arch-ctm`, `ATM_TEAM=atm-dev`

### Team Members

| Mail Alias   | Role            | Notes                        |
|-------------|-----------------|------------------------------|
| `team-lead` | arch-atm (lead) | Team lead, architecture      |
| `arch-ctm`  | architect       | You — Codex agent            |
| `publisher`  | publishing      | Releases, crates.io, Homebrew |

### ATM Commands

**Send a message:**
```bash
atm send <recipient> "your message"
```
Example: `atm send team-lead "review complete, found 2 issues"`

**Read your inbox:**
```bash
atm read
```

**Check team inbox status:**
```bash
atm inbox
```

**List team members:**
```bash
atm members
```

**Broadcast to entire team:**
```bash
atm broadcast "message to everyone"
```

**Show your config:**
```bash
atm config
```

### Communication Protocol

1. Check your inbox periodically with `atm read`
2. When you receive a task, acknowledge it with `atm send team-lead "ack, starting work on X"`
3. Report results back via `atm send team-lead "done: summary of findings"`
4. If you need help, send a message — don't block silently
