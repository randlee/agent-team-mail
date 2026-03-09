"""Tests for session-start.py hook script."""

import json
import os
import platform
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

    def _run_main(
        self,
        stdin_data: dict,
        *,
        toml_content: str | None = None,
        env_overrides: dict[str, str] | None = None,
    ) -> tuple[int, str]:
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
                env = {"ATM_TEAM": "", "ATM_IDENTITY": ""}
                if env_overrides:
                    env.update(env_overrides)
                with patch("sys.stdin", StringIO(stdin_text)), \
                     patch("sys.stdout", captured), \
                     patch.dict(os.environ, env):
                    # Reload module in new cwd context
                    mod = _load_module("session_start", _SESSION_START_PATH)
                    rc = mod.main()
            finally:
                os.chdir(orig_dir)

        return rc, captured.getvalue()

    def test_session_id_in_stdout_on_init(self):
        """SESSION_ID line appears when ATM context exists."""
        rc, out = self._run_main(
            {"session_id": "abc-123", "source": "init"},
            env_overrides={"ATM_TEAM": "atm-dev", "ATM_IDENTITY": "arch-ctm"},
        )
        self.assertEqual(rc, 0)
        self.assertIn("SESSION_ID=abc-123", out)
        self.assertIn("starting fresh", out)

    def test_source_compact_shows_returning_message(self):
        """source=compact produces '(returning from compact)' in output."""
        rc, out = self._run_main(
            {"session_id": "abc-456", "source": "compact"},
            env_overrides={"ATM_TEAM": "atm-dev", "ATM_IDENTITY": "arch-ctm"},
        )
        self.assertEqual(rc, 0)
        self.assertIn("SESSION_ID=abc-456", out)
        self.assertIn("returning from compact", out)

    def test_no_atm_toml_no_team_output(self):
        """No ATM context → no output."""
        rc, out = self._run_main({"session_id": "xyz-789", "source": "init"})
        self.assertEqual(rc, 0)
        self.assertEqual(out, "")
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
        env_overrides: dict[str, str] | None = None,
    ) -> tuple[int, str, str, list]:
        """Run main() with a mocked socket, return (rc, stdout, stderr, socket_calls)."""
        mod = _load_module("session_start", _SESSION_START_PATH)

        stdin_text = json.dumps(stdin_data)
        captured = StringIO()
        captured_err = StringIO()
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
                    if platform.system() == "Windows":
                        (sock_dir / "atm-daemon.port").write_text("12345")

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

                env = {"ATM_HOME": str(atm_home), "ATM_TEAM": "", "ATM_IDENTITY": ""}
                if env_overrides:
                    env.update(env_overrides)
                with patch("sys.stdin", StringIO(stdin_text)), \
                     patch("sys.stdout", captured), \
                     patch("sys.stderr", captured_err), \
                     patch.dict(os.environ, env):
                    mod = _load_module("session_start", _SESSION_START_PATH)
                    with patch("socket.socket", return_value=mock_sock_instance):
                        rc = mod.main()
            finally:
                os.chdir(orig_dir)

        return rc, captured.getvalue(), captured_err.getvalue(), socket_connect_calls

    def test_no_atm_toml_no_socket_send(self):
        """No .atm.toml → socket connect must NOT be called."""
        rc, out, _, calls = self._run_with_mock_socket(
            {"session_id": "s1", "source": "init"},
            toml_content=None,
        )
        self.assertEqual(rc, 0)
        self.assertEqual(calls, [], "Socket should not be called without .atm.toml")

    def test_atm_toml_present_socket_send_called(self):
        """When .atm.toml present, socket connect is called with correct path shape."""
        toml = '[core]\ndefault_team = "atm-dev"\nidentity = "team-lead"\n'
        rc, out, _, calls = self._run_with_mock_socket(
            {"session_id": "s2", "source": "init"},
            toml_content=toml,
            socket_file_exists=True,
        )
        self.assertEqual(rc, 0)
        self.assertEqual(len(calls), 1)
        if platform.system() != "Windows":
            self.assertIn("atm-daemon.sock", calls[0])

    def test_socket_error_exit_zero(self):
        """Socket connection error → still exits 0 (fail-open)."""
        toml = '[core]\ndefault_team = "atm-dev"\nidentity = "team-lead"\n'
        rc, out, _, calls = self._run_with_mock_socket(
            {"session_id": "s3", "source": "init"},
            toml_content=toml,
            socket_file_exists=True,
            socket_side_effect=ConnectionRefusedError("daemon not running"),
        )
        self.assertEqual(rc, 0)

    def test_daemon_not_running_socket_file_missing_exit_zero(self):
        """Daemon socket file not present → no connect attempt, exits 0."""
        toml = '[core]\ndefault_team = "atm-dev"\nidentity = "team-lead"\n'
        rc, out, _, calls = self._run_with_mock_socket(
            {"session_id": "s4", "source": "init"},
            toml_content=toml,
            socket_file_exists=False,
        )
        self.assertEqual(rc, 0)
        self.assertEqual(calls, [], "No connect attempt when socket file is absent")

    def test_env_overrides_toml_in_payload_and_emits_mismatch_warning(self):
        """ATM_TEAM/ATM_IDENTITY must override .atm.toml and warn when team mismatches."""
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
                if platform.system() == "Windows":
                    (sock_dir / "atm-daemon.port").write_text("12345")

                mock_sock = MagicMock()
                mock_sock.__enter__ = MagicMock(return_value=mock_sock)
                mock_sock.__exit__ = MagicMock(return_value=False)
                mock_sock.recv.return_value = b'{"status":"ok"}'
                mock_sock.sendall.side_effect = capture_send

                stdin_text = json.dumps({"session_id": "sid-env-1", "source": "init"})
                captured = StringIO()
                captured_err = StringIO()
                with patch("sys.stdin", StringIO(stdin_text)), \
                     patch("sys.stdout", captured), \
                     patch("sys.stderr", captured_err), \
                     patch.dict(
                         os.environ,
                         {
                             "ATM_HOME": str(atm_home),
                             "ATM_TEAM": "env-team",
                             "ATM_IDENTITY": "env-agent",
                         },
                     ), \
                     patch("socket.socket", return_value=mock_sock):
                    mod = _load_module("session_start", _SESSION_START_PATH)
                    rc = mod.main()
            finally:
                os.chdir(orig_dir)

        self.assertEqual(rc, 0)
        self.assertEqual(len(send_calls), 1)
        request = json.loads(send_calls[0].decode().strip())
        self.assertEqual(request["payload"]["team"], "env-team")
        self.assertEqual(request["payload"]["agent"], "env-agent")
        self.assertIn("ATM team: env-team", captured.getvalue())
        self.assertIn("overrides .atm.toml default_team", captured_err.getvalue())

    def test_env_context_without_toml_still_sends_hook_event(self):
        """When .atm.toml is absent but env team/identity exist, hook send still occurs."""
        rc, out, _, calls = self._run_with_mock_socket(
            {"session_id": "sid-env-no-toml", "source": "init"},
            toml_content=None,
            socket_file_exists=True,
            env_overrides={"ATM_TEAM": "atm-dev", "ATM_IDENTITY": "arch-ctm"},
        )
        self.assertEqual(rc, 0)
        self.assertEqual(len(calls), 1)
        self.assertIn("ATM team: atm-dev", out)

    def test_socket_payload_contains_session_id(self):
        """When socket is called, the sendall payload contains the session_id."""
        toml = '[core]\ndefault_team = "atm-dev"\nidentity = "team-lead"\n'
        send_calls: list[bytes] = []
        expected_parent_pid = 4242

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
                if platform.system() == "Windows":
                    (sock_dir / "atm-daemon.port").write_text("12345")

                mock_sock = MagicMock()
                mock_sock.__enter__ = MagicMock(return_value=mock_sock)
                mock_sock.__exit__ = MagicMock(return_value=False)
                mock_sock.recv.return_value = b'{"status":"ok"}'
                mock_sock.sendall.side_effect = capture_send

                stdin_text = json.dumps({"session_id": "unique-sess-id", "source": "init"})
                captured = StringIO()
                with patch("sys.stdin", StringIO(stdin_text)), \
                     patch("sys.stdout", captured), \
                     patch.dict(
                         os.environ,
                         {"ATM_HOME": str(atm_home), "ATM_TEAM": "", "ATM_IDENTITY": ""},
                     ), \
                     patch("os.getppid", return_value=expected_parent_pid), \
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
        self.assertEqual(request["payload"]["source"]["kind"], "claude_hook")
        self.assertIn("process_id", request["payload"])
        self.assertEqual(request["payload"]["process_id"], expected_parent_pid)


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
                    patch.dict(os.environ, {"ATM_TEAM": "", "ATM_IDENTITY": ""}),
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
                     patch("sys.stdout", captured), \
                     patch.dict(os.environ, {"ATM_TEAM": "", "ATM_IDENTITY": ""}, clear=False):
                    mod = _load_module("session_start", _SESSION_START_PATH)
                    rc = mod.main()
            finally:
                os.chdir(orig_dir)

        self.assertEqual(rc, 0)
        # No ATM context, so no context injection output.
        self.assertEqual(captured.getvalue(), "")
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
                     patch.dict(os.environ, {"ATM_TEAM": "", "ATM_IDENTITY": ""}), \
                     patch("socket.socket") as mock_sock:
                    mock_sock.return_value.__enter__ = MagicMock(return_value=MagicMock())
                    mock_sock.return_value.__exit__ = MagicMock(return_value=False)
                    mock_sock.side_effect = lambda *a, **kw: socket_calls.append(1) or MagicMock()
                    mod = _load_module("session_start", _SESSION_START_PATH)
                    rc = mod.main()
            finally:
                os.chdir(orig_dir)

        self.assertEqual(rc, 0)
        # Without TOML parsing and without env context, no output should be emitted.
        self.assertEqual(captured.getvalue(), "")
        # No socket send — tomllib unavailable means read_atm_toml() returned None
        self.assertEqual(socket_calls, [], "Socket must not be called when tomllib is unavailable")


