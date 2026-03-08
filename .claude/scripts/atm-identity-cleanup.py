#!/usr/bin/env python3
"""PostToolUse(Bash) hook: delete PID-based identity file written by atm-identity-write.py.

Deletes the hook file keyed by the current process PID (same PID the
PreToolUse hook used when it created the file).  All errors are fail-open
(exit 0 with stderr warning).
"""
import os
import sys
import tempfile
from pathlib import Path

# Import shared utilities
sys.path.insert(0, str(Path(__file__).parent))
from atm_hook_lib import first_str, load_payload, read_atm_toml


def main() -> None:
    # load_payload() drains stdin so the hook machinery doesn't block.
    load_payload()
    toml = read_atm_toml()
    core = toml.get("core", {}) if isinstance(toml, dict) else {}
    team_name = first_str(os.environ.get("ATM_TEAM"), core.get("default_team"))
    agent_name = first_str(os.environ.get("ATM_IDENTITY"), core.get("identity"))
    if toml is None and not team_name and not agent_name:
        sys.exit(0)

    hook_file = Path(tempfile.gettempdir()) / f"atm-hook-{os.getpid()}.json"

    try:
        hook_file.unlink()
    except FileNotFoundError:
        # File was never written (non-atm command) — that is normal.
        pass
    except Exception as exc:
        sys.stderr.write(f"[atm-hook] Failed to delete identity file {hook_file}: {exc}\n")

    sys.exit(0)


if __name__ == "__main__":
    main()
