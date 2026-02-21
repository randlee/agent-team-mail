"""Tests for session-end.py hook script."""

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


_SESSION_END_PATH = _SCRIPTS_DIR / "session-end.py"


class TestSessionEnd(unittest.TestCase):
    """Tests for session-end.py."""

    def _run(
        self,
        stdin_data: dict,
        *,
        toml_content: str | None = None,
        socket_file_exists: bool = True,
        socket_side_effect=None,
    ) -> tuple[int, list[bytes]]:
        """Run session-end.main(), return (exit_code, sendall_calls)."""
        send_calls: list[bytes] = []

        def capture_send(data: bytes):
            send_calls.append(data)

        with tempfile.TemporaryDirectory() as tmpdir:
            orig_dir = os.getcwd()
            try:
                os.chdir(tmpdir)
                if toml_content is not None:
                    Path(tmpdir, ".atm.toml").write_text(toml_content)

                atm_home = Path(tmpdir)
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
                with patch("sys.stdin", StringIO(stdin_text)), \
                     patch.dict(os.environ, {"ATM_HOME": str(atm_home)}), \
                     patch("socket.socket", return_value=mock_sock):
                    mod = _load_module("session_end", _SESSION_END_PATH)
                    rc = mod.main()
            finally:
                os.chdir(orig_dir)

        return rc, send_calls

    def test_no_atm_toml_no_socket_send(self):
        """No .atm.toml → socket connect must NOT be called."""
        rc, calls = self._run({"session_id": "s1"}, toml_content=None)
        self.assertEqual(rc, 0)
        self.assertEqual(calls, [])

    def test_atm_toml_present_sends_session_end_event(self):
        """With .atm.toml present, sends session_end event to socket."""
        toml = '[core]\ndefault_team = "atm-dev"\nidentity = "team-lead"\n'
        rc, calls = self._run(
            {"session_id": "sess-end-001"},
            toml_content=toml,
            socket_file_exists=True,
        )
        self.assertEqual(rc, 0)
        self.assertEqual(len(calls), 1)
        request = json.loads(calls[0].decode().strip())
        self.assertEqual(request["command"], "hook-event")
        self.assertEqual(request["payload"]["event"], "session_end")
        self.assertEqual(request["payload"]["session_id"], "sess-end-001")
        self.assertEqual(request["payload"]["agent"], "team-lead")
        self.assertEqual(request["payload"]["team"], "atm-dev")
        self.assertEqual(request["payload"]["reason"], "session_exit")

    def test_socket_error_exit_zero(self):
        """Socket error → still exits 0 (fail-open)."""
        toml = '[core]\ndefault_team = "atm-dev"\nidentity = "team-lead"\n'
        rc, _ = self._run(
            {"session_id": "s3"},
            toml_content=toml,
            socket_file_exists=True,
            socket_side_effect=ConnectionRefusedError("daemon not running"),
        )
        self.assertEqual(rc, 0)

    def test_always_exits_zero_with_broken_stdin(self):
        """Malformed stdin → exits 0 (fail-open)."""
        send_calls: list[bytes] = []

        with tempfile.TemporaryDirectory() as tmpdir:
            orig_dir = os.getcwd()
            try:
                os.chdir(tmpdir)
                with patch("sys.stdin", StringIO("not-json{{{")):
                    mod = _load_module("session_end", _SESSION_END_PATH)
                    rc = mod.main()
            finally:
                os.chdir(orig_dir)

        self.assertEqual(rc, 0)

    def test_no_stdout_output(self):
        """SessionEnd script produces no stdout output."""
        toml = '[core]\ndefault_team = "atm-dev"\nidentity = "team-lead"\n'
        captured = StringIO()

        with tempfile.TemporaryDirectory() as tmpdir:
            orig_dir = os.getcwd()
            try:
                os.chdir(tmpdir)
                Path(tmpdir, ".atm.toml").write_text(toml)
                atm_home = Path(tmpdir)
                sock_dir = atm_home / ".claude" / "daemon"
                sock_dir.mkdir(parents=True, exist_ok=True)
                (sock_dir / "atm-daemon.sock").touch()

                mock_sock = MagicMock()
                mock_sock.__enter__ = MagicMock(return_value=mock_sock)
                mock_sock.__exit__ = MagicMock(return_value=False)
                mock_sock.recv.return_value = b'{"status":"ok"}'

                with patch("sys.stdin", StringIO(json.dumps({"session_id": "s4"}))), \
                     patch("sys.stdout", captured), \
                     patch.dict(os.environ, {"ATM_HOME": str(atm_home)}), \
                     patch("socket.socket", return_value=mock_sock):
                    mod = _load_module("session_end", _SESSION_END_PATH)
                    rc = mod.main()
            finally:
                os.chdir(orig_dir)

        self.assertEqual(rc, 0)
        self.assertEqual(captured.getvalue(), "")

    def test_socket_file_missing_no_crash(self):
        """When socket file doesn't exist, no crash."""
        toml = '[core]\ndefault_team = "atm-dev"\nidentity = "team-lead"\n'
        rc, calls = self._run(
            {"session_id": "s5"},
            toml_content=toml,
            socket_file_exists=False,
        )
        self.assertEqual(rc, 0)
        self.assertEqual(calls, [])

    def test_empty_session_id_no_socket_send(self):
        """When session_id is empty or absent, no socket send occurs."""
        toml = '[core]\ndefault_team = "atm-dev"\nidentity = "team-lead"\n'
        # Explicit empty string
        rc, calls = self._run(
            {"session_id": ""},
            toml_content=toml,
            socket_file_exists=True,
        )
        self.assertEqual(rc, 0)
        self.assertEqual(calls, [], "No socket send when session_id is empty string")

    def test_missing_session_id_key_no_socket_send(self):
        """When session_id key is absent from payload, no socket send occurs."""
        toml = '[core]\ndefault_team = "atm-dev"\nidentity = "team-lead"\n'
        rc, calls = self._run(
            {},
            toml_content=toml,
            socket_file_exists=True,
        )
        self.assertEqual(rc, 0)
        self.assertEqual(calls, [], "No socket send when session_id key is absent")

    def test_daemon_not_running_socket_file_missing_exit_zero(self):
        """.atm.toml present but daemon socket file absent → no connect attempt, exits 0."""
        toml = '[core]\ndefault_team = "atm-dev"\nidentity = "team-lead"\n'
        rc, calls = self._run(
            {"session_id": "s6"},
            toml_content=toml,
            socket_file_exists=False,
        )
        self.assertEqual(rc, 0)
        self.assertEqual(calls, [], "No connect attempt when socket file is absent")


if __name__ == "__main__":
    unittest.main()
