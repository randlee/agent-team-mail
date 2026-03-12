"""Tests for Codex notify relay script parity and behavior."""

from __future__ import annotations

import json
import os
import subprocess
import sys
from pathlib import Path

import pytest

_REPO_ROOT = Path(__file__).resolve().parents[2]
_SCRIPT_ROOTS = [
    _REPO_ROOT / ".claude" / "scripts",
    _REPO_ROOT / "crates" / "atm" / "scripts",
]


def _run(
    script_path: Path,
    payload: dict,
    *,
    env_overrides: dict[str, str] | None = None,
    set_atm_home: bool = True,
    toml_content: str | None = None,
    cwd: Path,
) -> tuple[int, str, str]:
    env = os.environ.copy()
    if set_atm_home:
        env.setdefault("ATM_HOME", str(cwd))
        env["ATM_HOME"] = str(cwd)
    else:
        env.pop("ATM_HOME", None)
    env["ATM_TEAM"] = ""
    env["ATM_IDENTITY"] = ""
    if env_overrides:
        env.update(env_overrides)

    if toml_content is not None:
        (cwd / ".atm.toml").write_text(toml_content, encoding="utf-8")

    proc = subprocess.run(
        [sys.executable, str(script_path), json.dumps(payload)],
        cwd=str(cwd),
        env=env,
        text=True,
        capture_output=True,
        check=False,
    )
    return proc.returncode, proc.stdout, proc.stderr


def _read_events(cwd: Path) -> list[dict]:
    events_file = cwd / ".atm" / "daemon" / "hooks" / "events.jsonl"
    if not events_file.exists():
        return []
    return [json.loads(line) for line in events_file.read_text(encoding="utf-8").splitlines() if line.strip()]


def _read_events_at(path: Path) -> list[dict]:
    events_file = path / ".atm" / "daemon" / "hooks" / "events.jsonl"
    if not events_file.exists():
        return []
    return [json.loads(line) for line in events_file.read_text(encoding="utf-8").splitlines() if line.strip()]


@pytest.mark.parametrize("scripts_dir", _SCRIPT_ROOTS)
def test_relay_uses_toml_context(tmp_path: Path, scripts_dir: Path):
    script_path = scripts_dir / "atm-hook-relay.py"
    assert script_path.exists()

    payload = {"type": "agent-turn-complete", "turn-id": "turn-1", "thread-id": "thread-a"}
    toml = '[core]\ndefault_team = "atm-dev"\nidentity = "arch-ctm"\n'
    rc, _stdout, _stderr = _run(script_path, payload, toml_content=toml, cwd=tmp_path)

    assert rc == 0
    events = _read_events(tmp_path)
    assert len(events) == 1
    event = events[0]
    assert event["type"] == "agent-turn-complete"
    assert event["team"] == "atm-dev"
    assert event["agent"] == "arch-ctm"
    assert event["thread-id"] == "thread-a"
    assert event["turn-id"] == "turn-1"
    assert event["state"] == "idle"
    assert event["idempotency_key"] == "atm-dev:arch-ctm:turn-1"


@pytest.mark.parametrize("scripts_dir", _SCRIPT_ROOTS)
def test_relay_uses_env_context_without_toml(tmp_path: Path, scripts_dir: Path):
    script_path = scripts_dir / "atm-hook-relay.py"
    payload = {"type": "agent-turn-complete", "turn-id": "turn-2"}
    rc, _stdout, _stderr = _run(
        script_path,
        payload,
        env_overrides={"ATM_TEAM": "env-team", "ATM_IDENTITY": "env-agent"},
        toml_content=None,
        cwd=tmp_path,
    )

    assert rc == 0
    events = _read_events(tmp_path)
    assert len(events) == 1
    event = events[0]
    assert event["team"] == "env-team"
    assert event["agent"] == "env-agent"
    assert event["idempotency_key"] == "env-team:env-agent:turn-2"


@pytest.mark.parametrize("scripts_dir", _SCRIPT_ROOTS)
def test_relay_no_context_noop(tmp_path: Path, scripts_dir: Path):
    script_path = scripts_dir / "atm-hook-relay.py"
    payload = {"type": "agent-turn-complete", "turn-id": "turn-3"}
    rc, _stdout, _stderr = _run(
        script_path,
        payload,
        env_overrides={"ATM_TEAM": "", "ATM_IDENTITY": ""},
        toml_content=None,
        cwd=tmp_path,
    )

    assert rc == 0
    assert _read_events(tmp_path) == []


@pytest.mark.parametrize("scripts_dir", _SCRIPT_ROOTS)
def test_relay_uses_os_home_when_atm_home_unset(tmp_path: Path, scripts_dir: Path, monkeypatch: pytest.MonkeyPatch):
    script_path = scripts_dir / "atm-hook-relay.py"
    monkeypatch.setenv("HOME", str(tmp_path))

    payload = {"type": "agent-turn-complete", "turn-id": "turn-redirected"}
    rc, _stdout, _stderr = _run(
        script_path,
        payload,
        env_overrides={"ATM_TEAM": "atm-dev", "ATM_IDENTITY": "arch-ctm"},
        set_atm_home=False,
        toml_content=None,
        cwd=tmp_path,
    )

    assert rc == 0
    events = _read_events_at(tmp_path)
    assert len(events) == 1
    assert events[0]["idempotency_key"] == "atm-dev:arch-ctm:turn-redirected"
