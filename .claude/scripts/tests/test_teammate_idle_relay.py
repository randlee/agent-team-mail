"""Tests for teammate-idle-relay.py hook script."""

import json
import os
import sys
import tempfile
from io import StringIO
from pathlib import Path
from unittest.mock import MagicMock, patch
import unittest

_SCRIPTS_DIR = Path(__file__).resolve().parent.parent
sys.path.insert(0, str(_SCRIPTS_DIR))

import importlib.util


def _load_module(name: str, path: Path):
    """Load a Python file as a module by path."""
    spec = importlib.util.spec_from_file_location(name, path)
    mod = importlib.util.module_from_spec(spec)  # type: ignore[arg-type]
    spec.loader.exec_module(mod)  # type: ignore[union-attr]
    return mod


_RELAY_PATH = _SCRIPTS_DIR / "teammate-idle-relay.py"


def _make_payload(
    agent: str = "arch-ctm",
    team: str = "atm-dev",
    session_id: str = "sess-1",
) -> dict:
    return {
        "name": agent,
        "team_name": team,
        "session_id": session_id,
    }


class TestTeammateIdleRelayFileWrite(unittest.TestCase):
    """Original file-write behaviour must still work."""

    def _run(self, stdin_data: dict, *, atm_home: Path) -> int:
        mod = _load_module("teammate_idle_relay", _RELAY_PATH)
        stdin_text = json.dumps(stdin_data)
        with patch("sys.stdin", StringIO(stdin_text)), \
             patch.dict(os.environ, {"ATM_HOME": str(atm_home)}):
            mod = _load_module("teammate_idle_relay", _RELAY_PATH)
            return mod.main()

    def test_appends_jsonl_event(self):
        """Event is written to events.jsonl."""
        with tempfile.TemporaryDirectory() as tmpdir:
            atm_home = Path(tmpdir)
            rc = self._run(_make_payload(), atm_home=atm_home)
            self.assertEqual(rc, 0)
            events_file = atm_home / ".claude" / "daemon" / "hooks" / "events.jsonl"
            self.assertTrue(events_file.exists())
            lines = events_file.read_text().strip().splitlines()
            self.assertEqual(len(lines), 1)
            event = json.loads(lines[0])
            self.assertEqual(event["type"], "teammate-idle")
            self.assertEqual(event["agent"], "arch-ctm")

    def test_multiple_events_appended(self):
        """Multiple calls append multiple lines."""
        with tempfile.TemporaryDirectory() as tmpdir:
            atm_home = Path(tmpdir)
            for _ in range(3):
                self._run(_make_payload(), atm_home=atm_home)
            events_file = atm_home / ".claude" / "daemon" / "hooks" / "events.jsonl"
            lines = events_file.read_text().strip().splitlines()
            self.assertEqual(len(lines), 3)

    def test_team_from_toml(self):
        """Team is read from .atm.toml when not in payload."""
        toml = '[core]\ndefault_team = "toml-team"\nidentity = "some-agent"\n'
        with tempfile.TemporaryDirectory() as tmpdir:
            atm_home = Path(tmpdir)
            orig_dir = os.getcwd()
            try:
                os.chdir(tmpdir)
                Path(tmpdir, ".atm.toml").write_text(toml)
                payload = {"name": "arch-ctm", "session_id": "s1"}
                rc = self._run(payload, atm_home=atm_home)
                self.assertEqual(rc, 0)
                events_file = atm_home / ".claude" / "daemon" / "hooks" / "events.jsonl"
                event = json.loads(events_file.read_text().strip())
                self.assertEqual(event["team"], "toml-team")
            finally:
                os.chdir(orig_dir)

    def test_exit_zero_on_bad_stdin(self):
        """Malformed stdin → exits 0 (fail-open)."""
        with tempfile.TemporaryDirectory() as tmpdir:
            atm_home = Path(tmpdir)
            with patch("sys.stdin", StringIO("not-json{{{")), \
                 patch.dict(os.environ, {"ATM_HOME": str(atm_home)}):
                mod = _load_module("teammate_idle_relay", _RELAY_PATH)
                rc = mod.main()
        self.assertEqual(rc, 0)


