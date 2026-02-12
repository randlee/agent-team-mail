#!/usr/bin/env python3
"""PreToolUse hook for Task tool with two gating rules.

Rule 1: Block scrum-master from being spawned without name parameter
Rule 2: Block team_name parameter unless caller is team lead

Exit codes:
- 0: Allow
- 2: Block
"""

import json
import sys
from pathlib import Path

# Agent types that MUST be launched as named teammates
GATED_AGENTS = {"scrum-master"}

DEBUG_LOG = Path("/tmp/gate-agent-spawns-debug.jsonl")


def get_lead_session_id(team_name: str) -> str | None:
    """Get the team lead's session ID from team config."""
    if not team_name or not team_name.strip():
        return None

    config_path = Path.home() / ".claude" / "teams" / team_name / "config.json"
    if not config_path.exists():
        return None

    try:
        config = json.loads(config_path.read_text())
        return config.get("leadSessionId")
    except Exception:
        return None


def main() -> int:
    try:
        data = json.load(sys.stdin)
    except Exception:
        return 0

    # Log full payload for debugging
    try:
        with DEBUG_LOG.open("a") as f:
            f.write(json.dumps(data) + "\n")
    except Exception:
        pass

    tool_input = data.get("tool_input", {})
    agent_type = tool_input.get("subagent_type", "")
    name = tool_input.get("name", "")
    team_name = tool_input.get("team_name", "")
    session_id = data.get("session_id", "")

    # Rule 1: Block gated agents without name (unnamed sidechain)
    if agent_type in GATED_AGENTS and not name:
        sys.stderr.write(
            f"BLOCKED: '{agent_type}' must be launched as a named teammate.\n"
            f"\n"
            f"Correct:\n"
            f'  Task(subagent_type="{agent_type}", name="sm-sprint-X", team_name="<team>", ...)\n'
            f"\n"
            f"Wrong:\n"
            f'  Task(subagent_type="{agent_type}", run_in_background=true)  # no name = blocked\n'
        )
        return 2

    # Rule 2: Block team_name unless caller is team lead
    if team_name and str(team_name).strip():
        lead_session_id = get_lead_session_id(team_name)

        # If we can't determine the lead (no team config), allow
        if lead_session_id and session_id != lead_session_id:
            # Caller is NOT team lead - block
            sys.stderr.write(
                f"BLOCKED: Only the team lead can spawn agents with team_name.\n"
                f"\n"
                f"You are a teammate. Use background agents:\n"
                f'  Task(subagent_type="...", run_in_background=true, prompt="...")  # no team_name\n'
                f"\n"
                f"NOT allowed from teammates:\n"
                f'  Task(..., team_name="{team_name}", ...)  # creates named teammate\n'
            )
            return 2

    return 0


if __name__ == "__main__":
    sys.exit(main())
