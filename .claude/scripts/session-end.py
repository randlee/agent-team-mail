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


# ── TOML parsing ──────────────────────────────────────────────────────────────

def read_atm_toml() -> dict[str, Any] | None:
    """Read .atm.toml from current working directory.

    Returns the parsed config dict, or None if not present / unreadable.
    Supports Python 3.11+ (tomllib) and older versions via tomli fallback.
    """
    try:
        try:
            import tomllib
        except ImportError:
            try:
                import tomli as tomllib  # type: ignore[no-redef]
            except ImportError:
                return None  # Cannot parse TOML; treat as absent

        toml_path = Path(".atm.toml")
        if not toml_path.exists():
            return None
        with toml_path.open("rb") as f:
            return tomllib.load(f)
    except Exception:
        return None


# ── Socket helper ─────────────────────────────────────────────────────────────

def send_hook_event(payload: dict[str, Any]) -> None:
    """Send hook_event to daemon socket. Fail-open: any error is logged to stderr."""
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
    except Exception as exc:  # noqa: BLE001
        sys.stderr.write(f"[atm-hook] socket send failed: {exc}\n")


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
        "agent": identity,
        "team": default_team,
        "reason": "session_exit",
    }
    send_hook_event(payload)

    return 0


if __name__ == "__main__":
    sys.exit(main())
