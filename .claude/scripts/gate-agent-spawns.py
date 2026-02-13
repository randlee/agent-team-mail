#!/usr/bin/env python3
"""PreToolUse hook that enforces safe agent spawning patterns for orchestrators.

## Why This Exists

The scrum-master agent acts as an ORCHESTRATOR — it coordinates sprints by
spawning dev and QA sub-agents to do the actual work. Without this gate,
orchestrators can accidentally spawn agents incorrectly, leading to:

1. Resource exhaustion: Each named teammate = tmux pane. 3 scrum-masters each
   spawning 2 named teammates = 9 panes. With background agents = 3 panes.

2. Lifecycle issues: Background agents without names can't compact and die at
   context limit. Orchestrators need full teammate status to survive long sprints.

This gate enforces two rules:
- Rule 1: Orchestrators (scrum-master) MUST be named teammates
- Rule 2: Only the team LEAD can create named teammates (not orchestrators themselves)

## Mode Compatibility

Works with both in-process and tmux teammates because it uses PreToolUse hooks
in settings.json (fires for ALL Task calls) and session_id differentiation
(present in both modes). See Reddit post for mode differences:
https://www.reddit.com/r/ClaudeCode/comments/1qzypcs/playing_around_with_the_new_agent_teams_experiment/

NOTE: Agent-teams is pre-release as of 2/11/2026. Verified on Claude Code v2.1.39+.

Exit codes: 0 = Allow, 2 = Block
"""

import json
import sys
from pathlib import Path

# Orchestrator agents that require full teammate lifecycle
ORCHESTRATORS = {"scrum-master"}

DEBUG_LOG = Path("/tmp/gate-agent-spawns-debug.jsonl")


def get_lead_session_id(team_name: str) -> str | None:
    """Get team lead's session ID to differentiate lead from teammates.

    Returns None if team doesn't exist (allows by default).
    """
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
        # Can't parse input - allow by default (fail open)
        return 0

    # Log for debugging
    try:
        with DEBUG_LOG.open("a") as f:
            f.write(json.dumps(data) + "\n")
    except Exception:
        pass

    tool_input = data.get("tool_input", {})
    subagent_type = tool_input.get("subagent_type", "")
    teammate_name = tool_input.get("name", "")  # If present, spawns named teammate
    team_name = tool_input.get("team_name", "")  # If present, spawns into team
    session_id = data.get("session_id", "")

    # Rule 1: Orchestrators must be spawned with teammate_name
    # WHY: They need full lifecycle (compaction, proper shutdown) to coordinate
    # long-running sprints. Background agents die at context limit.
    if subagent_type in ORCHESTRATORS and not teammate_name:
        sys.stderr.write(
            f"BLOCKED: '{subagent_type}' is an orchestrator and must be a named teammate.\n"
            f"\n"
            f"Correct:\n"
            f'  Task(subagent_type="{subagent_type}", name="sm-sprint-X", team_name="<team>", ...)\n'
            f"\n"
            f"Wrong:\n"
            f'  Task(subagent_type="{subagent_type}", run_in_background=true)  # no name = dies at context limit\n'
        )
        return 2

    # Rule 2: Only team LEAD can spawn agents with team_name
    # WHY: Prevents orchestrators from creating teammates (pane exhaustion).
    # Orchestrators should spawn background agents (no team_name, no teammate_name).
    if team_name and str(team_name).strip():
        lead_session_id = get_lead_session_id(team_name)

        # Allow if we can't determine lead (no team config yet)
        # WHY: Fail open - team might be new, don't block legitimate spawns
        if not lead_session_id:
            return 0

        # Allow if caller IS the team lead
        # WHY: Lead creates the orchestrators, needs team_name to add them to team
        if session_id == lead_session_id:
            return 0

        # Block: caller is a teammate trying to use team_name
        # WHY: Teammates spawning teammates = pane explosion
        sys.stderr.write(
            f"BLOCKED: Only the team lead can spawn agents with team_name.\n"
            f"\n"
            f"You are a teammate. Use background agents:\n"
            f'  Task(subagent_type="...", run_in_background=true, prompt="...")  # no team_name\n'
            f"\n"
            f"NOT allowed from teammates:\n"
            f'  Task(..., team_name="{team_name}", ...)  # creates named teammate = pane exhaustion\n'
        )
        return 2

    # Allow: All checks passed
    # WHY: Either spawning non-orchestrator, or spawning background agent (no team_name)
    return 0


if __name__ == "__main__":
    sys.exit(main())
