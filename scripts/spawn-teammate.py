#!/usr/bin/env python3
"""Spawn a Claude Code teammate in a new tmux pane.

Python replacement for scripts/spawn-teammate.sh.
"""

from __future__ import annotations

import argparse
import json
import os
import re
import shlex
import subprocess
import sys
import time
from pathlib import Path
from typing import Any


def _parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser(
        description="Spawn a Claude Code teammate in a new tmux pane."
    )
    parser.add_argument("agent_name")
    parser.add_argument("team_name")
    parser.add_argument("color", nargs="?", default="")
    parser.add_argument("--model", default="")
    parser.add_argument("--repo-root", default="")
    return parser.parse_args()


def _repo_root(explicit: str) -> Path:
    script_root = Path(__file__).resolve().parent.parent
    env_override = os.environ.get("SPAWN_REPO_ROOT", "").strip()
    if env_override:
        return Path(env_override).expanduser().resolve()
    if explicit:
        return Path(explicit).expanduser().resolve()
    return script_root


def _is_atm_context(repo_root: Path) -> bool:
    if os.environ.get("ATM_TEAM", "").strip():
        return True
    if os.environ.get("ATM_IDENTITY", "").strip():
        return True
    return (repo_root / ".atm.toml").exists()


def _load_repo_env(repo_root: Path) -> None:
    env_file = repo_root / ".env"
    if not env_file.exists():
        return

    pattern = re.compile(r"^([A-Za-z_][A-Za-z0-9_]*)=(.*)$")
    for raw_line in env_file.read_text(encoding="utf-8").splitlines():
        line = raw_line.split("#", 1)[0].strip()
        if not line:
            continue
        match = pattern.match(line)
        if not match:
            continue
        key, value = match.groups()
        if len(value) >= 2 and value[0] == value[-1] and value[0] in {"'", '"'}:
            value = value[1:-1]
        os.environ[key] = os.path.expandvars(value)


def _shell_prefix(repo_root: Path) -> str:
    env_file = repo_root / ".env"
    parts = [f"cd {shlex.quote(str(repo_root))}"]
    if env_file.exists():
        parts.extend(
            [
                "set -a",
                f"source {shlex.quote(str(env_file))}",
                "set +a",
                "hash -r",
            ]
        )
    return "; ".join(parts)


def _extract_frontmatter(agent_file: Path) -> tuple[str, str, str]:
    if not agent_file.exists():
        return "", "", ""
    text = agent_file.read_text(encoding="utf-8")
    match = re.match(r"^---\n(.*?)\n---\n?(.*)$", text, re.DOTALL)
    if not match:
        return "", "", text.strip()
    frontmatter, body = match.groups()
    model = ""
    color = ""
    for line in frontmatter.splitlines():
        if line.startswith("model:"):
            model = line.split(":", 1)[1].strip()
        elif line.startswith("color:"):
            color = line.split(":", 1)[1].strip()
    return model, color, body.strip()


def _find_claude_binary() -> Path:
    versions_dir = Path.home() / ".local" / "share" / "claude" / "versions"
    if not versions_dir.exists():
        raise RuntimeError(f"Could not find Claude versions at {versions_dir}")
    candidates = [p for p in versions_dir.iterdir() if p.is_file() and re.match(r"^[0-9]", p.name)]
    if not candidates:
        raise RuntimeError(f"Could not find claude binary in {versions_dir}")
    return max(candidates, key=lambda p: p.stat().st_mtime)


def _read_lead_session_id(team_name: str) -> str:
    config = Path.home() / ".claude" / "teams" / team_name / "config.json"
    if not config.exists():
        return ""
    try:
        payload = json.loads(config.read_text(encoding="utf-8"))
        value = payload.get("leadSessionId", "")
        return value.strip() if isinstance(value, str) else ""
    except Exception:
        return ""


def _run(command: list[str], *, check: bool = True, capture: bool = False) -> subprocess.CompletedProcess[str]:
    return subprocess.run(
        command,
        check=check,
        text=True,
        capture_output=capture,
    )


def _shell_quote(value: str) -> str:
    return shlex.quote(value)


def main() -> int:
    args = _parse_args()
    repo_root = _repo_root(args.repo_root)
    _load_repo_env(repo_root)
    if not _is_atm_context(repo_root):
        return 0

    agent_name = os.environ.get("ATM_IDENTITY", args.agent_name)
    team_name = os.environ.get("ATM_TEAM", args.team_name)

    agent_file = repo_root / ".claude" / "agents" / f"{agent_name}.md"
    fm_model, fm_color, prompt_body = _extract_frontmatter(agent_file)

    model = args.model or fm_model or "sonnet"
    color = args.color or fm_color or "cyan"

    try:
        claude_bin = _find_claude_binary()
    except RuntimeError as exc:
        print(f"ERROR: {exc}", file=sys.stderr)
        return 1

    parent_session_id = os.environ.get("CLAUDE_SESSION_ID", "").strip() or _read_lead_session_id(team_name)
    agent_id = f"{agent_name}@{team_name}"
    shell_prefix = _shell_prefix(repo_root)

    print(f"Spawning '{agent_name}' in team '{team_name}' (color={color}, model={model})")
    print(f"Binary:     {claude_bin}")
    print(f"Repo root:  {repo_root}")
    print(f"Session ID: {parent_session_id or '<not found>'}")

    _run(["atm", "teams", "add-member", team_name, agent_name, "--agent-type", agent_name])

    cmd_parts = [
        shell_prefix,
        "export CLAUDECODE=1",
        "export CLAUDE_CODE_EXPERIMENTAL_AGENT_TEAMS=1",
        f"export ATM_IDENTITY={_shell_quote(agent_name)}",
        f"export ATM_TEAM={_shell_quote(team_name)}",
        _shell_quote(str(claude_bin)),
        "--agent-id",
        _shell_quote(agent_id),
        "--agent-name",
        _shell_quote(agent_name),
        "--team-name",
        _shell_quote(team_name),
        "--agent-color",
        _shell_quote(color),
        "--agent-type",
        _shell_quote(agent_name),
        "--model",
        _shell_quote(model),
        "--dangerously-skip-permissions",
    ]
    if parent_session_id:
        cmd_parts.extend(["--parent-session-id", _shell_quote(parent_session_id)])
    tmux_cmd = "; ".join(cmd_parts[:5]) + "; " + " ".join(cmd_parts[5:])

    pane = _run(
        ["tmux", "split-window", "-h", "-P", "-F", "#{pane_id}", f"zsh -lc {shlex.quote(tmux_cmd)}; exec zsh"],
        capture=True,
    ).stdout.strip()

    print(f"Spawned {agent_name} in pane {pane}")

    _run(
        [
            "atm",
            "teams",
            "add-member",
            team_name,
            agent_name,
            "--agent-type",
            agent_name,
            "--pane-id",
            pane,
        ]
    )

    if prompt_body:
        time.sleep(3)
        print(f"Sending agent prompt from {agent_file}...")
        _run(["atm", "send", agent_name, prompt_body, "--team", team_name])

    return 0


if __name__ == "__main__":
    raise SystemExit(main())
