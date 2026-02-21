"""Tests for session-start.py hook script."""

import json
import os
import sys
import tempfile
from io import StringIO
from pathlib import Path
from unittest.mock import MagicMock, patch, call
import unittest

# Resolve repo root and hook scripts path:
# tests/hook-scripts/*.py -> <repo>/.claude/scripts/*.py
_REPO_ROOT = Path(__file__).resolve().parents[2]
_SCRIPTS_DIR = _REPO_ROOT / ".claude" / "scripts"
sys.path.insert(0, str(_SCRIPTS_DIR))

import importlib.util

def _load_module(name: str, path: Path):
    """Load a Python file as a module by path."""
    spec = importlib.util.spec_from_file_location(name, path)
    mod = importlib.util.module_from_spec(spec)  # type: ignore[arg-type]
    spec.loader.exec_module(mod)  # type: ignore[union-attr]
    return mod

_SESSION_START_PATH = _SCRIPTS_DIR / "session-start.py"


class TestSessionStartOutput(unittest.TestCase):
    """Tests for stdout context-injection output."""

    def _run_main(self, stdin_data: dict, *, toml_content: str | None = None) -> tuple[int, str]:
        """Run session-start.main() with given stdin payload and optional .atm.toml.

        Returns (exit_code, captured_stdout).
        """
        mod = _load_module("session_start", _SESSION_START_PATH)

        stdin_text = json.dumps(stdin_data)
        captured = StringIO()

        with tempfile.TemporaryDirectory() as tmpdir:
            orig_dir = os.getcwd()
            try:
                os.chdir(tmpdir)
                if toml_content is not None:
                    Path(tmpdir, ".atm.toml").write_text(toml_content)
                with patch("sys.stdin", StringIO(stdin_text)), \
                     patch("sys.stdout", captured):
                    # Reload module in new cwd context
                    mod = _load_module("session_start", _SESSION_START_PATH)
                    rc = mod.main()
            finally:
                os.chdir(orig_dir)

        return rc, captured.getvalue()

    def test_session_id_in_stdout_on_init(self):
        """SESSION_ID line appears in stdout when payload has session_id + source=init."""
        rc, out = self._run_main({"session_id": "abc-123", "source": "init"})
        self.assertEqual(rc, 0)
        self.assertIn("SESSION_ID=abc-123", out)
        self.assertIn("starting fresh", out)

    def test_source_compact_shows_returning_message(self):
        """source=compact produces '(returning from compact)' in output."""
        rc, out = self._run_main({"session_id": "abc-456", "source": "compact"})
        self.assertEqual(rc, 0)
        self.assertIn("SESSION_ID=abc-456", out)
        self.assertIn("returning from compact", out)

    def test_no_atm_toml_no_team_output(self):
        """No .atm.toml → no 'ATM team:' line but SESSION_ID still printed."""
        rc, out = self._run_main({"session_id": "xyz-789", "source": "init"})
        self.assertEqual(rc, 0)
        self.assertIn("SESSION_ID=xyz-789", out)
        self.assertNotIn("ATM team:", out)

    def test_atm_toml_present_shows_team(self):
        """When .atm.toml present, 'ATM team:' line appears in stdout."""
        toml = '[core]\ndefault_team = "atm-dev"\nidentity = "team-lead"\n'
        rc, out = self._run_main(
            {"session_id": "sid-001", "source": "init"}, toml_content=toml
        )
        self.assertEqual(rc, 0)
        self.assertIn("ATM team: atm-dev", out)

    def test_welcome_message_shown_when_set(self):
        """When .atm.toml has welcome-message, it appears in stdout."""
        toml = '[core]\ndefault_team = "atm-dev"\nidentity = "team-lead"\nwelcome-message = "Hello, agent!"\n'
        rc, out = self._run_main(
            {"session_id": "sid-002", "source": "init"}, toml_content=toml
        )
        self.assertEqual(rc, 0)
        self.assertIn("Welcome: Hello, agent!", out)


