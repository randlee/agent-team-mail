---
name: team-join
description: Join an existing ATM team by adding a named teammate and returning the exact launch command for a Claude resume session. Use when onboarding a teammate to an established team.
allowed-tools: Bash
---

!atm members

# Team Join

Add a teammate to an existing team, then return exact resume commands to assume
that role in Claude/Codex/Gemini.

## Usage

```
/team-join <agent-name> [--team <team>] [--folder <path>] [--agent-type <type>] [--model <model>] [--runtime <claude|codex|gemini>] [--prompt <text>] [--spawn-via-atm]
```

## Instructions

1. Parse `$ARGUMENTS` into:
   - required: `<agent-name>`
   - optional: `--team`, `--folder`, `--agent-type`, `--model`, `--runtime`, `--prompt`, `--spawn-via-atm`

2. Determine caller team context first:
   - Use the preflight output from `!atm members`.
   - If the caller is already on a team, treat this as **team-lead initiated**.
   - If team-lead initiated and `--team` is present, it must match the caller team.
   - If caller is not on a team, `--team` is required.

3. Resolve target values:
   - `target_team`:
     - caller team in team-lead initiated mode
     - otherwise from `--team`
   - `target_folder`:
     - `--folder` if provided
     - otherwise current working directory (`pwd`)
   - `runtime`:
     - default `claude` when omitted

4. Verify target team exists and read current roster:
   - Run:
     ```bash
     atm members --team "<target_team>" --json
     ```
   - If this fails, stop and print a clear error.
   - Parse member list to determine if `<agent-name>` is already present.
   - If present, this command is idempotent: do not call `add-member`.

5. Resolve `color` (for role-assumption command output):
   - If member already exists and has `color`, use that value.
   - Else try `.claude/agents/<agent-name>.md` frontmatter `color`.
   - Else use `cyan`.

6. Add teammate to the existing team roster only when missing:
   - Base command:
     ```bash
     atm teams add-member "<target_team>" "<agent-name>" --cwd "<target_folder>"
     ```
   - Append `--agent-type` and `--model` when provided.
   - If add-member fails, stop and report the CLI error.
   - If member is already present, print:
     - `Requested team member is already in the roster.`

7. Build runtime bootstrap text for Codex/Gemini prompt injection.

   Use this bootstrap block:
   ```text
   Agent-teams-mail is configured for this session.
   <team-lead> is orchestrating this session.
   Use:
   atm read --timeout 60
   atm send <team-member> "<message>" --team <team>
   ```

   - If `--prompt` is provided, wrap it as:
     - bootstrap block
     - caller prompt text
     - `Acknowledge and wait for team instructions.`
   - If `--prompt` is not provided, use only bootstrap block + final acknowledgment line.

8. Build and return role-assumption commands.

   Required commands (always print all three, with known values):
   - Claude (plain resume mode, no explicit team CLI flags):
     ```bash
     cd "<target_folder>" && env ATM_TEAM="<target_team>" ATM_IDENTITY="<agent-name>" ATM_AGENT_COLOR="<color>" claude --resume
     ```
   - Claude (team-options launch mode, using Claude binary flags):
     ```bash
     cd "<target_folder>" && env CLAUDE_CODE_EXPERIMENTAL_AGENT_TEAMS=1 ATM_TEAM="<target_team>" ATM_IDENTITY="<agent-name>" "<claude_binary_path>" --agent-id "<agent-name>@<target_team>" --agent-name "<agent-name>" --team-name "<target_team>" --agent-color "<color>" --resume
     ```
   - Codex:
     ```bash
     cd "<target_folder>" && atm teams spawn "<agent-name>" --team "<target_team>" --runtime codex --resume --env ATM_AGENT_COLOR="<color>" --prompt "<bootstrap_plus_optional_user_prompt>"
     ```
   - Gemini:
     ```bash
     cd "<target_folder>" && atm teams spawn "<agent-name>" --team "<target_team>" --runtime gemini --resume --env ATM_AGENT_COLOR="<color>" --prompt "<bootstrap_plus_optional_user_prompt>"
     ```
   - If `--spawn-via-atm` is requested, also provide:
     ```bash
     cd "<target_folder>" && atm teams spawn "<agent-name>" --team "<target_team>" --runtime "<runtime>" --resume
     ```

9. Output format (tolerant / diagnostics-first):
   - First line: concise mode summary:
     - `Mode: team_lead_initiated` or `Mode: self_join`
   - Then print known context fields even if partial:
     - team
     - agent
     - folder
     - color
     - roster state (`added` or `already_present`)
     - add-member command used (or `skipped`)
     - Claude plain resume command
     - Claude team-options launch command
     - Codex resume command
     - Gemini resume command
     - bootstrap prompt used for Codex/Gemini launch
     - optional spawn command (if requested)

## Failure Rules

- Missing `<agent-name>`: print usage and stop.
- Not on a team and no `--team`: error and stop.
- Team mismatch in team-lead mode (`--team` differs from caller team): error and stop.
- Team does not exist: error and stop.
- Any ATM command failure: print stderr and stop.
