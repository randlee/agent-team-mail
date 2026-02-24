#!/usr/bin/env python3
"""PID/PPID logger for agent identity correlation testing.

Called by an agent via Bash tool to log process tree information.
Compares PID/PPID with PreToolUse hook PIDs to verify the stable
agent process PID hypothesis across platforms.

Usage:
  python3 .claude/scripts/log-pid.py [label]

Log file: $TMPDIR/agent-pid-debug.jsonl (or %TEMP% on Windows)
"""
import json
import os
import platform
import sys
import tempfile
from datetime import datetime, timezone

LOG = os.path.join(tempfile.gettempdir(), "agent-pid-debug.jsonl")

label = sys.argv[1] if len(sys.argv) > 1 else ""

entry = {
    "label": label,
    "pid": os.getpid(),
    "ppid": os.getppid(),
    "platform": platform.system(),
    "ts": datetime.now(timezone.utc).strftime("%Y-%m-%dT%H:%M:%SZ"),
}

# Walk up the process tree to get grandparent PID (ppid2)
try:
    if platform.system() == "Windows":
        import subprocess
        # wmic is deprecated but widely available; fallback to PowerShell
        try:
            result = subprocess.run(
                ["wmic", "process", "where", f"processid={os.getppid()}",
                 "get", "parentprocessid", "/value"],
                capture_output=True, text=True, timeout=5
            )
            for line in result.stdout.splitlines():
                if line.startswith("ParentProcessId="):
                    entry["ppid2"] = int(line.split("=")[1].strip())
                    break
        except FileNotFoundError:
            # wmic not available, try PowerShell
            result = subprocess.run(
                ["powershell", "-Command",
                 f"(Get-Process -Id {os.getppid()}).Parent.Id"],
                capture_output=True, text=True, timeout=5
            )
            ppid2 = result.stdout.strip()
            entry["ppid2"] = int(ppid2) if ppid2 else None
    else:
        import subprocess
        result = subprocess.run(
            ["ps", "-o", "ppid=", "-p", str(os.getppid())],
            capture_output=True, text=True, timeout=2
        )
        entry["ppid2"] = int(result.stdout.strip()) if result.stdout.strip() else None
except Exception as exc:
    entry["ppid2"] = None
    entry["ppid2_error"] = str(exc)

try:
    with open(LOG, "a") as f:
        f.write(json.dumps(entry, separators=(",", ":")) + "\n")
except Exception:
    pass

# Print so the agent sees it in Bash output
print(json.dumps(entry, indent=2))
