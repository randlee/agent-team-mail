"""Parity tests for hook relay scripts in local and embedded install paths."""

from __future__ import annotations

import importlib.util
import json
import os
import platform
import sys
import tempfile
from io import StringIO
from pathlib import Path
from unittest.mock import MagicMock, patch

import pytest

_REPO_ROOT = Path(__file__).resolve().parents[2]
_SCRIPT_ROOTS = [
    _REPO_ROOT / ".claude" / "scripts",
    _REPO_ROOT / "crates" / "atm" / "scripts",
]

_RELAYS = [
    ("permission-request-relay.py", "permission_request"),
    ("stop-relay.py", "stop"),
    ("notification-idle-relay.py", "notification_idle_prompt"),
]

_PARITY_SET = [
    "session-start.py",
    "session-end.py",
    "permission-request-relay.py",
    "stop-relay.py",
    "notification-idle-relay.py",
    "atm_hook_lib.py",
]



def _load_module(script_path: Path):
    module_name = f"{script_path.stem.replace('-', '_')}_{abs(hash(str(script_path)))}"
    spec = importlib.util.spec_from_file_location(module_name, script_path)
    mod = importlib.util.module_from_spec(spec)  # type: ignore[arg-type]
    assert spec.loader is not None
    spec.loader.exec_module(mod)  # type: ignore[union-attr]
    return mod



def _run_script(
    script_path: Path,
    payload: dict,
    *,
    toml_content: str | None = None,
    env_overrides: dict[str, str] | None = None,
    socket_file_exists: bool = True,
    parent_pid: int = 4242,
) -> tuple[int, list[bytes]]:
    send_calls: list[bytes] = []

    def capture_send(data: bytes) -> None:
        send_calls.append(data)

    with tempfile.TemporaryDirectory() as tmpdir:
        run_dir = Path(tmpdir)
        if toml_content is not None:
            (run_dir / ".atm.toml").write_text(toml_content)

        atm_home = run_dir
        daemon_dir = atm_home / ".claude" / "daemon"
        daemon_dir.mkdir(parents=True, exist_ok=True)
        if socket_file_exists:
            (daemon_dir / "atm-daemon.sock").touch()
            if platform.system() == "Windows":
                (daemon_dir / "atm-daemon.port").write_text("12345")

        env = {
            "ATM_HOME": str(atm_home),
            "ATM_TEAM": "",
            "ATM_IDENTITY": "",
        }
        if env_overrides:
            env.update(env_overrides)

        mock_sock = MagicMock()
        mock_sock.__enter__ = MagicMock(return_value=mock_sock)
        mock_sock.__exit__ = MagicMock(return_value=False)
        mock_sock.recv.return_value = b'{"status":"ok"}'
        mock_sock.sendall.side_effect = capture_send

        original_cwd = os.getcwd()
        try:
            os.chdir(run_dir)
            with patch("sys.stdin", StringIO(json.dumps(payload))), patch.dict(
                os.environ, env
            ), patch("socket.socket", return_value=mock_sock), patch(
                "os.getppid", return_value=parent_pid
            ):
                mod = _load_module(script_path)
                rc = mod.main()
        finally:
            os.chdir(original_cwd)

    return rc, send_calls


@pytest.mark.parametrize("scripts_dir", _SCRIPT_ROOTS)
@pytest.mark.parametrize("script_name,event_name", _RELAYS)
def test_relay_scripts_send_expected_event_with_toml(
    scripts_dir: Path, script_name: str, event_name: str
):
    script_path = scripts_dir / script_name
    assert script_path.exists(), f"missing script: {script_path}"

    payload = {
        "session_id": "sess-1",
        "tool_name": "Bash",
        "tool_input": {"name": "Bash"},
    }
    toml = '[core]\ndefault_team = "atm-dev"\nidentity = "arch-ctm"\n'

    rc, calls = _run_script(script_path, payload, toml_content=toml)

    assert rc == 0
    assert len(calls) == 1
    request = json.loads(calls[0].decode().strip())
    assert request["command"] == "hook-event"
    hook_payload = request["payload"]
    assert hook_payload["event"] == event_name
    assert hook_payload["team"] == "atm-dev"
    assert hook_payload["agent"] == "arch-ctm"
    assert hook_payload["session_id"] == "sess-1"
    assert hook_payload["process_id"] == 4242
    assert hook_payload["source"]["kind"] == "claude_hook"

    if script_name == "permission-request-relay.py":
        assert hook_payload["tool_name"] == "Bash"


@pytest.mark.parametrize("scripts_dir", _SCRIPT_ROOTS)
@pytest.mark.parametrize("script_name,_", _RELAYS)
def test_relay_scripts_no_context_noop(scripts_dir: Path, script_name: str, _: str):
    script_path = scripts_dir / script_name
    rc, calls = _run_script(
        script_path,
        {"session_id": "sess-2"},
        toml_content=None,
        env_overrides={"ATM_TEAM": "", "ATM_IDENTITY": ""},
    )

    assert rc == 0
    assert calls == []


@pytest.mark.parametrize("scripts_dir", _SCRIPT_ROOTS)
@pytest.mark.parametrize("script_name,event_name", _RELAYS)
def test_relay_scripts_env_only_context_supported(
    scripts_dir: Path, script_name: str, event_name: str
):
    script_path = scripts_dir / script_name
    rc, calls = _run_script(
        script_path,
        {"session_id": "sess-3"},
        toml_content=None,
        env_overrides={"ATM_TEAM": "env-team", "ATM_IDENTITY": "env-agent"},
    )

    assert rc == 0
    assert len(calls) == 1
    request = json.loads(calls[0].decode().strip())
    hook_payload = request["payload"]
    assert hook_payload["event"] == event_name
    assert hook_payload["team"] == "env-team"
    assert hook_payload["agent"] == "env-agent"
    assert hook_payload["session_id"] == "sess-3"


def test_parity_set_exists_in_both_script_roots():
    """All required AC.6 parity scripts exist in both local and embedded roots."""
    for script_name in _PARITY_SET:
        for scripts_dir in _SCRIPT_ROOTS:
            script_path = scripts_dir / script_name
            assert script_path.exists(), f"missing parity script: {script_path}"


@pytest.mark.parametrize("script_name", _PARITY_SET)
def test_parity_set_is_byte_identical_between_roots(script_name: str):
    """Required parity scripts must be byte-identical across both install roots."""
    local_path = _SCRIPT_ROOTS[0] / script_name
    embedded_path = _SCRIPT_ROOTS[1] / script_name
    assert local_path.read_bytes() == embedded_path.read_bytes(), (
        f"script mismatch between roots for {script_name}: "
        f"{local_path} != {embedded_path}"
    )
