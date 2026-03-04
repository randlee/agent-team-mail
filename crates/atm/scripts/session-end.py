#!/usr/bin/env python3
"""Global SessionEnd hook for Claude Code.

Reads the hook payload from stdin JSON, sends a hook_event/session_end message
to the ATM daemon, and cleans up the session file.

Exit codes:
- 0: always (success or soft failure — fail-open)
"""

import json
import os
import sys
from pathlib import Path
from typing import Any

sys.path.insert(0, str(Path(__file__).resolve().parent))
from atm_hook_lib import send_hook_event, read_atm_toml, atm_home  # noqa: E402


# ── Main ──────────────────────────────────────────────────────────────────────

def main() -> int:
    # Parse stdin JSON payload (best-effort)
    try:
        data = json.load(sys.stdin)
        if not isinstance(data, dict):
            data = {}
    except Exception:
        data = {}

    # Guard ALL side effects with .atm.toml presence.
    atm_config = read_atm_toml()
    if atm_config is None:
        return 0  # Not an ATM project session — do nothing

    session_id: str = data.get("session_id", "") or ""
    core = atm_config.get("core", {}) if isinstance(atm_config.get("core"), dict) else {}
    default_team: str = core.get("default_team", "") or ""
    identity: str = core.get("identity", "") or ""

    if not session_id:
        return 0

    # Send hook event to daemon socket
    payload: dict[str, Any] = {
        "event": "session_end",
        "session_id": session_id,
        "agent": identity,
        "team": default_team,
        "reason": "session_exit",
        "source": {"kind": "claude_hook"},
    }
    send_hook_event(payload)

    # Clean up THIS session's file only
    sessions_dir = atm_home() / ".claude" / "sessions"
    session_file = sessions_dir / f"{session_id}.json"
    try:
        session_file.unlink(missing_ok=True)
    except Exception as exc:
        sys.stderr.write(f"[atm-hook] Failed to delete session file: {exc}\n")

    return 0


if __name__ == "__main__":
    sys.exit(main())
