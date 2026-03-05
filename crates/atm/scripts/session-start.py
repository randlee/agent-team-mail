#!/usr/bin/env python3
"""Global SessionStart hook for Claude Code.

Reads the hook payload from stdin JSON, announces the session ID to stdout
(always), and optionally sends a hook_event/session_start message to the
ATM daemon socket when routing context is available from either:
- `.atm.toml` in the current working directory, or
- `ATM_TEAM` / `ATM_IDENTITY` environment variables.

Exit codes:
- 0: always (success or soft failure — fail-open)
"""

import json
import os
import sys
from pathlib import Path
from typing import Any

sys.path.insert(0, str(Path(__file__).resolve().parent))
from atm_hook_lib import first_str, send_hook_event, read_atm_toml  # noqa: E402


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

    # Resolve routing context from .atm.toml (repo) or env (spawned teammates).
    # This keeps hooks fail-open for non-ATM sessions while still supporting
    # cross-folder spawned teammates that rely on env-only context.
    atm_config = read_atm_toml()
    core: dict[str, Any] = {}
    if isinstance(atm_config, dict):
        maybe_core = atm_config.get("core")
        if isinstance(maybe_core, dict):
            core = maybe_core
    default_team: str = first_str(os.environ.get("ATM_TEAM"), core.get("default_team")) or ""
    identity: str = first_str(os.environ.get("ATM_IDENTITY"), core.get("identity")) or ""
    welcome_message: str = core.get("welcome-message", "") or ""

    if atm_config is None and not default_team and not identity:
        return 0  # Not an ATM project session and no env fallback — do nothing further

    if isinstance(atm_config, dict):
        toml_team: str = (
            core.get("default_team", "")
            if isinstance(core.get("default_team", ""), str)
            else ""
        )
        env_team = (os.environ.get("ATM_TEAM") or "").strip()
        if env_team and toml_team and env_team != toml_team:
            sys.stderr.write(
                f"[atm-hook] WARNING: ATM_TEAM='{env_team}' overrides .atm.toml default_team='{toml_team}'\n"
            )

    if default_team:
        print(f"ATM team: {default_team}")
    if welcome_message:
        print(f"Welcome: {welcome_message}")

    # Send hook event to daemon socket when we have complete routing context.
    # Use parent PID (Claude session process), not this short-lived hook PID.
    if session_id and default_team and identity:
        payload: dict[str, Any] = {
            "event": "session_start",
            "session_id": session_id,
            "agent": identity,
            "team": default_team,
            "source": {"kind": "claude_hook"},
            "process_id": os.getppid(),
        }
        send_hook_event(payload)

    return 0


if __name__ == "__main__":
    sys.exit(main())
