---
name: phase-orchestration
description: Orchestrate multi-sprint phase execution as team-lead (ARCH-ATM). Manages sprint waves, scrum-master lifecycle, PR merges, arch-ctm reviews, and integration branch strategy. This skill is for the TEAM-LEAD only, not for scrum-masters.
---

# Phase Orchestration

This skill defines how the team-lead (ARCH-ATM) orchestrates a development phase consisting of multiple sprints with dependency-aware parallelism.

**Audience**: Team-lead only. Scrum-masters have their own process defined in `.claude/agents/scrum-master.md`.

## Prerequisites

Before starting a phase:
1. Phase plan document exists (e.g., `docs/phase-{N}-*.md`) with sprint specs
2. Integration branch `integrate/phase-{N}` exists and is up to date with `develop`
3. Claude Code team exists (e.g., `atm-dev`) — do NOT recreate between phases
4. arch-ctm (Codex) is running and reachable via ATM CLI

## Phase Execution Loop

### 1. Build Sprint Dependency Graph

Read the phase plan and identify:
- Sprint dependencies (which sprints block others)
- Parallel waves (groups of sprints that can run concurrently)
- Merge order within each wave (to minimize conflicts on shared files)

### 2. Execute Sprints

For each sprint (respecting dependency order):

#### a. Spawn a Fresh Scrum-Master

Each sprint gets a **fresh** scrum-master — do NOT reuse scrum-masters across sprints.

```json
{
  "subagent_type": "scrum-master",
  "name": "sm-{phase}-{sprint}",
  "team_name": "<team-name>",
  "model": "sonnet",
  "prompt": "<sprint prompt — see template below>"
}
```

**Critical rules:**
- `subagent_type` MUST be `"scrum-master"` — it has built-in dev-QA loop orchestration
- `name` parameter IS required — scrum-masters are full tmux teammates that CAN spawn background sub-agents
- `team_name` IS required — they need team membership for SendMessage
- The scrum-master is a **COORDINATOR ONLY** — it spawns `rust-developer` and `rust-qa-agent` as background agents
- The scrum-master MUST NOT write code, run tests, or implement fixes itself
- If a scrum-master is found doing dev work, it is a bug in the orchestration

#### b. Sprint Prompt Template

```
You are the scrum-master for Phase {P}, Sprint {P}.{S}: {Title}.

PHASE PLAN: Read docs/{plan-file} for full context.
SPRINT SECTION: "Sprint {P}.{S}: {Title}" in the plan document.
REQUIREMENTS: Read docs/{requirements-file} for FRs and acceptance criteria.

WORKTREE:
- Create: git -C /Users/randlee/Documents/github/agent-team-mail worktree add /Users/randlee/Documents/github/agent-team-mail-worktrees/feature/{P}-{S}-{slug} integrate/phase-{P}
- Work in: /Users/randlee/Documents/github/agent-team-mail-worktrees/feature/{P}-{S}-{slug}
- Branch: git checkout -b feature/{P}-{S}-{slug}

PR targets: integrate/phase-{P}

REMINDER: You are a COORDINATOR. You spawn rust-developer and rust-qa-agent as
background agents. You do NOT write code, run cargo test, or implement fixes yourself.
Follow your standard dev-QA loop process (defined in your agent prompt).
When complete, send message to team-lead with PR number and summary.
```

#### c. Monitor Progress

- Scrum-masters report via SendMessage when done
- If a scrum-master reports subagent spawn failure, investigate and advise — do NOT tell it to do dev work itself
- If a scrum-master escalates, spawn a rust-architect (opus) for analysis and send findings back to scrum-master

### 3. Post-Sprint: CI Gate + Merge

After each scrum-master reports completion:

1. **Verify QA passed** — scrum-master should confirm QA agent gave PASS verdict
2. **Wait for CI** — poll PR checks until green (use delay-poll agent)
3. **Merge PR** to `integrate/phase-{N}` in dependency order
4. **Update integration branch** — pull latest into worktree
5. **Mark task completed** — TaskUpdate status to completed

### 4. Post-Sprint: Arch-CTM Critical Design Review

**After EVERY sprint PR is merged to `integrate/phase-{N}`**, request arch-ctm review:

1. Send arch-ctm the diff via ATM CLI:
   ```
   atm send arch-ctm "Sprint {P}.{S} merged (PR #{N}). Critical design review requested. Review: gh pr diff {N} --repo randlee/agent-team-mail. Focus: correctness bugs, architectural violations, missing edge cases."
   ```
