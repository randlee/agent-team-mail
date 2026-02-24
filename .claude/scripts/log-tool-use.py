#!/usr/bin/env python3
"""PreToolUse catch-all logger for hook testing.

Captures full payload from every tool call to a JSONL debug file.
NOT intended for production — enable temporarily in .claude/settings.json
for hook behavior verification.

Log file: $TMPDIR/pretooluse-debug.jsonl (or %TEMP% on Windows)
"""
import json
import os
import sys
import tempfile
from datetime import datetime, timezone

LOG = os.path.join(tempfile.gettempdir(), "pretooluse-debug.jsonl")

try:
    data = json.load(sys.stdin)
except Exception:
    sys.exit(0)

entry = {
    "tool_name": data.get("tool_name"),
    "session_id": data.get("session_id", "")[:12],
    "hook_event_name": data.get("hook_event_name"),
    "cwd": data.get("cwd"),
    "permission_mode": data.get("permission_mode"),
    "pid": os.getpid(),
    "ppid": os.getppid(),
    "ts": datetime.now(timezone.utc).strftime("%Y-%m-%dT%H:%M:%SZ"),
}

# Include tool_input summary (truncated for readability)
tool_input = data.get("tool_input", {})
if isinstance(tool_input, dict):
    entry["tool_input_keys"] = list(tool_input.keys())
    # For Bash, include the command
    if "command" in tool_input:
        cmd = tool_input["command"]
        entry["command_preview"] = cmd[:100] if isinstance(cmd, str) else str(cmd)[:100]

try:
    with open(LOG, "a") as f:
        f.write(json.dumps(entry, separators=(",", ":")) + "\n")
except Exception:
    pass

sys.exit(0)  # Always allow — never block tool execution
