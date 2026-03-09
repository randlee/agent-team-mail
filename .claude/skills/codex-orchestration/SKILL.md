---
name: codex-orchestration
description: Orchestrate multi-sprint phases where arch-ctm (Codex) is the sole developer, with pipelined QA via quality-mgr teammate. Team-lead tracks findings and schedules fix passes.
---

# Codex Orchestration

This skill defines how the team-lead (ARCH-ATM) orchestrates phases where **arch-ctm (Codex)** is the sole developer, executing sprints sequentially while QA runs in parallel via a dedicated **quality-mgr** teammate.

**Audience**: Team-lead only.

**When to use**: When a phase's implementation is done entirely by arch-ctm (a Codex agent communicating via ATM CLI), not by Claude Code scrum-masters. This pattern was proven in Phase M (8 sprints) and Phase O.

## Prerequisites

Before starting a phase:
1. Phase plan document exists with sprint specs and dependencies
2. Integration branch `integrate/phase-{P}` created off `develop`
3. ATM team (`atm-dev`) is active with team-lead and arch-ctm as members
4. arch-ctm is running and reachable via ATM CLI (`atm send arch-ctm "ping"`)

## Architecture

```
team-lead (ARCH-ATM)
  ├── arch-ctm (Codex) ──── sole developer, sequential sprints
  │     communicates via ATM CLI only
  └── quality-mgr (Claude Code) ──── QA coordinator teammate
        spawns rust-qa-agent + atm-qa-agent as background agents
```

Key principle: **arch-ctm does NOT wait for QA**. He proceeds to the next sprint as soon as he completes one, unless there are outstanding fix requests from earlier sprints.

## Phase Setup

### 1. Create Integration Branch

```bash
git fetch origin develop
git branch integrate/phase-{P} origin/develop
git push -u origin integrate/phase-{P}
```

### 2. Create First Sprint Worktree

```bash
# Use sc-git-worktree skill
/sc-git-worktree --create feature/p{P}-s1-{slug} integrate/phase-{P}
```

### 3. Spawn Quality Manager

Spawn once per phase. The quality-mgr persists across all sprints.

Use the Task tool with `name` parameter to spawn as a tmux teammate:

```json
{
  "subagent_type": "general-purpose",
  "name": "quality-mgr",
  "team_name": "atm-dev",
  "model": "sonnet",
  "prompt": "You are quality-mgr for Phase {P}. You will receive QA assignments from team-lead for each sprint as they complete. Stand by for first assignment. Integration branch: integrate/phase-{P}. Phase docs: docs/project-plan.md, docs/atm-agent-mcp/requirements.md."
}
```

**Tmux teammate launch troubleshooting**: If the pane opens but the Claude process doesn't start, manually launch in the pane with all three required flags:

```bash
CLAUDE_CODE_EXPERIMENTAL_AGENT_TEAMS=1 /Users/randlee/.local/share/claude/versions/<VERSION> \
  --agent-id quality-mgr@atm-dev \
  --agent-name quality-mgr \
  --team-name atm-dev
```

All three flags (`--agent-id`, `--agent-name`, `--team-name`) are required together — omitting any one causes an error.

### 4. Send O.1 Assignment to arch-ctm

```bash
atm send arch-ctm "Phase {P} Sprint {P}.1 assignment: {title}

Worktree: /path/to/worktree
Branch: feature/p{P}-s1-{slug}
PR target: integrate/phase-{P}

Deliverables:
- {list deliverables}

Requirements: docs/atm-agent-mcp/requirements.md ({relevant FRs})
Sprint plan: docs/project-plan.md (Phase {P} section)

When complete: commit, push, create PR targeting integrate/phase-{P}, then notify me via atm send."
```

## Sprint Pipeline

### Steady-State Flow

```
Timeline:
  arch-ctm:     [── S.1 ──]──fixes──[── S.2 ──]──fixes──[── S.3 ──]
  quality-mgr:         [── QA S.1 ──]      [── QA S.2 ──]     [── QA S.3 ──]
  team-lead:    assign S.1 → track → assign S.2 → track → assign S.3 → track
```

### When arch-ctm Completes Sprint S

1. **arch-ctm sends completion message** via ATM CLI with PR number
2. **Team-lead creates worktree for S+1** based on sprint S branch:
   ```
   /sc-git-worktree --create feature/p{P}-s{N+1}-{slug} feature/p{P}-s{N}-{slug}
   ```
   All worktrees chain: S+1 bases on S, so later sprints include earlier work.