2. Start the next eligible sprint immediately (dependency permitting) — do NOT wait for arch-ctm review before continuing development
3. Run arch-ctm review in parallel (use delay agent, nudge via tmux if no reply in 2 min)
4. Track arch-ctm findings:
   - **No issues**: Continue to next sprint
   - **Issues found**: Create/update a **parallel arch-ctm fix track** in a separate worktree (`feature/{P}-fixes-arch-review`) to address findings while later sprint waves continue
5. arch-ctm is authorized to implement fixes directly in the fix worktree for review findings
6. Every arch-ctm fix PR MUST be validated by QA agents (`rust-qa-agent` and `atm-qa-agent`) before merge
7. Do NOT block ongoing sprint execution unless arch-ctm marks findings as critical/blocking

### 5. Arch-CTM Fix Sprint (if needed)

If arch-ctm found issues across sprints:
1. Create a new worktree branched from `integrate/phase-{N}` (after all sprint PRs merged)
2. arch-ctm may execute fixes directly OR team-lead may delegate to a fresh scrum-master
3. Regardless of who implements fixes, run QA validation (`rust-qa-agent` + `atm-qa-agent`) before merge
4. Follow normal CI loop and merge fix PR to integration branch
5. Request arch-ctm re-review of fixes if delegated implementation was used

### 6. Wave Transitions (for parallel sprints)

Before starting the next wave:
1. All prerequisite sprints from previous wave must be merged
2. Integration branch must be updated (`git pull` in worktree)
3. Any arch-ctm critical/blocking findings must be addressed first
4. New scrum-masters get fresh worktrees branched from updated `integrate/phase-{N}`

### 7. Phase Completion

After all sprints (including fix sprint if needed) merge to `integrate/phase-{N}`:
1. Create PR: `integrate/phase-{N} → develop`
2. Wait for CI green
3. Merge after user approval
4. **Minor version bump** on `develop` (separate release prep commit)
5. Deploy the `atm-publisher` teammate (named teammate) to orchestrate package publishing
6. `atm-publisher` creates `release/vX.Y.Z` from `develop`, performs the minor version bump, runs validation, and publishes from the release branch
7. Create PR: `develop → main` for release branch-forwarding
8. Wait for CI green on `main` PR and merge after user approval
9. Shutdown all remaining scrum-master panes
10. Do NOT clean up worktrees until user reviews them

## Scrum-Master Lifecycle

- **Fresh per sprint** — each sprint gets a new scrum-master instance
- **Named tmux teammate** — spawned with `name` parameter for full CLI process
- **Can spawn sub-agents** — background rust-developer and rust-qa-agent (no `name` param)
- **Shutdown after sprint** — send shutdown_request after PR merges and CI passes
- **NEVER does dev work** — if you see a scrum-master writing code, the prompt is wrong

## Team Lifecycle

- **Team persists across phases** — NEVER use TeamDelete on persistent teams
- **Scrum-masters are ephemeral** — shutdown after their sprint completes
- **arch-ctm is persistent** — communicates exclusively via ATM CLI (not Claude Code SendMessage)
- Between sprints: team stays alive, only scrum-master panes come and go

## ATM CLI Communication (arch-ctm)

arch-ctm is a Codex agent that does NOT receive Claude Code team messages. Use ATM CLI only:

```bash
atm send arch-ctm "message"     # Send
atm read                         # Check replies
atm inbox                        # Summary
```

Nudge via tmux if no reply:
```bash
tmux send-keys -t <pane-id> -l "You have unread ATM messages. Run: atm read --team atm-dev" && sleep 0.5 && tmux send-keys -t <pane-id> Enter
```

## Task Tracking

Create one task per sprint at phase start:
- Set dependencies via addBlockedBy
- Assign owner when scrum-master starts
- Mark completed when PR merges

## Anti-Patterns

- Do NOT use `rust-developer` as scrum-master subagent_type — use `scrum-master`
- Do NOT tell scrum-masters to "do the work yourself" — they are coordinators
- Do NOT do dev or QA work as team-lead — delegate to scrum-masters
- Do NOT skip post-merge arch-ctm sprint review — every merged sprint requires it
- Do NOT merge arch-ctm fix PRs without QA validation from both QA agents
- Do NOT merge without QA pass + CI green
- Do NOT publish packages directly as team-lead — delegate publishing orchestration to `atm-publisher`
- Do NOT delete the team between sprints or phases
- Do NOT clean up worktrees without user approval
- Do NOT reuse scrum-masters across sprints — each sprint gets a fresh instance
