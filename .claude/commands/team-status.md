---
name: team-status
description: Show a consolidated team member status table combining members, inbox, and status data from the ATM CLI. Use when the user asks about team status, who's online, or message counts.
allowed-tools: Bash
---

# Team Status

Display a consolidated team status table by running ATM CLI commands and combining the results.

## Instructions

1. Run the following commands to gather data (use the team from `.atm.toml` or `$ARGUMENTS` if a team name is provided):

```bash
atm members --team <team> --json
atm inbox --team <team>
```

2. Parse the output and render a single markdown table with these columns:

| Name | Status | Messages | Latest | Model |
|------|--------|----------|--------|-------|

Column definitions:
- **Name**: Member name from `members` output
- **Status**: Derived from `isActive` field — show "Online" if true, "Offline" if false
- **Messages**: Format as `<new>/<total>` from `inbox` output (new = unread, total = all messages)
- **Latest**: Time since last message from `inbox` output (e.g. "2m", "20h"). Omit the "ago" suffix. Show "—" if no messages.
- **Model**: From `members` JSON `model` field

3. Display team header line: **Team: `<team-name>`**

4. If `$ARGUMENTS` is provided, use it as the team name. Otherwise read `default_team` from `.atm.toml` in the repo root.
