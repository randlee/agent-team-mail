---
name: atm-doctor
description: Run ATM health diagnostics, present severity-sorted findings, and delegate deep remediation analysis to the atm-doctor agent when critical findings exist.
---

# ATM Doctor

This skill runs `atm doctor --json`, formats results for operators, and conditionally delegates to the `atm-doctor` background agent for remediation analysis.

## Execution

1. Run doctor JSON and capture exit code:

```bash
set -o pipefail
tmp_json="$(mktemp)"
if atm doctor --json >"$tmp_json"; then
  doctor_ec=0
else
  doctor_ec=$?
fi
cat "$tmp_json"
rm -f "$tmp_json"
```

2. Parse JSON output and render a severity-sorted findings table:
- order: `critical` -> `warn` -> `info`
- columns: `severity`, `code`, `check`, `message`

3. Behavior by exit code:
- `0`: show clean status summary; do not spawn agent.
- `2`: spawn `atm-doctor` background agent and pass:
  - team
  - exit_code
  - full doctor JSON
- `1` or other non-zero: report command/runtime failure and stop.

## Agent Delegation

When exit code is `2`, invoke:

```json
{
  "description": "Analyze atm doctor findings and propose remediation runbook",
  "prompt": "Analyze the provided atm doctor JSON and provide actionable remediation guidance.",
  "subagent_type": "atm-doctor",
  "run_in_background": true
}
```

Input payload to the agent must be fenced JSON:

```json
{
  "team": "<resolved-team>",
  "exit_code": 2,
  "doctor_json": { "summary": {}, "findings": [], "recommendations": [], "log_window": {} }
}
```

## Output Expectations

- Always present findings as a concise severity-sorted table.
- If delegated, append a short section:
  - `Agent Analysis`: condensed remediation steps
  - `Escalate`: any explicit user-escalation reasons
- Never emit raw tool traces in final user output.
