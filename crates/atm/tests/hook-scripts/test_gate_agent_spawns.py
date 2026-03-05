"""Tests for .claude/scripts/gate-agent-spawns.py (PreToolUse Task hook)."""

import json
import os
import subprocess
import sys
from pathlib import Path


# Navigate: tests/hook-scripts -> tests -> atm -> crates -> repo root
REPO_ROOT = Path(__file__).parents[4]
SCRIPT_PATH = REPO_ROOT / ".claude" / "scripts" / "gate-agent-spawns.py"
SCRIPTS_DIR = SCRIPT_PATH.parent


def _write_team_config(home: Path, team: str) -> None:
    team_dir = home / ".claude" / "teams" / team
    team_dir.mkdir(parents=True, exist_ok=True)
    config = {
        "name": team,
        "leadAgentId": f"team-lead@{team}",
        "leadSessionId": "lead-sess",
        "members": [
            {
                "name": "team-lead",
                "agentId": f"team-lead@{team}",
                "sessionId": "lead-sess",
            },
            {
                "name": "arch-atm",
                "agentId": f"arch-atm@{team}",
                "sessionId": "co-sess",
            },
            {
                "name": "dev-1",
                "agentId": f"dev-1@{team}",
                "sessionId": "dev-sess",
            },
        ],
    }
    (team_dir / "config.json").write_text(json.dumps(config), encoding="utf-8")


def _write_atm_toml(workspace: Path, body: str) -> None:
    (workspace / ".atm.toml").write_text(body, encoding="utf-8")


def _run_script(payload: dict, *, cwd: Path, home: Path, extra_env: dict | None = None):
    env = {
        **os.environ,
        "HOME": str(home),
        "PYTHONPATH": str(SCRIPTS_DIR),
    }
    env.pop("ATM_IDENTITY", None)
    env.pop("ATM_TEAM", None)
    if extra_env:
        env.update(extra_env)
    return subprocess.run(
        [sys.executable, str(SCRIPT_PATH)],
        input=json.dumps(payload).encode(),
        capture_output=True,
        cwd=cwd,
        env=env,
    )


def _spawn_payload(session_id: str, team_name: str = "atm-dev") -> dict:
    return {
        "session_id": session_id,
        "tool_input": {
            "subagent_type": "general-purpose",
            "name": "worker-1",
            "team_name": team_name,
        },
    }


def test_leaders_only_allows_team_lead(tmp_path: Path) -> None:
    workspace = tmp_path / "workspace"
    workspace.mkdir()
    _write_atm_toml(
        workspace,
        """
[core]
default_team = "atm-dev"

[team."atm-dev"]
spawn_policy = "leaders-only"
co_leaders = ["arch-atm"]
""".strip()
        + "\n",
    )
    _write_team_config(tmp_path / "home", "atm-dev")

    result = _run_script(
        _spawn_payload("lead-sess"), cwd=workspace, home=tmp_path / "home"
    )
    assert result.returncode == 0, result.stderr.decode()


def test_leaders_only_allows_co_leader(tmp_path: Path) -> None:
    workspace = tmp_path / "workspace"
    workspace.mkdir()
    _write_atm_toml(
        workspace,
        """
[core]
default_team = "atm-dev"

[team."atm-dev"]
spawn_policy = "leaders-only"
co_leaders = ["arch-atm"]
""".strip()
        + "\n",
    )
    _write_team_config(tmp_path / "home", "atm-dev")

    result = _run_script(_spawn_payload("co-sess"), cwd=workspace, home=tmp_path / "home")
    assert result.returncode == 0, result.stderr.decode()


def test_leaders_only_blocks_non_leader_with_spawn_unauthorized(tmp_path: Path) -> None:
    workspace = tmp_path / "workspace"
    workspace.mkdir()
    _write_atm_toml(
        workspace,
        """
[core]
default_team = "atm-dev"

[team."atm-dev"]
spawn_policy = "leaders-only"
co_leaders = ["arch-atm"]
""".strip()
        + "\n",
    )
    _write_team_config(tmp_path / "home", "atm-dev")

    result = _run_script(
        _spawn_payload("dev-sess"), cwd=workspace, home=tmp_path / "home"
    )
    assert result.returncode == 2
    assert "SPAWN_UNAUTHORIZED" in result.stderr.decode()


def test_missing_team_section_defaults_to_leaders_only(tmp_path: Path) -> None:
    workspace = tmp_path / "workspace"
    workspace.mkdir()
    _write_atm_toml(
        workspace,
        """
[core]
default_team = "atm-dev"
""".strip()
        + "\n",
    )
    _write_team_config(tmp_path / "home", "atm-dev")

    result = _run_script(
        _spawn_payload("dev-sess"), cwd=workspace, home=tmp_path / "home"
    )
    assert result.returncode == 2
    assert "SPAWN_UNAUTHORIZED" in result.stderr.decode()


def test_any_member_policy_allows_non_leader(tmp_path: Path) -> None:
    workspace = tmp_path / "workspace"
    workspace.mkdir()
    _write_atm_toml(
        workspace,
        """
[core]
default_team = "atm-dev"

[team."atm-dev"]
spawn_policy = "any-member"
""".strip()
        + "\n",
    )
    _write_team_config(tmp_path / "home", "atm-dev")

    result = _run_script(
        _spawn_payload("dev-sess"), cwd=workspace, home=tmp_path / "home"
    )
    assert result.returncode == 0, result.stderr.decode()
