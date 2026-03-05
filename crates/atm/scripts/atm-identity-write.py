#!/usr/bin/env python3
"""PreToolUse(Bash) hook: write PID-based identity file for atm commands.

Only writes when the Bash tool is about to run an `atm` or `cargo run atm`
command. All errors are fail-open (exit 0 with stderr warning).
"""
import json
import os
import platform
import shlex
import sys
import tempfile
import time
from pathlib import Path

# Import shared utilities
sys.path.insert(0, str(Path(__file__).parent))
from atm_hook_lib import load_payload, read_atm_toml, atm_home


def _is_atm_invocation(command: str) -> bool:
    """Return True if the command invokes the `atm` binary as a token.

    Matches `atm`, `/path/to/atm`, or `atm.exe` as a discrete token.
    Rejects partial matches like `atm-log.txt` or `latm`.
    """
    try:
        tokens = shlex.split(command)
    except ValueError:
        tokens = command.split()

    return any(
        t == "atm"
        or t.endswith("/atm")
        or t.endswith("\\atm")
        or t == "atm.exe"
        or t.endswith("/atm.exe")
        for t in tokens
    )


def main() -> None:
    payload = load_payload()

    command = payload.get("tool_input", {}).get("command", "")
    if not _is_atm_invocation(command):
        sys.exit(0)

    session_id = payload.get("session_id", "")

    # Resolve agent_name from .atm.toml identity field.
    toml = read_atm_toml()
    agent_name: str | None = None
    if toml:
        core = toml.get("core", {})
        raw = core.get("identity") if isinstance(core, dict) else None
        if isinstance(raw, str) and raw.strip():
            agent_name = raw.strip()

    hook_file_name = f"atm-hook-{os.getpid()}.json"
    hook_file = Path(tempfile.gettempdir()) / hook_file_name

    data = {
        "pid": os.getppid(),
        "session_id": session_id,
        "agent_name": agent_name,
        "created_at": time.time(),
    }

    try:
        hook_file.write_text(json.dumps(data))
        if platform.system() != "Windows":
            hook_file.chmod(0o600)
        else:
            sys.stderr.write("[atm-hook] Windows: skipping chmod (fail-open)\n")
    except Exception as exc:
        sys.stderr.write(f"[atm-hook] Failed to write identity file: {exc}\n")

    # Refresh updated_at on the session file (keeps the 24h TTL alive).
    # Team is resolved from .atm.toml only (not ATM_TEAM env) to ensure the heartbeat
    # updates the same path that session-start.py created. Invocations without .atm.toml
    # context do not update the timestamp.
    if session_id and agent_name:
        try:
            toml_inner = toml or {}
            core_inner = toml_inner.get("core", {}) if isinstance(toml_inner.get("core"), dict) else {}
            default_team: str = core_inner.get("default_team", "") or ""
            if default_team:
                sessions_dir = atm_home() / ".claude" / "teams" / default_team / "sessions"
                session_file = sessions_dir / f"{session_id}.json"
                if session_file.exists():
                    sf_data = json.loads(session_file.read_text())
                    sf_data["updated_at"] = time.time()
                    session_file.write_text(json.dumps(sf_data))
        except Exception:
            pass  # Fail-open: session file refresh is best-effort

    sys.exit(0)


if __name__ == "__main__":
    main()
