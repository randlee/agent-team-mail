# Claude Instructions for agent-team-mail

## ⚠️ CRITICAL: Branch Management Rules

**NEVER switch the main repository branch on disk from `develop`.**

- Main repo at `/Users/randlee/Documents/github/agent-team-mail/` MUST remain on `develop` at all times
- **ALWAYS use `sc-git-worktree` skill** to create worktrees for all development work
- **ALWAYS create worktrees FROM `develop` branch** (not from `main`)
- Do NOT use `git checkout` or `git switch` in the main repository
- All sprint work happens in worktrees at `../agent-team-mail-worktrees/<branch-name>`
- **All PRs target `develop` branch** (integration branch, not `main`)

**Why**: Switching branches in the main repo breaks worktree references and destabilizes the development environment.

**Worktree Creation Pattern**:
```bash
# ✅ CORRECT: Create worktree from develop
/sc-git-worktree --create feature/1-2a-work-bead develop

# ❌ WRONG: Creating from main
/sc-git-worktree --create feature/1-2a-work-bead main
```

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

- 5 phases, 18 sprints (Phase 6 open-ended for additional plugins)
- Parallel sprint tracks identified per phase
- Agent team execution: Scrum Master → Dev(s) + QA(s), Opus Architect on escalation
- All work on dedicated worktrees via `sc-git-worktree`

**Current Status**: Pre-development — requirements and plan under review

---

## Key Documentation

**Primary references — read as needed:**

- [`docs/requirements.md`](./docs/requirements.md) - System requirements, architecture, plugin design
- [`docs/project-plan.md`](./docs/project-plan.md) - Phased sprint plan with dependency graphs
- [`docs/agent-team-api.md`](./docs/agent-team-api.md) - Claude agent team API reference (schema baseline: Claude Code 2.1.39)

**Rust development reference — read only when implementation decisions are needed:**

- [`.claude/skills/rust-development/guidelines.txt`](./.claude/skills/rust-development/guidelines.txt) - Pragmatic Rust Guidelines (Microsoft)

---

## Workflow

### Sprint Execution Pattern (Dev-QA Loop)

Every sprint follows this pattern:

1. **Create worktree** using `sc-git-worktree` skill
2. **Dev work** by assigned dev agent(s)
3. **QA validation** by assigned QA agent(s)
4. **Retry loop** if QA fails (max attempts configurable)
5. **Commit/Push/PR** to `develop` branch
6. **Agent-teams review** documenting what worked/didn't

### Integration Branch

- All PRs target `develop` branch
- Merge to `main` after review/approval
- Post-merge CI runs as safety net

---

## Agent Model Selection

- **Haiku** - Exploration, test execution, simple validation
- **Sonnet** - Implementation work, documentation writing
- **Opus** - Critical planning, architecture decisions, complex review

---

## Environment

**Task List**: `agent-team-mail` (configured in `.env`)
**Agent Teams**: Enabled (experimental feature)

---

## Initialization Process
- Read project plan (`docs/project-plan.md`)
- Check current status (branches, PRs, worktrees)
- Output concise project summary and status to user
- Identify the next sprint(s) ready to execute
- Be prepared to begin the next sprint upon user approval