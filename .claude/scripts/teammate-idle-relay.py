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


def read_atm_toml() -> dict[str, Any] | None:
    """Read full .atm.toml from current working directory.

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
        payload.get("name"),
        payload.get("agent"),
        tool_input.get("name"),
        os.environ.get("ATM_IDENTITY"),
    )

    received_at = datetime.now(timezone.utc).strftime("%Y-%m-%dT%H:%M:%SZ")

    event = {
        "type": "teammate-idle",
        "agent": agent,
        "team": team,
        "session_id": payload.get("session_id"),
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
            "agent": agent,
            "team": team,
            "received_at": received_at,
        })
    except Exception:
        pass  # Fail-open

    return 0


if __name__ == "__main__":
    sys.exit(main())
