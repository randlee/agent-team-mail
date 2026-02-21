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


# ── TOML parsing ──────────────────────────────────────────────────────────────

def read_atm_toml() -> dict[str, Any] | None:
    """Read .atm.toml from current working directory.

    Returns the parsed config dict, or None if not present / unreadable.
    """
    try:
        import tomllib
        toml_path = Path(".atm.toml")
        if not toml_path.exists():
            return None
        with toml_path.open("rb") as f:
            return tomllib.load(f)
    except Exception:
        return None


# ── Socket helper ─────────────────────────────────────────────────────────────

def send_hook_event(payload: dict[str, Any]) -> None:
    """Send hook_event to daemon socket. Fail-open: any error is silently swallowed."""
    import socket as _socket
    import uuid
    atm_home = Path(os.environ.get("ATM_HOME", str(Path.home())))
    sock_path = atm_home / ".claude" / "daemon" / "atm-daemon.sock"
    if not sock_path.exists():
        return
    request = {
        "version": 1,
        "request_id": str(uuid.uuid4()),
        "command": "hook-event",
        "payload": payload,
    }
    msg = (json.dumps(request, separators=(",", ":")) + "\n").encode()
    try:
        with _socket.socket(_socket.AF_UNIX, _socket.SOCK_STREAM) as s:
            s.settimeout(1.0)
            s.connect(str(sock_path))
            s.sendall(msg)
            # Drain response (ignore content)
            s.recv(4096)
    except Exception:
        pass  # Fail-open


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

    # Always print SESSION_ID to stdout for Claude context injection
    if session_id:
        if source == "compact":
            print(f"SESSION_ID={session_id} (returning from compact)")
        else:
            print(f"SESSION_ID={session_id} (starting fresh)")

    # Read .atm.toml — guards both the context output AND the socket call
    atm_config = read_atm_toml()
    if atm_config is not None:
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
                "source": source if source == "compact" else "init",
                "process_id": os.getpid(),
            }
            send_hook_event(payload)

    return 0


if __name__ == "__main__":
    sys.exit(main())
