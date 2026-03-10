---
name: team-lead-startup
description: >
  Session initialization for the team-lead identity. Restores the atm-dev team,
  inbox polling, and Claude Code task list after a new session or context compaction.
  Only run when ATM_IDENTITY=team-lead.
---

# Team Lead Startup Skill

**Trigger**: Run this skill at the start of every session where `ATM_IDENTITY=team-lead`.

---

## Step 0 — Confirm Identity

```bash
echo "ATM_IDENTITY=$ATM_IDENTITY"
```

Stop if `ATM_IDENTITY` is not `team-lead`. This skill is for the team-lead only.

> **TODO**: Add identity conflict detection — verify no other active session is already
> running as `team-lead` for this team before proceeding with restore.

---

## Step 1 — Detect Whether Restore Is Needed

Get the current session ID from the SessionStart hook output at the top of context
(format: `SESSION_ID=<uuid>`). Then compare with `leadSessionId` in the team config:

```bash
cat ~/.claude/teams/atm-dev/config.json | python3 -c \
  "import json,sys; print(json.load(sys.stdin)['leadSessionId'])"
```

- **Match** → team is already initialized for this session. Skip to Step 7.
- **Mismatch** (or config missing) → proceed with full restore sequence below.

---

## Step 2 — Backup Current State

Always backup before modifying the team:

```bash
atm teams backup atm-dev
# Note the backup path from output, e.g.:
# Backup created: ~/.claude/teams/.backups/atm-dev/<timestamp>
```

Also backup the Claude Code project task list (separate bucket — see note below):

```bash
BACKUP_PATH=$(ls -td ~/.claude/teams/.backups/atm-dev/*/ | head -1)
cp -r ~/.claude/tasks/agent-team-mail/ "$BACKUP_PATH/tasks-cc"
echo "CC task list backed up to $BACKUP_PATH/tasks-cc"
```

> **Note**: `atm teams backup` captures `~/.claude/tasks/atm-dev/` (ATM sprint tasks)
> but NOT `~/.claude/tasks/agent-team-mail/` (Claude Code TaskCreate/TaskList tasks).
> These are two separate buckets — issue #650 tracks fixing this in the CLI.

---

## Step 3 — Clear Stale Team State

TeamDelete requires an active team context. In a fresh session it may report
"No team name found" — that is expected. The important step is removing the
stale directory:

```bash
# 1. Remove the randomly-named team if TeamCreate already ran this session
TeamDelete  # (tool call — may say "No team name found", that is OK)

# 2. Remove the stale atm-dev directory so TeamCreate uses the correct name
rm -rf ~/.claude/teams/atm-dev
```

> **Warning**: If `TeamDelete` reports it cleaned up a team with the CORRECT name
> (`atm-dev`), do NOT `rm -rf` — the directory is already gone.

---

## Step 4 — Create Team

```
TeamCreate(team_name="atm-dev", description="ATM development team", agent_type="team-lead")
```

**Verify the result**: `team_name` in the response MUST be `"atm-dev"`.
If it is any other name, stop immediately — do not proceed.

---

## Step 5 — Restore Team Members and Inboxes

Use the backup created in Step 2 (or a known-good manual backup):

```bash
atm teams restore atm-dev --from ~/.claude/teams/.backups/atm-dev/<timestamp>
# Expected output: N member(s) added, N inbox file(s) restored
```

**After restore**, verify the member list and remove any ghost/test members:

```bash
atm members
```

Remove unexpected members by editing `~/.claude/teams/atm-dev/config.json` directly
(no CLI `remove-member` command exists yet — issue #649 tracks adding it):

```python
python3 -c "
import json
path = '/Users/randlee/.claude/teams/atm-dev/config.json'
with open(path) as f: cfg = json.load(f)
cfg['members'] = [m for m in cfg['members'] if m['name'] not in ['test-member-1', 'test-member-3']]
with open(path, 'w') as f: json.dump(cfg, f, indent=2)
print('Members:', [m['name'] for m in cfg['members']])
"
```

---

## Step 6 — Restore Claude Code Task List

The Claude Code task list lives at `~/.claude/tasks/agent-team-mail/` and is NOT
restored by `atm teams restore`. Restore it manually from the backup:

```bash
BACKUP_PATH=$(ls -td ~/.claude/teams/.backups/atm-dev/*/ | head -1)
if [ -d "$BACKUP_PATH/tasks-cc" ]; then
  cp "$BACKUP_PATH/tasks-cc/"*.json ~/.claude/tasks/agent-team-mail/ 2>/dev/null || true
  MAX_ID=$(ls ~/.claude/tasks/agent-team-mail/*.json 2>/dev/null \
    | xargs -I{} basename {} .json \
    | sort -n | tail -1)
  [ -n "$MAX_ID" ] && echo -n "$MAX_ID" > ~/.claude/tasks/agent-team-mail/.highwatermark
  echo "Task list restored. Highwatermark: $MAX_ID"
else
  echo "No tasks-cc/ in backup — task list not restored."
fi
```

> **Note**: Even with correct files and highwatermark, the Claude Code UI task panel
> will not show restored tasks until one task is created via the `TaskCreate` tool.
> Create a real task (e.g., next pending sprint) to trigger the panel refresh.

> **Known bug**: `atm teams restore` sets `.highwatermark` to `min_id - 1` instead
> of `max_id` — issue #651 tracks this fix.

---

## Step 7 — Verify Team Health

```bash
atm members          # confirm expected members present
atm inbox            # check for unread messages from teammates
atm gh pr list       # review open PRs and CI status
```

---

## Step 8 — Read Project Context

1. Read `docs/project-plan.md` — focus on current phase and open tasks
2. Check `TaskList` — restore pending tasks via `TaskCreate` if list is empty
3. Output a concise project summary to the user:
   - Current phase and status
   - Open PRs
   - Active teammates and their last known task
   - Next sprint(s) ready to execute

---

## Step 9 — Notify Teammates

Send each active teammate a session-restored notification via ATM:

```bash
atm send arch-ctm "New session started (session-id: <SESSION_ID>). Team atm-dev restored. Please acknowledge and confirm your current status."
```

If a teammate does not respond within ~60s, nudge via tmux:

```bash
tmux list-panes -a -F '#{session_name}:#{window_index}.#{pane_index} #{pane_title}'
tmux send-keys -t <pane-id> "You have unread ATM messages. Run: atm read --team atm-dev" Enter
```

---

## Quick Reference — Common Failure Modes

| Symptom | Cause | Fix |
|---------|-------|-----|
| `TeamCreate` returns random team name | `~/.claude/teams/atm-dev` still exists | `rm -rf ~/.claude/teams/atm-dev` then retry |
| `TeamDelete` says "No team name found" | Fresh session, no active team context | Expected — proceed anyway |
| `TaskList` returns empty after restore | Highwatermark < max task ID | Set hwm manually: `echo -n "<max_id>" > ~/.claude/tasks/agent-team-mail/.highwatermark` then create one task via `TaskCreate` |
| `atm send` fails with "Agent not found" | Member missing from config after restore overwrite | Re-add with `atm teams add-member` |
| Self-send message (team-lead → team-lead) | Teammate using wrong `ATM_IDENTITY` | Set `ATM_IDENTITY=<correct-name>` in their tmux pane or relaunch with correct env |
