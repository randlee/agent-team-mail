#!/usr/bin/env python3
"""Global SessionEnd hook for Claude Code.

Reads the hook payload from stdin JSON and, when .atm.toml exists in the
current working directory, sends a hook_event/session_end message to the
ATM daemon socket so the daemon can mark the session as dead in real-time.

No stdout output is needed (SessionEnd output does not go to Claude context).

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
    # If .atm.toml is absent this is not an ATM project session — do nothing.
    atm_config = read_atm_toml()
    if atm_config is None:
        return 0  # Not an ATM project session — do nothing

    session_id: str = data.get("session_id", "") or ""
    core = atm_config.get("core", {}) if isinstance(atm_config.get("core"), dict) else {}
    default_team: str = core.get("default_team", "") or ""
    identity: str = core.get("identity", "") or ""

    if not session_id:
        return 0

    payload: dict[str, Any] = {
        "event": "session_end",
        "session_id": session_id,
        # Parent PID is the long-lived Claude session process.
        "process_id": os.getppid(),
        "agent": identity,
        "team": default_team,
        "reason": "session_exit",
        "source": {"kind": "claude_hook"},
    }
    send_hook_event(payload)

    # Clean up THIS session's file only (fail-open — missing file is fine).
    sessions_dir = atm_home() / ".claude" / "sessions"
    session_file = sessions_dir / f"{session_id}.json"
    try:
        session_file.unlink(missing_ok=True)
    except Exception as exc:
        sys.stderr.write(f"[atm-hook] Failed to delete session file: {exc}\n")

    return 0


if __name__ == "__main__":
    sys.exit(main())
