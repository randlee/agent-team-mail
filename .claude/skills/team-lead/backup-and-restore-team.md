# Team Backup and Restore Procedure

Follow this procedure when Step 1 of the team-lead skill detects a session ID
mismatch (i.e., a full team restore is required).

---

## Step 2 — Backup Current State

Always backup before modifying the team:

```bash
atm teams backup atm-dev
# Note the backup path from output, e.g.:
# Backup created: ~/.claude/teams/.backups/atm-dev/<timestamp>
```

Also backup the Claude Code project task list (separate bucket):

```bash
BACKUP_PATH=$(ls -td ~/.claude/teams/.backups/atm-dev/*/ | head -1)
cp -r ~/.claude/tasks/agent-team-mail/ "$BACKUP_PATH/tasks-cc"
echo "CC task list backed up to $BACKUP_PATH/tasks-cc"
```

> **Note**: `atm teams backup` captures `~/.claude/tasks/atm-dev/` (ATM sprint
> tasks) but NOT `~/.claude/tasks/agent-team-mail/` (Claude Code task tools).
> These are two separate buckets — issue #650 tracks fixing this in the CLI.

---

## Step 3 — Clear Stale Team State

```bash
# 1. Clear any active team context in this session
TeamDelete  # tool call — may say "No team name found", that is OK

# 2. Remove the stale atm-dev directory so TeamCreate uses the correct name
rm -rf ~/.claude/teams/atm-dev
```

> **Warning**: If `TeamDelete` reports it cleaned up a team named `atm-dev`,
> do NOT `rm -rf` — the directory is already gone.

---

## Step 4 — Create Team

```
TeamCreate(team_name="atm-dev", description="ATM development team", agent_type="team-lead")
```

**Verify**: `team_name` in the response MUST be `"atm-dev"`.
If it is any other name, **stop immediately** — do not proceed.

---

## Step 5 — Restore Team Members and Inboxes

```bash
atm teams restore atm-dev --from ~/.claude/teams/.backups/atm-dev/<timestamp>
# Expected: N member(s) added, N inbox file(s) restored
```

Verify members and remove any ghosts:

```bash
atm members
```

Remove unexpected members (until `atm teams remove-member` ships — issue #649):

```python
python3 -c "
import json
path = '/Users/randlee/.claude/teams/atm-dev/config.json'
with open(path) as f: cfg = json.load(f)
keep = ['team-lead', 'arch-ctm', 'arch-gtm', 'arch-ctask']
cfg['members'] = [m for m in cfg['members'] if m['name'] in keep]
with open(path, 'w') as f: json.dump(cfg, f, indent=2)
print('Members:', [m['name'] for m in cfg['members']])
"
```

---

## Step 6 — Restore Claude Code Task List

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

> The Claude Code UI task panel will not show restored tasks until one task is
> created via `TaskCreate`. Create a real task to trigger the panel refresh.

> **Known bug** (issue #651): `atm teams restore` sets `.highwatermark` to
> `min_id - 1` instead of `max_id`. The script above corrects this manually.

---

## Step 7 — Verify Team Health

```bash
atm members          # confirm expected members
atm inbox            # check for unread messages
atm gh pr list       # open PRs and CI status
```

---

## Step 8 — Read Project Context

1. Read `docs/project-plan.md` — focus on current phase and open tasks
2. Check `TaskList` — recreate pending tasks via `TaskCreate` if list is empty
3. Output a concise project summary:
   - Current phase and status
   - Open PRs
   - Active teammates and their last known task
   - Next sprint(s) ready to execute

---

## Step 9 — Notify Teammates

```bash
atm send arch-ctm "New session (session-id: <SESSION_ID>). Team atm-dev restored. Please acknowledge and confirm status."
```

If no response within ~60s, nudge via tmux:

```bash
tmux list-panes -a -F '#{session_name}:#{window_index}.#{pane_index} #{pane_title}'
tmux send-keys -t <pane-id> "You have unread ATM messages. Run: atm read --team atm-dev" Enter
```

---

## Common Failure Modes

| Symptom | Cause | Fix |
|---------|-------|-----|
| `TeamCreate` returns random name | `~/.claude/teams/atm-dev` still exists | `rm -rf ~/.claude/teams/atm-dev` then retry |
| `TeamDelete` says "No team name found" | Fresh session, no active team context | Expected — proceed |
| `TaskList` returns empty after restore | Highwatermark mismatch | Set manually + create one task via `TaskCreate` |
| `atm send` fails "Agent not found" | Member lost after restore overwrite | `atm teams add-member atm-dev <name> ...` |
| Self-send (team-lead → team-lead) | Teammate wrong `ATM_IDENTITY` | Relaunch with `ATM_IDENTITY=<correct-name>` |
