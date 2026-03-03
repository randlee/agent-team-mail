---
name: team-join
description: Join an existing ATM team by adding a named teammate and returning a copy-pastable `claude --resume` command.
allowed-tools: Bash
---

!atm members

# Team Join

Use this command to onboard a teammate into an existing ATM team and return the
exact launch command for the teammate session.

## Usage

```bash
/team-join <agent> [--team <team>] [--agent-type <type>] [--model <model>] [--folder <path>] [--json]
```

## Behavior Contract

1. Caller team context is checked first by `atm teams join`.
2. Team-lead-initiated mode:
- If caller is already in a team, `--team` is optional verification.
- If `--team` is provided and mismatches current team, fail.
3. Self-join mode:
- If caller is not in a team, `--team` is required.
4. Join action:
- Adds `<agent>` to team roster (`config.json`) using ATM validation rules.
5. Output contract:
- Human output includes mode, team, agent, folder, and a copy-pastable launch command.
- JSON output includes exactly: `team`, `agent`, `folder`, `launch_command`, `mode`.

## Execution

1. Parse `$ARGUMENTS` into:
- required: `<agent>`
- optional: `--team`, `--agent-type`, `--model`, `--folder`
2. Run:
```bash
atm teams join <agent> [--team <team>] [--agent-type <type>] [--model <model>] [--folder <path>] [--json]
```
3. If command fails, return ATM error text unchanged.
4. If command succeeds, return output unchanged.

## Failure Rules

- Missing `<agent>`: print usage and stop.
- Team mismatch in team-lead-initiated mode: fail non-zero with mismatch guidance.
- Self-join without `--team`: fail non-zero with actionable `--team required` guidance.
