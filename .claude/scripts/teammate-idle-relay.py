#!/usr/bin/env python3
"""TeammateIdle hook relay for ATM daemon state tracking.

Reads the hook payload from stdin JSON, enriches with ATM identity/team context,
and appends one JSON line to:
  ${ATM_HOME:-$HOME}/.claude/daemon/hooks/events.jsonl

Also sends a hook_event/teammate_idle message to the ATM daemon socket (when
.atm.toml exists in the cwd) so daemon state is updated in real-time. The file
write remains the durable audit trail; the socket send is additive.

Both the file write and the socket send are guarded by .atm.toml presence.
Non-ATM Claude Code sessions are completely unaffected.

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

# Import shared helpers (same directory)
sys.path.insert(0, str(Path(__file__).resolve().parent))
from atm_hook_lib import send_hook_event, read_atm_toml, first_str, load_payload  # noqa: E402


def append_event(event: dict[str, Any]) -> None:
    """Append one event JSON line to daemon hook event log."""
    from atm_hook_lib import atm_home
    events_file = atm_home() / ".claude" / "daemon" / "hooks" / "events.jsonl"
    events_file.parent.mkdir(parents=True, exist_ok=True)
    with events_file.open("a", encoding="utf-8") as f:
        f.write(json.dumps(event, separators=(",", ":")) + "\n")


def main() -> int:
    payload = load_payload()

    # Guard ALL side effects (file I/O and socket sends) with .atm.toml presence.
    # Non-ATM Claude Code sessions produce no file writes or socket calls at all.
    atm_config = read_atm_toml()
    if atm_config is None:
        return 0  # Not an ATM project session — do nothing

    tool_input = payload.get("tool_input", {}) if isinstance(payload.get("tool_input"), dict) else {}

    core = atm_config.get("core", {}) if isinstance(atm_config.get("core"), dict) else {}
    required_team: str | None = core.get("default_team") or None

    team = first_str(
        payload.get("team_name"),
        tool_input.get("team_name"),
        payload.get("team"),
        os.environ.get("ATM_TEAM"),
        required_team,
    )
    agent = first_str(
        payload.get("teammate_name"),   # Claude Code TeammateIdle payload key
        payload.get("name"),
        payload.get("agent"),
        tool_input.get("name"),
        os.environ.get("ATM_IDENTITY"),
    )

    # Require both identity fields before any audit write/socket send.
    # Fail-open: skip relay if required identity context cannot be resolved.
    if not team or not agent:
        return 0

    received_at = datetime.now(timezone.utc).strftime("%Y-%m-%dT%H:%M:%SZ")

    # Parent PID is the long-lived Claude session process.
    process_id = os.getppid()

    event = {
        "type": "teammate-idle",
        "agent": agent,
        "team": team,
        "session_id": payload.get("session_id"),
        "process_id": process_id,
        "received_at": received_at,
        "payload": payload,
    }

    try:
        append_event(event)
    except Exception:
        # Fail open: never block teammate progress if relay has an issue.
        pass

    # Socket send — additive real-time update.
    # File write above is the durable audit trail and is unaffected by socket errors.
    try:
        send_hook_event({
            "event": "teammate_idle",
            "session_id": payload.get("session_id"),
            "process_id": process_id,
            "agent": agent,
            "team": team,
            "received_at": received_at,
            "source": {"kind": "claude_hook"},
        })
    except Exception:
        pass  # Fail-open

    return 0


if __name__ == "__main__":
    sys.exit(main())
