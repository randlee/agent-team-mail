"""Shared helpers for ATM Claude Code hook scripts.

Provides platform-aware daemon socket communication and common utilities.
All functions are fail-open: errors are logged to stderr, never raised.
"""

import json
import os
import platform
import sys
from pathlib import Path
from typing import Any


def atm_home() -> Path:
    """Resolve ATM home directory."""
    return Path(os.environ.get("ATM_HOME", str(Path.home())))


def daemon_dir() -> Path:
    """Resolve daemon directory."""
    return atm_home() / ".atm" / "daemon"


def send_hook_event(payload: dict[str, Any]) -> None:
    """Send hook_event to daemon socket. Platform-aware: AF_UNIX on Unix, TCP on Windows.

    Fail-open: any error is logged to stderr, never raised.
    """
    import socket as _socket
    import uuid

    dd = daemon_dir()
    request = {
        "version": 1,
        "request_id": str(uuid.uuid4()),
        "command": "hook-event",
        "payload": payload,
    }
    msg = (json.dumps(request, separators=(",", ":")) + "\n").encode()

    if platform.system() == "Windows":
        _send_tcp(dd, msg)
    else:
        _send_unix(dd, msg)


def _send_unix(dd: Path, msg: bytes) -> None:
    """Send via AF_UNIX socket."""
    import socket as _socket

    sock_path = dd / "atm-daemon.sock"
    if not sock_path.exists():
        return
    try:
        with _socket.socket(_socket.AF_UNIX, _socket.SOCK_STREAM) as s:
            s.settimeout(1.0)
            s.connect(str(sock_path))
            s.sendall(msg)
            try:
                s.recv(4096)  # Drain response — optional; daemon ack is fire-and-forget
            except (TimeoutError, ConnectionResetError, OSError):
                pass  # Send succeeded; recv failure is not an error
    except Exception as exc:
        sys.stderr.write(f"[atm-hook] unix socket send failed: {exc}\n")


def _send_tcp(dd: Path, msg: bytes) -> None:
    """Send via TCP localhost. Port read from atm-daemon.port file."""
    import socket as _socket

    port_file = dd / "atm-daemon.port"
    if not port_file.exists():
        return
    try:
        port = int(port_file.read_text().strip())
    except (ValueError, OSError):
        return
    try:
        with _socket.socket(_socket.AF_INET, _socket.SOCK_STREAM) as s:
            s.settimeout(1.0)
            s.connect(("127.0.0.1", port))
            s.sendall(msg)
            try:
                s.recv(4096)  # Drain response — optional; daemon ack is fire-and-forget
            except (TimeoutError, ConnectionResetError, OSError):
                pass  # Send succeeded; recv failure is not an error
    except Exception as exc:
        sys.stderr.write(f"[atm-hook] tcp socket send failed: {exc}\n")


def read_atm_toml() -> dict[str, Any] | None:
    """Read .atm.toml from cwd. Returns parsed dict or None if absent/unreadable.

    Supports Python 3.11+ (tomllib) and older versions via tomli fallback.
    """
    try:
        try:
            import tomllib
        except ImportError:
            try:
                import tomli as tomllib  # type: ignore[no-redef]
            except ImportError:
                return None

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
