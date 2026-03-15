#!/usr/bin/env python3
"""Launch an ATM worker agent in tmux.

Python replacement for scripts/launch-worker.sh.
"""

from __future__ import annotations

import argparse
import os
import re
import shutil
import shlex
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


def _repo_root() -> Path:
    override = os.environ.get("LAUNCH_REPO_ROOT", "").strip()
    if override:
        return Path(override).expanduser().resolve()
    return Path(__file__).resolve().parent.parent


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
        expanded = os.path.expandvars(value)
        os.environ[key] = expanded


def _pane_shell_prefix(repo_root: Path) -> str:
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

    repo_root = _repo_root()
    _load_repo_env(repo_root)
    atm_toml = repo_root / ".atm.toml"
    if not atm_toml.exists():
        return 0

    try:
        default_team = _read_default_team(atm_toml)
    except Exception as exc:
        print(f"Error: failed to read [core].default_team from {atm_toml}: {exc}", file=sys.stderr)
        return 1

    team = os.environ.get("ATM_TEAM", "").strip() or default_team
    agent_name = args.agent_name
    worker_cmd = args.command
    shell_prefix = _pane_shell_prefix(repo_root)

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
    pane_cmd = (
        f"zsh -lc {shlex.quote(f'{shell_prefix}; export {env_str}; {worker_cmd}')}"
    )

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
                f"{pane_cmd}; echo ''; echo 'Worker exited. Press Enter to close.'; read",
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
            f"{pane_cmd}; echo ''; echo 'Worker exited. Press Enter to close.'; read",
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
