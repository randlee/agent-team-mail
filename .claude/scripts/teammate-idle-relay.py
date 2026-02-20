#!/usr/bin/env python3
"""TeammateIdle hook relay for ATM daemon state tracking.

Reads the hook payload from stdin JSON, enriches with ATM identity/team context,
and appends one JSON line to:
  ${ATM_HOME:-$HOME}/.claude/daemon/hooks/events.jsonl

The script is fail-open: it never blocks teammate flow.
Exit codes:
- 0: always (success or soft failure)
"""

import json
import os
import sys
from datetime import datetime, timezone
from pathlib import Path
from typing import Any


def read_required_team() -> str | None:
    """Read .atm.toml core.default_team from project root."""
    try:
        import tomllib

        toml_path = Path(".atm.toml")
        if not toml_path.exists():
            return None
        with toml_path.open("rb") as f:
            config = tomllib.load(f)
        return config.get("core", {}).get("default_team")
    except Exception:
        return None


def first_str(*values: Any) -> str | None:
    """Return first non-empty string value."""
    for value in values:
        if isinstance(value, str) and value.strip():
            return value
    return None


def load_payload() -> dict[str, Any]:
    """Best-effort parse stdin JSON payload."""
    try:
        data = json.load(sys.stdin)
        if isinstance(data, dict):
            return data
    except Exception:
        pass
    return {}


def append_event(event: dict[str, Any]) -> None:
    """Append one event JSON line to daemon hook event log."""
    atm_home = Path(os.environ.get("ATM_HOME", str(Path.home())))
    events_file = atm_home / ".claude" / "daemon" / "hooks" / "events.jsonl"
    events_file.parent.mkdir(parents=True, exist_ok=True)
    with events_file.open("a", encoding="utf-8") as f:
        f.write(json.dumps(event, separators=(",", ":")) + "\n")


def main() -> int:
    payload = load_payload()
    tool_input = payload.get("tool_input", {}) if isinstance(payload.get("tool_input"), dict) else {}

    required_team = read_required_team()
    team = first_str(
        payload.get("team_name"),
        tool_input.get("team_name"),
        payload.get("team"),
        os.environ.get("ATM_TEAM"),
        required_team,
    )
    agent = first_str(
        payload.get("name"),
        payload.get("agent"),
        tool_input.get("name"),
        os.environ.get("ATM_IDENTITY"),
    )

    event = {
        "type": "teammate-idle",
        "agent": agent,
        "team": team,
        "session_id": payload.get("session_id"),
        "received_at": datetime.now(timezone.utc).strftime("%Y-%m-%dT%H:%M:%SZ"),
        "payload": payload,
    }

    try:
        append_event(event)
    except Exception:
        # Fail open: never block teammate progress if relay has an issue.
        pass

    return 0


if __name__ == "__main__":
    sys.exit(main())
