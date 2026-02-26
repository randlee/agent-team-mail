#!/usr/bin/env bash
set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
tmp_dir="$(mktemp -d)"
cleanup() {
  rm -rf "$tmp_dir"
}
trap cleanup EXIT

mkdir -p "$tmp_dir/home/.claude/teams/atm-dev"
cat >"$tmp_dir/home/.claude/teams/atm-dev/config.json" <<'JSON'
{
  "name": "atm-dev",
  "createdAt": 1770765919076,
  "leadAgentId": "team-lead@atm-dev",
  "leadSessionId": "6075f866-f103-4be1-b2e9-8dbf66009eb9",
  "members": [
    {
      "agentId": "team-lead@atm-dev",
      "name": "team-lead",
      "agentType": "general-purpose",
      "model": "claude-haiku-4-5-20251001",
      "joinedAt": 1770765919076,
      "tmuxPaneId": "",
      "cwd": "/tmp",
      "subscriptions": []
    },
    {
      "agentId": "arch-ctm@atm-dev",
      "name": "arch-ctm",
      "agentType": "general-purpose",
      "model": "gpt-5.2",
      "joinedAt": 1770765919077,
      "tmuxPaneId": "",
      "cwd": "/tmp",
      "subscriptions": []
    },
    {
      "agentId": "qa-bot@atm-dev",
      "name": "qa-bot",
      "agentType": "general-purpose",
      "model": "claude-haiku-4-5-20251001",
      "joinedAt": 1770765919078,
      "tmuxPaneId": "",
      "cwd": "/tmp",
      "subscriptions": []
    }
  ]
}
JSON

echo "Building atm-agent-mcp test binary..."
cargo build -q -p agent-team-mail-mcp --bin atm-agent-mcp

echo "Running MCP Inspector smoke suite (reference + atm-agent-mcp)..."
ATM_HOME="$tmp_dir/home" ATM_IDENTITY="arch-ctm" ATM_TEAM="atm-dev" REPO_ROOT="$repo_root" python3 - <<'PY'
import json
import os
import subprocess
from pathlib import Path

repo_root = Path(os.environ["REPO_ROOT"])
atm_bin = repo_root / "target/debug/atm-agent-mcp"

def run(cmd, *, allow_failure=False):
    proc = subprocess.run(cmd, text=True, capture_output=True, timeout=60, env=os.environ.copy())
    if not allow_failure and proc.returncode != 0:
        raise AssertionError(
            f"Command failed: {' '.join(cmd)}\nSTDOUT:\n{proc.stdout}\nSTDERR:\n{proc.stderr}"
        )
    return proc

def run_inspector(*, method, tool_name=None, tool_args=None, allow_failure=False):
    cmd = [
        "npx",
        "-y",
        "@modelcontextprotocol/inspector",
        "--cli",
        str(atm_bin),
        "serve",
        "--method",
        method,
    ]
    if tool_name:
        cmd.extend(["--tool-name", tool_name])
    if tool_args:
        for k, v in tool_args.items():
            cmd.extend(["--tool-arg", f"{k}={v}"])
    return run(cmd, allow_failure=allow_failure)

def parse_json_output(proc):
    return json.loads(proc.stdout)

def assert_text_payload(payload):
    content = payload.get("content")
    if not isinstance(content, list) or not content:
        raise AssertionError(f"Missing content array: {payload}")
    first = content[0]
    if first.get("type") != "text":
        raise AssertionError(f"Expected text content item: {payload}")
    text = first.get("text")
    if not isinstance(text, str):
        raise AssertionError(f"Expected text payload: {payload}")
    return text

# Baseline reference MCP server.
baseline = run([
    "npx", "-y", "@modelcontextprotocol/inspector", "--cli",
    "npx", "-y", "@modelcontextprotocol/server-everything",
    "--method", "tools/list",
])
if '"name": "echo"' not in baseline.stdout:
    raise AssertionError("Expected echo tool not present in reference MCP server output")

