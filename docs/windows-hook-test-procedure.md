# Windows Hook Test Procedure

Verify Claude Code hook behaviors on Windows that were confirmed on macOS.

## Prerequisites

- Windows machine with Claude Code installed (v2.1.39+)
- Python 3.11+ (for `tomllib`)
- Agent teams enabled: `CLAUDE_CODE_EXPERIMENTAL_AGENT_TEAMS=1`
- This branch checked out: `feature/pN-s1-hook-test-harness`

## Setup

### 1. Enable test hooks

Add the catch-all PreToolUse logger to `.claude/settings.json`:

```json
{
  "hooks": {
    "PreToolUse": [
      {
        "matcher": "Task",
        "hooks": [
          {
            "type": "command",
            "command": "python3 \"$CLAUDE_PROJECT_DIR/.claude/scripts/gate-agent-spawns.py\""
          }
        ]
      },
      {
        "hooks": [
          {
            "type": "command",
            "command": "python3 \"$CLAUDE_PROJECT_DIR/.claude/scripts/log-tool-use.py\""
          }
        ]
      }
    ],
    "TeammateIdle": [
      {
        "hooks": [
          {
            "type": "command",
            "command": "python3 \"$CLAUDE_PROJECT_DIR/.claude/scripts/teammate-idle-relay.py\""
          }
        ]
      }
    ]
  }
}
```

### 2. Create `.atm.toml` in repo root (if not present)

```toml
[core]
default_team = "atm-dev"
identity = "team-lead"
```

### 3. Clear log files

```powershell
# PreToolUse log
"" | Set-Content "$env:TEMP\pretooluse-debug.jsonl"

# PID debug log
"" | Set-Content "$env:TEMP\agent-pid-debug.jsonl"

# TeammateIdle event log
$atmHome = if ($env:ATM_HOME) { $env:ATM_HOME } else { $env:USERPROFILE }
$eventsFile = Join-Path $atmHome ".claude\daemon\hooks\events.jsonl"
if (Test-Path $eventsFile) { "" | Set-Content $eventsFile }

# Gate debug log
"" | Set-Content "$env:TEMP\gate-agent-spawns-debug.jsonl"
```

## Tests

### Test 1: PreToolUse fires for teammates

**Goal**: Verify PreToolUse catch-all hook fires when a tmux teammate uses Bash/Read tools.

**Steps**:
1. Start Claude Code in the repo directory
2. Spawn a test teammate:
   ```
   Use Task tool: subagent_type=general-purpose, name=win-test-1, team_name=atm-dev, model=haiku
   Prompt: "Run this bash command: echo 'hello from win-test-1'. Then send a message to team-lead saying 'done'."
   ```
3. Wait for the teammate to complete
4. Check the log:
   ```powershell
   Get-Content "$env:TEMP\pretooluse-debug.jsonl" | ConvertFrom-Json | Format-Table tool_name, session_id, pid, ppid
   ```

**Expected**: Entries for `Bash` and `SendMessage` tool calls with PIDs and the teammate's session_id.

### Test 2: TeammateIdle resolves `teammate_name`

**Goal**: Verify `teammate_name` field is correctly resolved (not `null`).

**Steps**:
1. After Test 1, the teammate should have gone idle
2. Check the event log:
   ```powershell
   $atmHome = if ($env:ATM_HOME) { $env:ATM_HOME } else { $env:USERPROFILE }
   Get-Content (Join-Path $atmHome ".claude\daemon\hooks\events.jsonl") | ConvertFrom-Json | Format-Table agent, team, session_id, process_id
   ```

**Expected**: `agent` = `"win-test-1"` (not `null`). `process_id` should be an integer.

### Test 3: PID stability across tool calls

**Goal**: Verify `os.getppid()` from PreToolUse hook is stable (same agent PID for all tool calls from one teammate).

**Steps**:
1. Spawn a teammate that makes multiple tool calls:
   ```
   Use Task tool: subagent_type=general-purpose, name=win-test-2, team_name=atm-dev, model=haiku
   Prompt: "Do these steps in order:
   1. Run: python3 "$CLAUDE_PROJECT_DIR/.claude/scripts/log-pid.py" before-send
   2. Run: echo 'test message'
   3. Run: python3 "$CLAUDE_PROJECT_DIR/.claude/scripts/log-pid.py" after-send
   4. Send message to team-lead: 'PID test done'"
   ```
2. Check PreToolUse log:
   ```powershell
   Get-Content "$env:TEMP\pretooluse-debug.jsonl" | ConvertFrom-Json | Format-Table tool_name, pid, ppid
   ```
3. Check PID log:
   ```powershell
   Get-Content "$env:TEMP\agent-pid-debug.jsonl" | ConvertFrom-Json | Format-Table label, pid, ppid, ppid2, platform
   ```

**Expected**:
- All PreToolUse entries for win-test-2 have the **same `ppid`** (the agent PID)
- Each entry has a **different `pid`** (fresh hook process each time)
- PID log entries show `ppid2` matching the PreToolUse `ppid` (confirming grandparent = agent PID)
- `platform` = `"Windows"`

### Test 4: `os.getppid()` works on Windows

**Goal**: Confirm Python `os.getppid()` returns valid values on Windows.

**Steps**: Covered by Test 3 — check that `ppid` values are non-zero integers in all logs.

**Expected**: All `ppid` values are positive integers, not `0` or `None`.

### Test 5: Grandparent PID resolution on Windows

**Goal**: Verify `ppid2` can be obtained on Windows (via `wmic` or PowerShell).

**Steps**: Covered by Test 3 — check `ppid2` field in PID log.

**Expected**: `ppid2` is an integer (not `null`). If `ppid2_error` is present, note the error for cross-platform compatibility.

### Test 6: PreToolUse does NOT fire for lead after compaction

**Goal**: Verify the compaction behavior observed on macOS also occurs on Windows.

**Steps**:
1. In the lead session, note the current entry count:
   ```powershell
   (Get-Content "$env:TEMP\pretooluse-debug.jsonl").Count
   ```
2. Run `/compact` in the lead session
3. Spawn another teammate (Task tool call)
4. Check the log again — look for entries with the **lead's** session_id

**Expected**: No new PreToolUse entries from the lead session after compaction (only teammate entries).

### Test 7: Unit tests pass on Windows

**Goal**: Verify all hook test files pass on Windows.

```powershell
python3 -m pytest tests/hook-scripts/ -v
```

**Expected**: All 55 tests pass.

## Recording Results

For each test, note:
- **Pass/Fail**
- **Platform**: Windows version, Python version, Claude Code version
- **Any differences from macOS behavior** (especially PID/PPID values, ppid2 resolution method)
- **Log file contents** (copy relevant entries)

## Cleanup

After testing, remove the catch-all logger from `.claude/settings.json` (delete the second `PreToolUse` entry). The gate and idle relay hooks should remain.