class TestSessionStartSocketSend(unittest.TestCase):
    """Tests for daemon socket communication."""

    def _run_with_mock_socket(
        self,
        stdin_data: dict,
        *,
        toml_content: str | None = None,
        socket_file_exists: bool = True,
        socket_side_effect=None,
    ) -> tuple[int, str, list]:
        """Run main() with a mocked socket, return (rc, stdout, socket_calls)."""
        mod = _load_module("session_start", _SESSION_START_PATH)

        stdin_text = json.dumps(stdin_data)
        captured = StringIO()
        socket_connect_calls: list = []

        with tempfile.TemporaryDirectory() as tmpdir:
            orig_dir = os.getcwd()
            try:
                os.chdir(tmpdir)
                if toml_content is not None:
                    Path(tmpdir, ".atm.toml").write_text(toml_content)

                # Create a fake socket file when requested
                atm_home = Path(tmpdir)
                sock_dir = atm_home / ".claude" / "daemon"
                sock_dir.mkdir(parents=True, exist_ok=True)
                sock_path = sock_dir / "atm-daemon.sock"
                if socket_file_exists:
                    sock_path.touch()

                mock_sock_instance = MagicMock()
                mock_sock_instance.__enter__ = MagicMock(return_value=mock_sock_instance)
                mock_sock_instance.__exit__ = MagicMock(return_value=False)
                mock_sock_instance.recv.return_value = b'{"status":"ok"}'
                if socket_side_effect:
                    mock_sock_instance.connect.side_effect = socket_side_effect

                def record_connect(addr):
                    socket_connect_calls.append(addr)

                mock_sock_instance.connect.side_effect = (
                    socket_side_effect if socket_side_effect else record_connect
                )

                with patch("sys.stdin", StringIO(stdin_text)), \
                     patch("sys.stdout", captured), \
                     patch.dict(os.environ, {"ATM_HOME": str(atm_home)}):
                    mod = _load_module("session_start", _SESSION_START_PATH)
                    with patch("socket.socket", return_value=mock_sock_instance):
                        rc = mod.main()
            finally:
                os.chdir(orig_dir)

        return rc, captured.getvalue(), socket_connect_calls

    def test_no_atm_toml_no_socket_send(self):
        """No .atm.toml → socket connect must NOT be called."""
        rc, out, calls = self._run_with_mock_socket(
            {"session_id": "s1", "source": "init"},
            toml_content=None,
        )
        self.assertEqual(rc, 0)
        self.assertEqual(calls, [], "Socket should not be called without .atm.toml")

    def test_atm_toml_present_socket_send_called(self):
        """When .atm.toml present, socket connect is called with correct path shape."""
        toml = '[core]\ndefault_team = "atm-dev"\nidentity = "team-lead"\n'
        rc, out, calls = self._run_with_mock_socket(
            {"session_id": "s2", "source": "init"},
            toml_content=toml,
            socket_file_exists=True,
        )
        self.assertEqual(rc, 0)
        self.assertEqual(len(calls), 1)
        self.assertIn("atm-daemon.sock", calls[0])

    def test_socket_error_exit_zero(self):
        """Socket connection error → still exits 0 (fail-open)."""
        toml = '[core]\ndefault_team = "atm-dev"\nidentity = "team-lead"\n'
        rc, out, calls = self._run_with_mock_socket(
            {"session_id": "s3", "source": "init"},
            toml_content=toml,
            socket_file_exists=True,
            socket_side_effect=ConnectionRefusedError("daemon not running"),
        )
        self.assertEqual(rc, 0)

    def test_daemon_not_running_socket_file_missing_exit_zero(self):
        """Daemon socket file not present → no connect attempt, exits 0."""
        toml = '[core]\ndefault_team = "atm-dev"\nidentity = "team-lead"\n'
        rc, out, calls = self._run_with_mock_socket(
            {"session_id": "s4", "source": "init"},
            toml_content=toml,
            socket_file_exists=False,
        )
        self.assertEqual(rc, 0)
        self.assertEqual(calls, [], "No connect attempt when socket file is absent")

    def test_socket_payload_contains_session_id(self):
        """When socket is called, the sendall payload contains the session_id."""
        toml = '[core]\ndefault_team = "atm-dev"\nidentity = "team-lead"\n'
        send_calls: list[bytes] = []

        def capture_send(data: bytes):
            send_calls.append(data)

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
                mock_sock.sendall.side_effect = capture_send

                stdin_text = json.dumps({"session_id": "unique-sess-id", "source": "init"})
                captured = StringIO()
                with patch("sys.stdin", StringIO(stdin_text)), \
                     patch("sys.stdout", captured), \
                     patch.dict(os.environ, {"ATM_HOME": str(atm_home)}), \
                     patch("socket.socket", return_value=mock_sock):
                    mod = _load_module("session_start", _SESSION_START_PATH)
                    rc = mod.main()
            finally:
                os.chdir(orig_dir)

        self.assertEqual(rc, 0)
        self.assertEqual(len(send_calls), 1)
        request = json.loads(send_calls[0].decode().strip())
        self.assertEqual(request["command"], "hook-event")
        self.assertEqual(request["payload"]["event"], "session_start")
        self.assertEqual(request["payload"]["session_id"], "unique-sess-id")
        self.assertEqual(request["payload"]["agent"], "team-lead")
        self.assertEqual(request["payload"]["team"], "atm-dev")


