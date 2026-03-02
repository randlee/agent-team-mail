---
name: named-teammate-launch
description: Launch and verify named teammates that participate in team messaging. Use when setting up a new team member with mailbox polling, troubleshooting why a teammate is not receiving messages, or creating named Claude/Codex/Gemini teammates with team-scoped communication.
---

# Named Teammate Launch

Use this skill to create teammates that are true team participants (have team membership + inbox polling), not just standalone interactive sessions.

## Tokens

Replace these placeholders before running commands or tool calls:
- `<TEAM_NAME>`
- `<TEAM_DESCRIPTION>`
- `<TEAM_LEAD_AGENT_TYPE>`
- `<TEAMMATE_NAME>`
- `<TEAMMATE_AGENT_TYPE>`
- `<RUNTIME>` (`codex` or `gemini`)
- `<INITIAL_PROMPT>`
- `<MESSAGE_TEXT>`

## Workflow A: Claude Team Tools (Named Teammate)

1. Create team:
   - Tool: `TeamCreate`
   - Required:
     - `team_name: "<TEAM_NAME>"`
     - `description: "<TEAM_DESCRIPTION>"`
     - `agent_type: "<TEAM_LEAD_AGENT_TYPE>"`

2. Spawn teammate as a named team member:
   - Tool: `Task`
   - Required:
     - `subagent_type: "<TEAMMATE_AGENT_TYPE>"`
     - `name: "<TEAMMATE_NAME>"`
     - `team_name: "<TEAM_NAME>"`
     - `run_in_background: true`
     - `prompt: "<INITIAL_PROMPT>"`

3. Verify message loop:
   - Tool: `SendMessage`
   - Required:
     - `recipient: "<TEAMMATE_NAME>"`
     - `content: "<MESSAGE_TEXT>"`

Expected result:
- Teammate receives and processes team messages.
- Team mailbox exists at:
  - `~/.claude/teams/<TEAM_NAME>/inboxes/<TEAMMATE_NAME>.json`

## Workflow B: ATM-Backed Named Teammates (Codex/Gemini)

Use this when ATM is installed and you want named runtime teammates outside Claude Task spawning.

1. Ensure member exists in roster (idempotent step):
```bash
atm teams add-member <TEAM_NAME> <TEAMMATE_NAME> --agent-type <TEAMMATE_AGENT_TYPE> --backend-type <RUNTIME>
```

2. Spawn named Codex teammate via daemon:
```bash
atm teams spawn <TEAMMATE_NAME> --team <TEAM_NAME> --runtime <RUNTIME> --prompt "<INITIAL_PROMPT>"
```

3. Validate communication:
```bash
atm send <TEAMMATE_NAME> --team <TEAM_NAME> "<MESSAGE_TEXT>"
```

Optional read/listen checks:
```bash
atm members --team <TEAM_NAME> --json
atm read --team <TEAM_NAME> --as <TEAMMATE_NAME> --timeout 30
```

## Critical Distinction

- A plain CLI display name alone is not enough for team messaging.
- The teammate must be created as a team member (named + team-scoped) to get mailbox polling and protocol participation.
- In Claude Task flow, that means setting both `name` and `team_name`.
- In ATM flow, that means adding/spawning through ATM team/member commands.

## Troubleshooting

If teammate does not receive messages:
1. Run ATM health diagnostics first:
```bash
atm doctor --team <TEAM_NAME>
```
2. For ongoing/intermittent issues, launch the `atm-monitor` agent for expert monitoring/triage workflow.
3. Confirm roster entry:
```bash
atm members --team <TEAM_NAME>
```
4. Confirm inbox file exists:
```bash
ls -l ~/.claude/teams/<TEAM_NAME>/inboxes/<TEAMMATE_NAME>.json
```
5. Send with explicit team:
```bash
atm send <TEAMMATE_NAME> --team <TEAM_NAME> "<MESSAGE_TEXT>"
```
6. For identity ambiguity in reads/sends, use explicit override:
```bash
atm read --team <TEAM_NAME> --as <TEAMMATE_NAME>
```