# 1) tools/list and schema checks for 10 ATM tools.
tools = parse_json_output(run_inspector(method="tools/list"))
entries = tools.get("tools")
if not isinstance(entries, list):
    raise AssertionError(f"tools/list missing tools array: {tools}")
expected = {
    "atm_send",
    "atm_read",
    "atm_broadcast",
    "atm_pending_count",
    "agent_sessions",
    "agent_status",
    "agent_close",
    "agent_watch_attach",
    "agent_watch_poll",
    "agent_watch_detach",
}
actual = {e.get("name") for e in entries if isinstance(e, dict)}
missing = expected - actual
if missing:
    raise AssertionError(f"tools/list missing expected ATM tools: {sorted(missing)}")
for e in entries:
    if e.get("name") in expected and "inputSchema" not in e:
        raise AssertionError(f"tool missing inputSchema: {e}")

# 2) tools/call checks for 7 standalone tools.
send = parse_json_output(run_inspector(
    method="tools/call",
    tool_name="atm_send",
    tool_args={"to": "arch-ctm", "message": "q3-smoke-message"},
))
if "Message sent to arch-ctm@atm-dev" not in assert_text_payload(send):
    raise AssertionError(f"atm_send success text mismatch: {send}")

read = parse_json_output(run_inspector(
    method="tools/call",
    tool_name="atm_read",
    tool_args={"all": "true", "mark_read": "false", "limit": "10"},
))
if "q3-smoke-message" not in assert_text_payload(read):
    raise AssertionError(f"atm_read did not include sent message: {read}")

broadcast = parse_json_output(run_inspector(
    method="tools/call",
    tool_name="atm_broadcast",
    tool_args={"message": "q3-broadcast-message"},
))
if "Broadcast sent to" not in assert_text_payload(broadcast):
    raise AssertionError(f"atm_broadcast success text mismatch: {broadcast}")

pending = parse_json_output(run_inspector(method="tools/call", tool_name="atm_pending_count"))
pending_obj = json.loads(assert_text_payload(pending))
if not isinstance(pending_obj.get("unread"), int):
    raise AssertionError(f"atm_pending_count unread must be int: {pending_obj}")

sessions = parse_json_output(run_inspector(method="tools/call", tool_name="agent_sessions"))
sessions_obj = json.loads(assert_text_payload(sessions))
if not isinstance(sessions_obj, list):
    raise AssertionError(f"agent_sessions must return JSON array text: {sessions_obj}")

status = parse_json_output(run_inspector(method="tools/call", tool_name="agent_status"))
status_obj = json.loads(assert_text_payload(status))
for key in ("team", "child_alive", "uptime_secs", "pending_mail_count"):
    if key not in status_obj:
        raise AssertionError(f"agent_status missing key '{key}': {status_obj}")
if status_obj["team"] != "atm-dev":
    raise AssertionError(f"agent_status team mismatch: {status_obj}")

close_proc = run_inspector(
    method="tools/call",
    tool_name="agent_close",
    tool_args={"agent_id": "does-not-exist"},
    allow_failure=True,
)
close_json = None
if close_proc.stdout.strip():
    try:
        close_json = json.loads(close_proc.stdout)
    except json.JSONDecodeError as exc:
        raise AssertionError(
            f"agent_close output must be JSON payload when available; got:\n{close_proc.stdout}\n{close_proc.stderr}"
        ) from exc

if close_json is not None:
    err = close_json.get("error")
    if not isinstance(err, dict):
        raise AssertionError(f"agent_close expected JSON-RPC error payload, got: {close_json}")
    msg = str(err.get("message", ""))
    if "session not found" not in msg:
        raise AssertionError(f"agent_close error message missing not-found text: {close_json}")
else:
    # Fallback when inspector surfaces tool errors via non-zero process path only.
    combined = f"{close_proc.stdout}\n{close_proc.stderr}"
    if "session not found" not in combined:
        raise AssertionError(f"agent_close missing not-found signal: {combined}")

print("MCP Inspector smoke checks passed.")
PY

echo "MCP Inspector smoke suite passed."
