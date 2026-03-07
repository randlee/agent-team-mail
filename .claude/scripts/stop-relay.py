#!/usr/bin/env python3
"""Stop hook relay for ATM daemon state tracking.

Marks an agent as idle after Claude finishes a response turn.
Fail-open: never blocks Claude execution.
"""

import os
import sys
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
from atm_hook_lib import first_str, load_payload, read_atm_toml, send_hook_event  # noqa: E402


def main() -> int:
    payload = load_payload()
    atm_config = read_atm_toml()
    core = atm_config.get("core", {}) if isinstance(atm_config, dict) else {}

    team = first_str(
        payload.get("team_name"),
        payload.get("team"),
        os.environ.get("ATM_TEAM"),
        core.get("default_team"),
    )
    agent = first_str(
        payload.get("teammate_name"),
        payload.get("name"),
        payload.get("agent"),
        os.environ.get("ATM_IDENTITY"),
        core.get("identity"),
    )
    session_id = first_str(payload.get("session_id"))

    if atm_config is None and not team and not agent:
        return 0
    if not team or not agent:
        return 0

    try:
        send_hook_event(
            {
                "event": "stop",
                "session_id": session_id or "",
                "process_id": os.getppid(),
                "agent": agent,
                "team": team,
                "source": {"kind": "claude_hook"},
            }
        )
    except Exception:
        pass
    return 0


if __name__ == "__main__":
    sys.exit(main())