class TestSessionStartSessionFile(unittest.TestCase):
    """Tests for the session file write block added to session-start.py."""

    _TOML = '[core]\ndefault_team = "atm-dev"\nidentity = "team-lead"\n'

    def _run(self, tmpdir: str, stdin_text: str) -> int:
        """Run session-start.py from tmpdir with the given stdin, returning exit code."""
        captured = StringIO()
        orig_dir = os.getcwd()
        try:
            os.chdir(tmpdir)
            Path(tmpdir, ".atm.toml").write_text(self._TOML)
            with patch("sys.stdin", StringIO(stdin_text)), \
                 patch("sys.stdout", captured), \
                 patch.dict(os.environ, {"ATM_HOME": tmpdir, "ATM_TEAM": "", "ATM_IDENTITY": ""}), \
                 patch("socket.socket"):
                mod = _load_module("session_start", _SESSION_START_PATH)
                return mod.main()
        finally:
            os.chdir(orig_dir)

    def test_session_file_written_with_correct_fields(self):
        """Session file is written with session_id, team, identity, pid, created_at, updated_at."""
        with tempfile.TemporaryDirectory() as tmpdir:
            stdin_text = json.dumps({"session_id": "abc-123", "source": "init"})
            rc = self._run(tmpdir, stdin_text)
            self.assertEqual(rc, 0)

            expected_path = Path(tmpdir) / ".claude" / "teams" / "atm-dev" / "sessions" / "abc-123.json"
            self.assertTrue(expected_path.exists(), f"Session file not found at {expected_path}")
            data = json.loads(expected_path.read_text())
            self.assertEqual(data["session_id"], "abc-123")
            self.assertEqual(data["team"], "atm-dev")
            self.assertEqual(data["identity"], "team-lead")
            self.assertIn("pid", data)
            self.assertIn("created_at", data)
            self.assertIn("updated_at", data)

    def test_session_file_not_written_without_session_id(self):
        """No session file is written when session_id is absent."""
        with tempfile.TemporaryDirectory() as tmpdir:
            stdin_text = json.dumps({"source": "init"})
            rc = self._run(tmpdir, stdin_text)
            self.assertEqual(rc, 0)

            sessions_dir = Path(tmpdir) / ".claude" / "teams" / "atm-dev" / "sessions"
            if sessions_dir.exists():
                files = list(sessions_dir.glob("*.json"))
                self.assertEqual(files, [], "No session file should be written without session_id")

    def test_session_file_preserves_created_at_on_refire(self):
        """On compact/resume re-fire, created_at is preserved; only updated_at changes."""
        with tempfile.TemporaryDirectory() as tmpdir:
            sessions_dir = Path(tmpdir) / ".claude" / "teams" / "atm-dev" / "sessions"
            sessions_dir.mkdir(parents=True)
            import time
            original_created = time.time() - 3600  # 1 hour ago
            existing = {
                "session_id": "abc-resume",
                "team": "atm-dev",
                "identity": "team-lead",
                "pid": 9999,
                "created_at": original_created,
                "updated_at": original_created,
            }
            (sessions_dir / "abc-resume.json").write_text(json.dumps(existing))

            stdin_text = json.dumps({"session_id": "abc-resume", "source": "compact"})
            rc = self._run(tmpdir, stdin_text)
            self.assertEqual(rc, 0)

            data = json.loads((sessions_dir / "abc-resume.json").read_text())
            self.assertAlmostEqual(
                data["created_at"], original_created, delta=1.0,
                msg="created_at must be preserved on re-fire"
            )
            self.assertGreater(
                data["updated_at"], original_created,
                msg="updated_at must be refreshed on re-fire"
            )

    def test_session_file_write_failure_exits_zero(self):
        """A write failure is silently swallowed — script exits 0 (fail-open)."""
        with tempfile.TemporaryDirectory() as tmpdir:
            orig_dir = os.getcwd()
            captured = StringIO()
            try:
                os.chdir(tmpdir)
                Path(tmpdir, ".atm.toml").write_text(self._TOML)
                stdin_text = json.dumps({"session_id": "err-test", "source": "init"})
                with patch("sys.stdin", StringIO(stdin_text)), \
                     patch("sys.stdout", captured), \
                     patch.dict(os.environ, {"ATM_HOME": tmpdir, "ATM_TEAM": "", "ATM_IDENTITY": ""}), \
                     patch("socket.socket"), \
                     patch("pathlib.Path.write_text", side_effect=OSError("simulated disk full")):
                    mod = _load_module("session_start", _SESSION_START_PATH)
                    rc = mod.main()
            finally:
                os.chdir(orig_dir)

            self.assertEqual(rc, 0, "Script must exit 0 even when write fails (fail-open)")


if __name__ == "__main__":
    unittest.main()
