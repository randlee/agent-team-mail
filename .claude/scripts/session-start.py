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
import time
from pathlib import Path
from typing import Any

sys.path.insert(0, str(Path(__file__).resolve().parent))
from atm_hook_lib import atm_home, read_atm_toml, send_hook_event  # noqa: E402


def write_session_file(session_id: str, team: str, identity: str) -> None:
    """Best-effort write/update of session file for team+identity lifecycle tracking.

    Fail-open: errors are intentionally swallowed so the hook never blocks Claude.
    """
    if not session_id or not team or not identity:
        return

    try:
        sessions_dir = atm_home() / ".claude" / "teams" / team / "sessions"
        sessions_dir.mkdir(parents=True, exist_ok=True)
        session_path = sessions_dir / f"{session_id}.json"

        now = time.time()
        created_at = now

        if session_path.exists():
            try:
                existing = json.loads(session_path.read_text(encoding="utf-8"))
                existing_created = existing.get("created_at")
                if isinstance(existing_created, (int, float)) and existing_created > 0:
                    created_at = float(existing_created)
            except Exception:
                pass

        payload = {
            "session_id": session_id,
            "team": team,
            "identity": identity,
            "pid": os.getppid(),
            "created_at": created_at,
            "updated_at": now,
        }
        session_path.write_text(json.dumps(payload), encoding="utf-8")
    except Exception:
        # Hook scripts are fail-open by design.
        pass


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

    # Maintain session file for fallback session-id discovery in CLI sends.
    write_session_file(session_id, default_team, identity)

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
