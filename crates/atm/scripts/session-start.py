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
from atm_hook_lib import first_str, send_hook_event, read_atm_toml, atm_home  # noqa: E402


# ── Main ──────────────────────────────────────────────────────────────────────

def load_team_lead_context(team: str) -> tuple[str, str]:
    """Return (lead_name, lead_session_id) from team config, best-effort."""
    if not team:
        return "", ""
    try:
        cfg_path = atm_home() / ".claude" / "teams" / team / "config.json"
        cfg = json.loads(cfg_path.read_text())
        lead_agent_id = str(cfg.get("leadAgentId", "") or "")
        lead_session_id = str(cfg.get("leadSessionId", "") or "")
        lead_name = lead_agent_id.split("@", 1)[0].strip() if lead_agent_id else ""
        return lead_name, lead_session_id.strip()
    except Exception:
        return "", ""


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

    if session_id:
        if source == "compact":
            print(f"SESSION_ID={session_id} (returning from compact)")
        else:
            print(f"SESSION_ID={session_id} (starting fresh)")

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

    # Prevent cross-identity corruption: non-lead sessions must not claim the
    # configured leadSessionId for this team.
    lead_name, lead_session_id = load_team_lead_context(default_team)
    lead_session_collision = (
        bool(session_id)
        and bool(default_team)
        and bool(identity)
        and bool(lead_name)
        and bool(lead_session_id)
        and session_id == lead_session_id
        and identity != lead_name
    )
    if lead_session_collision:
        sys.stderr.write(
            f"[atm-hook] WARNING: refusing session_start relay for non-lead identity "
            f"'{identity}' using reserved leadSessionId '{session_id}' (lead='{lead_name}')\n"
        )
        return 0
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

    # Write session file for CLI identity resolution fallback.
    # Only written when we have full routing context (session_id + team + identity).
    # Uses os.getppid() — the long-lived Claude session process PID, not this
    # short-lived hook subprocess.
    #
    # Intentionally non-atomic: session files are transient/rewritable and the
    # 24-hour TTL in read_session_file() handles stale or partially-written files.
    # The hook is short-lived and crash probability is negligible.
    if session_id and default_team and identity:
        try:
            import time
            sessions_dir = atm_home() / ".claude" / "teams" / default_team / "sessions"
            sessions_dir.mkdir(parents=True, exist_ok=True)
            session_file = sessions_dir / f"{session_id}.json"
            # Preserve created_at on re-fires (compact/resume) for the same session_id.
            # Only created_at is preserved; updated_at is always refreshed.
            existing_created_at: float | None = None
            if session_file.exists():
                try:
                    existing = json.loads(session_file.read_text())
                    if existing.get("session_id") == session_id:
                        existing_created_at = existing.get("created_at")
                except Exception:
                    pass
            now = time.time()
            session_data = {
                "session_id": session_id,
                "team": default_team,
                "identity": identity,
                "pid": os.getppid(),
                "created_at": existing_created_at if existing_created_at else now,
                "updated_at": now,
            }
            session_file.write_text(json.dumps(session_data))
        except Exception as exc:
            sys.stderr.write(f"[atm-hook] Failed to write session file: {exc}\n")

    return 0


if __name__ == "__main__":
    sys.exit(main())