3. **Team-lead assigns QA to quality-mgr** via SendMessage:
   ```
   "Run QA on Sprint {P}.{S}. Worktree: {path}. Sprint deliverables: {summary}.
    Design docs: {list}. PR: #{N}."
   ```
4. **Team-lead checks for outstanding findings** from earlier sprints:
   - If findings exist for S-2 or S-1: send fix assignment to arch-ctm BEFORE S+1 assignment
   - If no findings: send S+1 assignment immediately
5. **arch-ctm addresses fixes first, then starts S+1**

### When arch-ctm Has Outstanding Findings

Priority order for arch-ctm:
1. Fix findings on oldest sprint first (S-2 before S-1)
2. Merge fixes forward into later sprint worktrees
3. Then proceed to next sprint

Fix workflow:
```bash
# arch-ctm fixes on the sprint's original worktree
# arch-ctm pushes fix commits to same PR branch
# team-lead asks quality-mgr to re-run QA on the fixed worktree
# If QA passes, team-lead merges PR to integration branch
```

### Merge Forward Protocol

After fixes merge to `integrate/phase-{P}`:
- arch-ctm must merge integration branch into any active sprint worktree before continuing:
  ```bash
  git fetch origin
  git merge origin/integrate/phase-{P}
  ```
- This ensures later sprints include all fixes from earlier sprints

## QA Coordination

### Team-lead → quality-mgr Messages

Assignment format:
```
Run QA on Sprint {P}.{S}: {title}
Worktree: {absolute path}
Sprint deliverables: {bullet list}
Design docs: {list of relevant doc paths}
PR: #{number}
```

Re-run after fixes:
```
Re-run QA on Sprint {P}.{S} (post-fix).
Worktree: {path}
Fixed findings: {list of QA IDs addressed}
```

### quality-mgr → team-lead Reports

quality-mgr reports PASS/FAIL with finding IDs. Team-lead tracks:

| Sprint | QA Run | Verdict | Blocking Findings | Status |
|--------|--------|---------|-------------------|--------|
| O.1    | 1      | FAIL    | QA-001, QA-002    | Fixes assigned |
| O.1    | 2      | PASS    | —                 | Merged |
| O.2    | 1      | PASS    | —                 | Merged |

### Finding Lifecycle

```
OPEN → assigned to arch-ctm → FIXED (arch-ctm pushes) → re-QA → VERIFIED (QA passes)
                             → WONTFIX (team-lead approves deviation)
```

## PR and Merge Strategy

- **All PRs target `integrate/phase-{P}`** (never develop directly)
- **Merge order**: Sprint PRs merge in order (S.1 before S.2)
- **Merge gate**: QA pass + CI green
- **Team-lead merges** (not arch-ctm)
- After all sprints merge: one final PR `integrate/phase-{P} → develop`

## ATM Communication Protocol

All arch-ctm communication is via ATM CLI. Follow the dogfooding protocol (ACK → work → complete → ACK).

### Sending assignments
```bash
atm send arch-ctm "message"
```

### Checking for replies
```bash
atm read
```

### Nudging (if no reply in 2+ minutes)
```bash
# Find arch-ctm's pane
tmux list-panes -a -F '#{session_name}:#{window_index}.#{pane_index} #{pane_title} #{pane_current_command}'
# Send nudge
tmux send-keys -t <pane-id> -l "You have unread ATM messages. Run: atm read --team atm-dev" && sleep 0.5 && tmux send-keys -t <pane-id> Enter
```

### Advise arch-ctm to poll with timeout
When arch-ctm is waiting for assignments, tell him:
```
"Standing by? Use: atm read --team atm-dev --timeout 60"
```
This keeps him responsive without busy-polling.

## Phase Completion

After all sprints pass QA and merge to integration branch:
1. Run final integration QA (quality-mgr validates full integration branch)
2. Create PR: `integrate/phase-{P} → develop`
3. Wait for CI green
4. Merge after user approval
5. Shutdown quality-mgr teammate
6. Do NOT clean up worktrees until user reviews

## Anti-Patterns

- Do NOT tell arch-ctm to wait for QA before starting the next sprint
- Do NOT skip QA on any sprint — quality-mgr runs both agents every time
- Do NOT merge PRs without QA pass + CI green
- Do NOT let findings accumulate — schedule fixes before assigning new sprints
- Do NOT create worktrees off `develop` — chain from previous sprint or integration branch
- Do NOT communicate with arch-ctm via SendMessage — use ATM CLI only
- Do NOT reuse quality-mgr across phases — spawn fresh per phase
- Do NOT clean up worktrees without user approval
