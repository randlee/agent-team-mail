#!/usr/bin/env python3
"""PreToolUse hook for Task tool that blocks agents requiring named teammate deployment.

Reads the Task tool input from stdin JSON. If the subagent_type is in the
gated list and no `name` parameter is provided, blocks with exit code 2.

Named teammates have a `name` parameter → allowed.
Unnamed sidechain agents do not → blocked for gated types.

Exit codes:
- 0: Allow
- 2: Block
"""

import json
import sys

# Agent types that MUST be launched as named teammates
GATED_AGENTS = {"scrum-master"}


def main() -> int:
    try:
        data = json.load(sys.stdin)
    except Exception:
        return 0

    tool_input = data.get("tool_input", {})
    agent_type = tool_input.get("subagent_type", "")
    name = tool_input.get("name", "")

    if agent_type not in GATED_AGENTS:
        return 0

    if name:
        return 0

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


if __name__ == "__main__":
    sys.exit(main())
