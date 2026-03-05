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


def _write_named_required_agent(workspace: Path, agent_name: str = "scrum-master") -> None:
    agents_dir = workspace / ".claude" / "agents"
    agents_dir.mkdir(parents=True, exist_ok=True)
    body = """---
name: scrum-master
description: test
metadata:
  spawn_policy: named_teammate_required
---
You are scrum-master.
"""
    (agents_dir / f"{agent_name}.md").write_text(body, encoding="utf-8")


def _run_script(
    payload: dict,
    *,
    cwd: Path,
    home: Path,
    extra_env: dict | None = None,
    raw_stdin: bytes | None = None,
):
    env = {
        **os.environ,
        "ATM_HOME": str(home),
        "HOME": str(home),
        "USERPROFILE": str(home),
        "PYTHONPATH": str(SCRIPTS_DIR),
    }
    env.pop("ATM_IDENTITY", None)
    env.pop("ATM_TEAM", None)
    if extra_env:
        env.update(extra_env)
    return subprocess.run(
        [sys.executable, str(SCRIPT_PATH)],
        input=raw_stdin if raw_stdin is not None else json.dumps(payload).encode(),
        capture_output=True,
        cwd=cwd,
        env=env,
    )


def _spawn_payload(
    session_id: str,
    *,
    team_name: str | None = "atm-dev",
    teammate_name: str | None = "worker-1",
    subagent_type: str = "general-purpose",
) -> dict:
    tool_input: dict[str, str] = {"subagent_type": subagent_type}
    if teammate_name is not None:
        tool_input["name"] = teammate_name
    if team_name is not None:
        tool_input["team_name"] = team_name
    return {"session_id": session_id, "tool_input": tool_input}


def _leaders_only_toml() -> str:
    return (
        """
[core]
default_team = "atm-dev"

[team."atm-dev"]
spawn_policy = "leaders-only"
co_leaders = ["arch-atm"]
""".strip()
        + "\n"
    )


def test_leaders_only_allows_team_lead(tmp_path: Path) -> None:
    workspace = tmp_path / "workspace"
    workspace.mkdir()
    _write_atm_toml(workspace, _leaders_only_toml())
    _write_team_config(tmp_path / "home", "atm-dev")

    result = _run_script(_spawn_payload("lead-sess"), cwd=workspace, home=tmp_path / "home")
    assert result.returncode == 0, result.stderr.decode()


def test_leaders_only_allows_co_leader(tmp_path: Path) -> None:
    workspace = tmp_path / "workspace"
    workspace.mkdir()
    _write_atm_toml(workspace, _leaders_only_toml())
    _write_team_config(tmp_path / "home", "atm-dev")

    result = _run_script(_spawn_payload("co-sess"), cwd=workspace, home=tmp_path / "home")
    assert result.returncode == 0, result.stderr.decode()


def test_leaders_only_blocks_non_leader_with_spawn_unauthorized(tmp_path: Path) -> None:
    workspace = tmp_path / "workspace"
    workspace.mkdir()
    _write_atm_toml(workspace, _leaders_only_toml())
    _write_team_config(tmp_path / "home", "atm-dev")

    result = _run_script(_spawn_payload("dev-sess"), cwd=workspace, home=tmp_path / "home")
    assert result.returncode == 2
    assert "SPAWN_UNAUTHORIZED" in result.stderr.decode()


def test_named_spawn_without_team_name_still_enforces_rule2(tmp_path: Path) -> None:
    workspace = tmp_path / "workspace"
    workspace.mkdir()
    _write_atm_toml(workspace, _leaders_only_toml())
    _write_team_config(tmp_path / "home", "atm-dev")

    result = _run_script(
        _spawn_payload("dev-sess", team_name=None, teammate_name="sm-sprint-A"),
        cwd=workspace,
        home=tmp_path / "home",
    )
    assert result.returncode == 2
    assert "SPAWN_UNAUTHORIZED" in result.stderr.decode()


def test_named_spawn_without_team_name_allows_team_lead(tmp_path: Path) -> None:
    workspace = tmp_path / "workspace"
    workspace.mkdir()
    _write_atm_toml(workspace, _leaders_only_toml())
    _write_team_config(tmp_path / "home", "atm-dev")

    result = _run_script(
        _spawn_payload("lead-sess", team_name=None, teammate_name="sm-sprint-B"),
        cwd=workspace,
        home=tmp_path / "home",
    )
    assert result.returncode == 0, result.stderr.decode()


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

    result = _run_script(_spawn_payload("dev-sess"), cwd=workspace, home=tmp_path / "home")
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

    result = _run_script(_spawn_payload("dev-sess"), cwd=workspace, home=tmp_path / "home")
    assert result.returncode == 0, result.stderr.decode()