class TestSessionStartGuards(unittest.TestCase):
    """Tests for C-1 and I-1: .atm.toml guard and tomllib fallback."""

    def _run_main_in_tmpdir(
        self,
        stdin_data: dict,
        *,
        toml_content: str | None = None,
        mock_mkdir=None,
        mock_open=None,
    ) -> tuple[int, str]:
        """Run session-start.main() in a temp dir, return (exit_code, stdout)."""
        captured = StringIO()
        stdin_text = json.dumps(stdin_data)

        with tempfile.TemporaryDirectory() as tmpdir:
            orig_dir = os.getcwd()
            try:
                os.chdir(tmpdir)
                if toml_content is not None:
                    Path(tmpdir, ".atm.toml").write_text(toml_content)

                patches = [
                    patch("sys.stdin", StringIO(stdin_text)),
                    patch("sys.stdout", captured),
                ]
                if mock_mkdir is not None:
                    patches.append(patch("pathlib.Path.mkdir", mock_mkdir))
                if mock_open is not None:
                    patches.append(patch("builtins.open", mock_open))

                with patches[0], patches[1]:
                    mod = _load_module("session_start", _SESSION_START_PATH)
                    rc = mod.main()
            finally:
                os.chdir(orig_dir)

        return rc, captured.getvalue()

    def test_no_atm_toml_no_file_io(self):
        """When .atm.toml is absent, no file I/O or directory creation occurs."""
        mkdir_calls = []
        open_calls = []

        def fake_mkdir(self_path, *args, **kwargs):
            mkdir_calls.append(str(self_path))

        def fake_open(path, *args, **kwargs):
            open_calls.append(str(path))
            raise AssertionError(f"open() must not be called without .atm.toml, got: {path}")

        with tempfile.TemporaryDirectory() as tmpdir:
            orig_dir = os.getcwd()
            try:
                os.chdir(tmpdir)
                # No .atm.toml written — guard should prevent all I/O
                stdin_text = json.dumps({"session_id": "test-sid", "source": "init"})
                captured = StringIO()
                with patch("sys.stdin", StringIO(stdin_text)), \
                     patch("sys.stdout", captured):
                    mod = _load_module("session_start", _SESSION_START_PATH)
                    rc = mod.main()
            finally:
                os.chdir(orig_dir)

        self.assertEqual(rc, 0)
        # SESSION_ID stdout is unconditional — that is expected
        self.assertIn("SESSION_ID=test-sid", captured.getvalue())
        # No socket, no file writes: open_calls stays empty (no fake_open was triggered)
        self.assertEqual(open_calls, [])

    def test_tomllib_unavailable_exits_zero(self):
        """When both tomllib and tomli are unavailable, script exits 0 with no side effects."""
        import builtins
        real_import = builtins.__import__

        def import_blocker(name, *args, **kwargs):
            if name in ("tomllib", "tomli"):
                raise ImportError(f"Simulated missing: {name}")
            return real_import(name, *args, **kwargs)

        captured = StringIO()
        socket_calls = []

        with tempfile.TemporaryDirectory() as tmpdir:
            orig_dir = os.getcwd()
            try:
                os.chdir(tmpdir)
                # Write a valid .atm.toml — but tomllib can't parse it
                Path(tmpdir, ".atm.toml").write_text(
                    '[core]\ndefault_team = "atm-dev"\nidentity = "team-lead"\n'
                )
                stdin_text = json.dumps({"session_id": "sid-no-toml", "source": "init"})
                with patch("sys.stdin", StringIO(stdin_text)), \
                     patch("sys.stdout", captured), \
                     patch("builtins.__import__", side_effect=import_blocker), \
                     patch("socket.socket") as mock_sock:
                    mock_sock.return_value.__enter__ = MagicMock(return_value=MagicMock())
                    mock_sock.return_value.__exit__ = MagicMock(return_value=False)
                    mock_sock.side_effect = lambda *a, **kw: socket_calls.append(1) or MagicMock()
                    mod = _load_module("session_start", _SESSION_START_PATH)
                    rc = mod.main()
            finally:
                os.chdir(orig_dir)

        self.assertEqual(rc, 0)
        # SESSION_ID stdout still printed (unconditional)
        self.assertIn("SESSION_ID=sid-no-toml", captured.getvalue())
        # No socket send — tomllib unavailable means read_atm_toml() returned None
        self.assertEqual(socket_calls, [], "Socket must not be called when tomllib is unavailable")


if __name__ == "__main__":
    unittest.main()
