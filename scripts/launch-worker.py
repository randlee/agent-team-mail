#!/usr/bin/env python3
"""Launch an ATM worker agent in tmux.

Python replacement for scripts/launch-worker.sh.
"""

from __future__ import annotations

import argparse
import os
import shutil
import subprocess
import sys
from pathlib import Path


def _parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser(description="Launch an ATM worker in tmux")
    parser.add_argument("agent_name")
    parser.add_argument("command", nargs="?", default="codex --yolo")
    return parser.parse_args()


def _run(cmd: list[str], *, check: bool = True, capture: bool = False) -> subprocess.CompletedProcess[str]:
    return subprocess.run(cmd, check=check, text=True, capture_output=capture)


def _read_default_team(atm_toml: Path) -> str:
    try:
        import tomllib
    except ImportError:  # pragma: no cover
        import tomli as tomllib  # type: ignore[no-redef]

    with atm_toml.open("rb") as f:
        parsed = tomllib.load(f)
    team = parsed.get("core", {}).get("default_team")
    if not isinstance(team, str) or not team.strip():
        raise ValueError("[core].default_team is missing or empty")
    return team.strip()


def _check_command_available(command: str) -> None:
    base = command.split()[0]
    if shutil.which(base) is None:
        raise RuntimeError(f"'{base}' is not installed or not in PATH")


def main() -> int:
    args = _parse_args()
    mode = os.environ.get("LAUNCH_MODE", "session").strip() or "session"

    repo_root = Path(__file__).resolve().parent.parent
    atm_toml = repo_root / ".atm.toml"
    if not atm_toml.exists():
        print(f"Error: .atm.toml not found at {atm_toml}", file=sys.stderr)
        print("Set up [core].default_team in repo .atm.toml before launching workers.", file=sys.stderr)
        return 1

    try:
        default_team = _read_default_team(atm_toml)
    except Exception as exc:
        print(f"Error: failed to read [core].default_team from {atm_toml}: {exc}", file=sys.stderr)
        return 1

    team = os.environ.get("ATM_TEAM", "").strip() or default_team
    agent_name = args.agent_name
    worker_cmd = args.command

    if shutil.which("tmux") is None:
        print("Error: tmux is not installed or not in PATH", file=sys.stderr)
        return 1

    try:
        _check_command_available(worker_cmd)
    except RuntimeError as exc:
        print(f"Error: {exc}", file=sys.stderr)
        return 1

    if "codex" in worker_cmd:
        codex_config = Path.home() / ".codex" / "config.toml"
        if not codex_config.exists() or "notify" not in codex_config.read_text(encoding="utf-8", errors="ignore"):
            print("WARNING: Codex notify hook not configured.")
            print("Run: atm init <team> to auto-install runtime hook wiring.")
            print()

    env_prefix = [f"ATM_IDENTITY={agent_name}", f"ATM_TEAM={team}"]
    if os.environ.get("ATM_HOME", "").strip():
        env_prefix.append(f"ATM_HOME={os.environ['ATM_HOME'].strip()}")
    env_str = " ".join(env_prefix)

    if mode == "pane":
        if not os.environ.get("TMUX"):
            print("Error: LAUNCH_MODE=pane requires an active tmux session.", file=sys.stderr)
            return 1

        pane = _run(
            [
                "tmux",
                "split-window",
                "-h",
                "-P",
                "-F",
                "#{pane_id}",
                f"env {env_str} {worker_cmd}; echo ''; echo 'Worker exited. Press Enter to close.'; read",
            ],
            capture=True,
        ).stdout.strip()
        _run(["tmux", "select-layout", "even-horizontal"])

        _run(
            ["atm", "teams", "add-member", team, agent_name, "--pane-id", pane],
            check=False,
        )

        print(f"Launched worker '{agent_name}' in new pane.")
        print()
        print(f"  Pane:      {pane} (current window)")
        print(f"  Identity:  ATM_IDENTITY={agent_name}")
        print(f"  Team:      ATM_TEAM={team}")
        print(f"  Command:   {worker_cmd}")
        print()
        print(f"  Send keys: tmux send-keys -t {pane} 'your message' Enter")
        return 0

    # Default: separate tmux session
    existing = _run(["tmux", "has-session", "-t", agent_name], check=False)
    if existing.returncode == 0:
        print(f"tmux session '{agent_name}' already exists.")
        print()
        print(f"  Attach:  tmux attach -t {agent_name}")
        print(f"  Kill:    tmux kill-session -t {agent_name}")
        print()
        answer = input("Attach to existing session? [Y/n] ").strip() or "Y"
        if answer.lower().startswith("n"):
            print("Aborted.")
            return 0
        os.execvp("tmux", ["tmux", "attach", "-t", agent_name])

    _run(
        [
            "tmux",
            "new-session",
            "-d",
            "-s",
            agent_name,
            f"env {env_str} {worker_cmd}; echo ''; echo 'Worker exited. Press Enter to close.'; read",
        ]
    )
    pane_info = _run(
        ["tmux", "list-panes", "-t", agent_name, "-F", "#{session_name}:#{window_index}.#{pane_index} (pid #{pane_pid})"],
        capture=True,
    ).stdout.strip()

    print(f"Launched worker '{agent_name}' in tmux session.")
    print()
    print(f"  Session:   {agent_name}")
    print(f"  Identity:  ATM_IDENTITY={agent_name}")
    print(f"  Team:      ATM_TEAM={team}")
    print(f"  Command:   {worker_cmd}")
    print(f"  Pane:      {pane_info}")
    print()
    print(f"  Attach:    tmux attach -t {agent_name}")
    print(f"  Send keys: tmux send-keys -t {agent_name} 'your message' Enter")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
