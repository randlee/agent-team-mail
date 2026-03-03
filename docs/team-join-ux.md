# Team Join UX Contract

This document defines the canonical behavior for `/team-join` and `atm teams join`.

## Command Surface

```bash
atm teams join <agent> [--team <team>] [--agent-type <type>] [--model <model>] [--folder <path>] [--json]
```

## Modes

| Mode | Entry Condition | `--team` Behavior |
|---|---|---|
| `team_lead_initiated` | Caller resolves to an existing member of current team | Optional verification only; mismatch fails non-zero |
| `self_join` | Caller is not currently on a team | Required; missing flag fails non-zero |

## Required Flow

1. Resolve caller context first using ATM config identity/team resolution.
2. Determine mode (`team_lead_initiated` or `self_join`).
3. Verify target team exists before mutation.
4. Add teammate to roster (`config.json`) using `teams add-member` validation and persistence guarantees.
5. Return launch guidance for teammate resume.

## Output Contract

Human output must include:
- mode
- team
- agent
- folder
- copy-pastable `launch_command`

JSON output must include:
- `team`
- `agent`
- `folder`
- `launch_command`
- `mode`

## Error Contract

- Team mismatch in lead-initiated mode: explicit mismatch text and non-zero exit.
- Self-join without `--team`: actionable `--team is required` text and non-zero exit.
- Missing target team/config: explicit not-found text and non-zero exit.
