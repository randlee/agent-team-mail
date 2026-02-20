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

**Current Status**: Active development on `develop` with ongoing sprint execution via worktrees

---

## Key Documentation

**Primary references — read as needed:**

- [`docs/requirements.md`](./docs/requirements.md) - System requirements, architecture, plugin design
- [`docs/project-plan.md`](./docs/project-plan.md) - Phased sprint plan with dependency graphs
- [`docs/agent-team-api.md`](./docs/agent-team-api.md) - Claude agent team API reference (schema baseline: Claude Code 2.1.39)
- [`docs/cross-platform-guidelines.md`](./docs/cross-platform-guidelines.md) - Mandatory Windows CI compliance patterns

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
5. **Commit/Push/PR** to phase integration branch
6. **Agent-teams review** documenting what worked/didn't

### Worktree Cleanup Policy

**Do NOT clean up worktrees until the user has reviewed them.** The user reviews each sprint's worktree separately to check for design divergence before approving cleanup. Worktree cleanup is only performed when explicitly requested.

### Branch Flow

- Sprint PRs → `develop` (integration branch)
- Release PR → `main` (after user review/approval)
- Post-merge CI runs as safety net at each level

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

## Agent Team Mail (ATM) Communication

### Team Configuration

- **Team source of truth**: repo `.atm.toml` `[core].default_team`
- **ARCH-ATM** (you) is `team-lead` — start and maintain the configured team for the session duration
- **ARCH-CTM** is a Codex agent — communicates **exclusively** via ATM CLI messages (not Claude Code team API)
- **All other Claude agents** communicate using Claude Code's built-in team messaging API (`SendMessage` tool)

### Mandatory Team Bootstrap (Resume-First)

Before any teammate spawn, Claude MUST follow this order:

1. Read required team from repo `.atm.toml` `[core].default_team`.
2. Run `atm teams resume <team-from-toml>`.
3. Call `TeamCreate(team_name="<team-from-.atm.toml>", ...)`.
4. Run `atm teams resume <team-from-toml>` again.
5. If needed, call `TeamCreate(team_name="<team-from-.atm.toml>", ...)` one more time.

Behavior requirements:
- `TeamCreate` must always use the exact team name from `.atm.toml` `[core].default_team`.
- If `.atm.toml` is missing, `default_team` is unset/empty, or the resolved name does not match the name being used for `TeamCreate`: tell the user and stop.
- Use `atm teams resume` as the primary fix before treating team-name drift as a hard failure.
- If the sequence above still fails after the second `TeamCreate`, escalate to the user.
- If a non-lead agent attempts this flow and gets an authorization error, stop and escalate to `team-lead` instead of creating a new team name.

Required `TeamCreate` syntax:
```text
TeamCreate(team_name="<team-from-.atm.toml>", description="<short description>")
```

Never invent or auto-generate a different team name when `.atm.toml` defines `default_team`.

### Identity

`.atm.toml` at repo root sets `identity` and `default_team`; these are the required defaults for CLI and team bootstrap behavior.

**Note**: ARCH-CTM gets his identity from `ATM_IDENTITY=arch-ctm` set in his tmux session via `launch-worker.sh`.

### Communicating with ARCH-CTM (Codex)

ARCH-CTM does **not** monitor Claude Code messages. Use ATM CLI only:

**Send a message:**
```bash
atm send arch-ctm "your message here"
```

**Check your inbox for replies:**
```bash
atm read
```

**Check team inbox summary (who has unread messages):**
```bash
atm inbox
```

**Nudge ARCH-CTM to check inbox** (when he hasn't replied):

ARCH-CTM runs in a tmux pane. Discover the pane, then send-keys:
```bash
# Find arch-ctm's pane
tmux list-panes -a -F '#{session_name}:#{window_index}.#{pane_index} #{pane_title} #{pane_current_command}'

# Send nudge (use the correct pane ID from above)
tmux send-keys -t <pane-id> -l "You have unread ATM messages. Run: atm read --team <team-from-.atm.toml>" && sleep 0.5 && tmux send-keys -t <pane-id> Enter
```

### Communication Rules

1. **No broadcast messages** — all communications are direct (team-lead ↔ specific agent)
2. **Poll for replies** — after sending to arch-ctm, wait 30-60s then `atm read`. If no reply after 2 minutes, nudge via tmux send-keys
3. **arch-ctm is async** — he processes messages on his next turn. Do not block waiting; continue other work and check back

### ATM CLI Quick Reference

| Action | Command |
|--------|---------|
| Send message | `atm send <agent> "msg"` |
| Read inbox | `atm read` |
| Inbox summary | `atm inbox` |
| List teams | `atm teams` |
| Team members | `atm members` |

---

## Initialization Process
- Resolve team from `.atm.toml`, then run: `resume → TeamCreate → resume → TeamCreate` using the same team name
- Read project plan (`docs/project-plan.md`)
- Check current status (branches, PRs, worktrees)
- Output concise project summary and status to user
- Identify the next sprint(s) ready to execute
- Be prepared to begin the next sprint upon user approval