def test_rule1_blocks_named_required_without_name(tmp_path: Path) -> None:
    workspace = tmp_path / "workspace"
    workspace.mkdir()
    _write_atm_toml(workspace, _leaders_only_toml())
    _write_team_config(tmp_path / "home", "atm-dev")
    _write_named_required_agent(workspace)

    result = _run_script(
        _spawn_payload(
            "lead-sess", subagent_type="scrum-master", teammate_name=None, team_name="atm-dev"
        ),
        cwd=workspace,
        home=tmp_path / "home",
        extra_env={"CLAUDE_PROJECT_DIR": str(workspace)},
    )
    assert result.returncode == 2
    assert "requires named teammate spawn policy" in result.stderr.decode()


def test_rule1_allows_named_required_with_name(tmp_path: Path) -> None:
    workspace = tmp_path / "workspace"
    workspace.mkdir()
    _write_atm_toml(workspace, _leaders_only_toml())
    _write_team_config(tmp_path / "home", "atm-dev")
    _write_named_required_agent(workspace)

    result = _run_script(
        _spawn_payload(
            "lead-sess",
            subagent_type="scrum-master",
            teammate_name="sm-sprint-B",
            team_name="atm-dev",
        ),
        cwd=workspace,
        home=tmp_path / "home",
        extra_env={"CLAUDE_PROJECT_DIR": str(workspace)},
    )
    assert result.returncode == 0, result.stderr.decode()


def test_rule3_blocks_team_name_mismatch(tmp_path: Path) -> None:
    workspace = tmp_path / "workspace"
    workspace.mkdir()
    _write_atm_toml(workspace, _leaders_only_toml())
    _write_team_config(tmp_path / "home", "atm-dev")

    result = _run_script(
        _spawn_payload("lead-sess", team_name="wrong-team"),
        cwd=workspace,
        home=tmp_path / "home",
    )
    assert result.returncode == 2
    assert "team_name must match .atm.toml core.default_team" in result.stderr.decode()


def test_fail_open_on_invalid_json_input(tmp_path: Path) -> None:
    workspace = tmp_path / "workspace"
    workspace.mkdir()
    result = _run_script(
        _spawn_payload("lead-sess"),
        cwd=workspace,
        home=tmp_path / "home",
        raw_stdin=b"{invalid-json",
    )
    assert result.returncode == 0, result.stderr.decode()


def test_fail_open_when_agent_prompt_missing(tmp_path: Path) -> None:
    workspace = tmp_path / "workspace"
    workspace.mkdir()
    _write_atm_toml(workspace, _leaders_only_toml())
    _write_team_config(tmp_path / "home", "atm-dev")

    # No .claude/agents/scrum-master.md exists; rule-1 parse should fail-open.
    result = _run_script(
        _spawn_payload(
            "lead-sess",
            subagent_type="scrum-master",
            teammate_name="sm-sprint-C",
            team_name="atm-dev",
        ),
        cwd=workspace,
        home=tmp_path / "home",
        extra_env={"CLAUDE_PROJECT_DIR": str(workspace)},
    )
    assert result.returncode == 0, result.stderr.decode()


def test_env_identity_overrides_session_mapping(tmp_path: Path) -> None:
    workspace = tmp_path / "workspace"
    workspace.mkdir()
    _write_atm_toml(workspace, _leaders_only_toml())
    _write_team_config(tmp_path / "home", "atm-dev")

    # Session belongs to dev-sess, but ATM_IDENTITY override should win.
    result = _run_script(
        _spawn_payload("dev-sess"),
        cwd=workspace,
        home=tmp_path / "home",
        extra_env={"ATM_IDENTITY": "arch-atm"},
    )
    assert result.returncode == 0, result.stderr.decode()


def test_load_team_config_prefers_atm_home_over_home(tmp_path: Path) -> None:
    workspace = tmp_path / "workspace"
    workspace.mkdir()
    _write_atm_toml(workspace, _leaders_only_toml())

    atm_home = tmp_path / "atm-home"
    wrong_home = tmp_path / "wrong-home"
    _write_team_config(atm_home, "atm-dev")
    wrong_home.mkdir(parents=True, exist_ok=True)

    result = _run_script(
        _spawn_payload("lead-sess"),
        cwd=workspace,
        home=atm_home,
        extra_env={"HOME": str(wrong_home), "USERPROFILE": str(wrong_home)},
    )
    assert result.returncode == 0, result.stderr.decode()
