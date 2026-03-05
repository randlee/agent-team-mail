#!/usr/bin/env python3
"""Global SessionStart hook for Claude Code.

Reads the hook payload from stdin JSON, announces the session ID to stdout
(always), and sends hook_event/session_start when an effective team+identity
can be resolved (env vars take precedence over .atm.toml values).

Exit codes:
- 0: always (success or soft failure — fail-open)
"""

import json
import os
import sys
from pathlib import Path
from typing import Any

sys.path.insert(0, str(Path(__file__).resolve().parent))
from atm_hook_lib import send_hook_event, read_atm_toml  # noqa: E402


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

    # Resolve project/env context. Env vars take precedence; .atm.toml is fallback.
    atm_config = read_atm_toml()
    core = atm_config.get("core", {}) if isinstance(atm_config, dict) and isinstance(atm_config.get("core"), dict) else {}
    toml_team: str = core.get("default_team", "") or ""
    toml_identity: str = core.get("identity", "") or ""
    env_team: str = os.environ.get("ATM_TEAM", "").strip()
    env_identity: str = os.environ.get("ATM_IDENTITY", "").strip()
    default_team: str = env_team or toml_team
    identity: str = env_identity or toml_identity
    welcome_message: str = core.get("welcome-message", "") or ""

    if env_team and toml_team and env_team != toml_team:
        sys.stderr.write(
            f"[atm-hook] WARNING: ATM_TEAM='{env_team}' overrides .atm.toml default_team='{toml_team}'\n"
        )

    if default_team:
        print(f"ATM team: {default_team}")
    if welcome_message:
        print(f"Welcome: {welcome_message}")

    # Send hook event to daemon socket when effective routing identity is known.
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
