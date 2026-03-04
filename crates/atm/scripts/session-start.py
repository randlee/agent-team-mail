#!/usr/bin/env python3
"""Global SessionStart hook for Claude Code.

Reads the hook payload from stdin JSON, announces the session ID to stdout
(always), and optionally sends a hook_event/session_start message to the
ATM daemon socket when .atm.toml exists in the current working directory.

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

    session_id: str = data.get("session_id", "") or ""
    source: str = data.get("source", "init") or "init"

    # Always print SESSION_ID to stdout for Claude context injection.
    # This is pure stdout output — safe for all Claude sessions, no file I/O.
    if session_id:
        if source == "compact":
            print(f"SESSION_ID={session_id} (returning from compact)")
        else:
            print(f"SESSION_ID={session_id} (starting fresh)")

    # From here: guard ALL side effects with .atm.toml presence.
    # File I/O and socket sends only happen when this is an ATM project session.
    atm_config = read_atm_toml()
    if atm_config is None:
        return 0  # Not an ATM project session — do nothing further

    core = atm_config.get("core", {}) if isinstance(atm_config.get("core"), dict) else {}
    default_team: str = core.get("default_team", "") or ""
    identity: str = core.get("identity", "") or ""
    welcome_message: str = core.get("welcome-message", "") or ""

    if default_team:
        print(f"ATM team: {default_team}")
    if welcome_message:
        print(f"Welcome: {welcome_message}")

    # Send hook event to daemon socket (only when .atm.toml present)
    if session_id:
        payload: dict[str, Any] = {
            "event": "session_start",
            "session_id": session_id,
            "agent": identity,
            "team": default_team,
            "source": {"kind": "claude_hook"},
            "process_id": os.getpid(),
        }
        send_hook_event(payload)

    # Write session file for CLI identity resolution
    if session_id and default_team and identity:
        try:
            sessions_dir = atm_home() / ".claude" / "sessions"
            sessions_dir.mkdir(parents=True, exist_ok=True)
            session_file = sessions_dir / f"{session_id}.json"
            import time
            session_data = {
                "session_id": session_id,
                "team": default_team,
                "identity": identity,
                "pid": os.getpid(),
                "created_at": time.time(),
            }
            session_file.write_text(json.dumps(session_data))
            import platform
            if platform.system() != "Windows":
                session_file.chmod(0o600)
        except Exception as exc:
            sys.stderr.write(f"[atm-hook] Failed to write session file: {exc}\n")

    return 0


if __name__ == "__main__":
    sys.exit(main())