class TestTeammateIdleRelaySocketSend(unittest.TestCase):
    """Socket send behaviour — additive, does not affect file write."""

    def _run_with_mock_socket(
        self,
        stdin_data: dict,
        *,
        atm_home: Path,
        toml_content: str | None = None,
        socket_file_exists: bool = True,
        socket_side_effect=None,
    ) -> tuple[int, list[bytes]]:
        send_calls: list[bytes] = []

        def capture_send(data: bytes):
            send_calls.append(data)

        sock_dir = atm_home / ".claude" / "daemon"
        sock_dir.mkdir(parents=True, exist_ok=True)
        if socket_file_exists:
            (sock_dir / "atm-daemon.sock").touch()

        mock_sock = MagicMock()
        mock_sock.__enter__ = MagicMock(return_value=mock_sock)
        mock_sock.__exit__ = MagicMock(return_value=False)
        mock_sock.recv.return_value = b'{"status":"ok"}'
        if socket_side_effect:
            mock_sock.connect.side_effect = socket_side_effect
        else:
            mock_sock.sendall.side_effect = capture_send

        stdin_text = json.dumps(stdin_data)
        # Always chdir to a temp dir so no ambient .atm.toml is found unless
        # toml_content is explicitly provided.
        with tempfile.TemporaryDirectory() as run_dir:
            orig_dir = os.getcwd()
            try:
                os.chdir(run_dir)
                if toml_content is not None:
                    Path(run_dir, ".atm.toml").write_text(toml_content)
                with patch("sys.stdin", StringIO(stdin_text)), \
                     patch.dict(os.environ, {"ATM_HOME": str(atm_home)}), \
                     patch("socket.socket", return_value=mock_sock):
                    mod = _load_module("teammate_idle_relay", _RELAY_PATH)
                    rc = mod.main()
            finally:
                os.chdir(orig_dir)

        return rc, send_calls

    def test_no_atm_toml_no_socket_send(self):
        """No .atm.toml → no socket send."""
        with tempfile.TemporaryDirectory() as tmpdir:
            atm_home = Path(tmpdir)
            rc, calls = self._run_with_mock_socket(
                _make_payload(),
                atm_home=atm_home,
                toml_content=None,
            )
        self.assertEqual(rc, 0)
        self.assertEqual(calls, [])

    def test_atm_toml_present_socket_send_called(self):
        """With .atm.toml, socket send is called with correct payload shape."""
        toml = '[core]\ndefault_team = "atm-dev"\nidentity = "team-lead"\n'
        with tempfile.TemporaryDirectory() as tmpdir:
            atm_home = Path(tmpdir)
            rc, calls = self._run_with_mock_socket(
                _make_payload(agent="arch-ctm", team="atm-dev", session_id="sess-42"),
                atm_home=atm_home,
                toml_content=toml,
            )
        self.assertEqual(rc, 0)
        self.assertEqual(len(calls), 1)
        request = json.loads(calls[0].decode().strip())
        self.assertEqual(request["command"], "hook-event")
        self.assertEqual(request["payload"]["event"], "teammate_idle")
        self.assertEqual(request["payload"]["agent"], "arch-ctm")

    def test_socket_error_file_write_still_succeeds(self):
        """Socket error must not prevent the file write from succeeding."""
        toml = '[core]\ndefault_team = "atm-dev"\nidentity = "team-lead"\n'
        with tempfile.TemporaryDirectory() as tmpdir:
            atm_home = Path(tmpdir)
            rc, _ = self._run_with_mock_socket(
                _make_payload(),
                atm_home=atm_home,
                toml_content=toml,
                socket_file_exists=True,
                socket_side_effect=ConnectionRefusedError("daemon not running"),
            )
            events_file = atm_home / ".claude" / "daemon" / "hooks" / "events.jsonl"
            self.assertEqual(rc, 0)
            self.assertTrue(events_file.exists(), "events.jsonl must exist even on socket error")

    def test_socket_error_exit_zero(self):
        """Socket error → exits 0 (fail-open)."""
        toml = '[core]\ndefault_team = "atm-dev"\nidentity = "team-lead"\n'
        with tempfile.TemporaryDirectory() as tmpdir:
            atm_home = Path(tmpdir)
            rc, _ = self._run_with_mock_socket(
                _make_payload(),
                atm_home=atm_home,
                toml_content=toml,
                socket_file_exists=True,
                socket_side_effect=ConnectionRefusedError("daemon not running"),
            )
        self.assertEqual(rc, 0)

    def test_daemon_not_running_socket_file_missing_exit_zero(self):
        """.atm.toml present but daemon socket file absent → no connect attempt, exits 0."""
        toml = '[core]\ndefault_team = "atm-dev"\nidentity = "team-lead"\n'
        with tempfile.TemporaryDirectory() as tmpdir:
            atm_home = Path(tmpdir)
            rc, calls = self._run_with_mock_socket(
                _make_payload(),
                atm_home=atm_home,
                toml_content=toml,
                socket_file_exists=False,
            )
        self.assertEqual(rc, 0)
        self.assertEqual(calls, [], "No connect attempt when socket file is absent")


if __name__ == "__main__":
    unittest.main()
