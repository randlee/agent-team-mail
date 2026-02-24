"""Tests for gate-agent-spawns.py (PreToolUse hook)."""

import json
import os
import sys
import tempfile
from io import StringIO
from pathlib import Path
import unittest

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


_GATE_PATH = _SCRIPTS_DIR / "gate-agent-spawns.py"

_TOML_WITH_TEAM = '[core]\ndefault_team = "atm-dev"\nidentity = "team-lead"\n'


def _make_tool_input(
    subagent_type: str = "rust-developer",
    name: str = "",
    team_name: str = "",
) -> dict:
    """Build a PreToolUse Task payload."""
    ti: dict = {"subagent_type": subagent_type}
    if name:
        ti["name"] = name
    if team_name:
        ti["team_name"] = team_name
    return ti


def _run_gate(
    tool_input: dict,
    *,
    session_id: str = "sess-lead-001",
    tmpdir: str,
    toml_content: str | None = None,
    team_config: dict | None = None,
    team_name_for_config: str = "atm-dev",
) -> tuple[int, "module"]:
    """Run gate-agent-spawns.main() in a temp dir environment.

    Returns (exit_code, loaded_module).
    """
    data = {
        "tool_name": "Task",
        "tool_input": tool_input,
        "session_id": session_id,
    }

    orig_dir = os.getcwd()
    try:
        os.chdir(tmpdir)
        if toml_content is not None:
            Path(tmpdir, ".atm.toml").write_text(toml_content)

        # Create team config.json if requested (mock HOME)
        home_dir = Path(tmpdir) / "fakehome"
        if team_config is not None:
            team_dir = home_dir / ".claude" / "teams" / team_name_for_config
            team_dir.mkdir(parents=True, exist_ok=True)
            (team_dir / "config.json").write_text(json.dumps(team_config))

        stdin_text = json.dumps(data)
        from unittest.mock import patch

        with patch("sys.stdin", StringIO(stdin_text)), \
             patch("pathlib.Path.home", return_value=home_dir):
            mod = _load_module("gate_agent_spawns", _GATE_PATH)
            # Override DEBUG_LOG to temp path to avoid polluting system tmp
            mod.DEBUG_LOG = Path(tmpdir) / "debug.jsonl"
            mod.SESSION_ID_FILE = Path(tmpdir) / "atm-session-id"
            rc = mod.main()
    finally:
        os.chdir(orig_dir)

    return rc, mod


class TestGateRule1Orchestrators(unittest.TestCase):
    """Rule 1: Orchestrators must be named teammates."""

    def test_orchestrator_without_name_blocked(self):
        """scrum-master without name → exit 2."""
        with tempfile.TemporaryDirectory() as tmpdir:
            rc, _ = _run_gate(
                _make_tool_input(subagent_type="scrum-master"),
                tmpdir=tmpdir,
            )
        self.assertEqual(rc, 2)

    def test_orchestrator_with_name_allowed(self):
        """scrum-master with name → exit 0."""
        with tempfile.TemporaryDirectory() as tmpdir:
            rc, _ = _run_gate(
                _make_tool_input(subagent_type="scrum-master", name="sm-x"),
                tmpdir=tmpdir,
            )
        self.assertEqual(rc, 0)

    def test_non_orchestrator_without_name_allowed(self):
        """rust-developer without name → exit 0."""
        with tempfile.TemporaryDirectory() as tmpdir:
            rc, _ = _run_gate(
                _make_tool_input(subagent_type="rust-developer"),
                tmpdir=tmpdir,
            )
        self.assertEqual(rc, 0)

    def test_non_orchestrator_with_name_allowed(self):
        """rust-developer with name → exit 0."""
        with tempfile.TemporaryDirectory() as tmpdir:
            rc, _ = _run_gate(
                _make_tool_input(subagent_type="rust-developer", name="dev"),
                tmpdir=tmpdir,
            )
        self.assertEqual(rc, 0)


class TestGateRule2TeamLeadOnly(unittest.TestCase):
    """Rule 2: Only team lead can use team_name."""

    def test_team_lead_can_use_team_name(self):
        """Matching session_id → exit 0."""
        with tempfile.TemporaryDirectory() as tmpdir:
            rc, _ = _run_gate(
                _make_tool_input(subagent_type="rust-developer", name="dev", team_name="atm-dev"),
                session_id="lead-sess-123",
                tmpdir=tmpdir,
                toml_content=_TOML_WITH_TEAM,
                team_config={"leadSessionId": "lead-sess-123"},
            )
        self.assertEqual(rc, 0)

    def test_teammate_blocked_from_team_name(self):
        """Non-matching session_id → exit 2."""
        with tempfile.TemporaryDirectory() as tmpdir:
            rc, _ = _run_gate(
                _make_tool_input(subagent_type="rust-developer", name="dev", team_name="atm-dev"),
                session_id="teammate-sess-999",
                tmpdir=tmpdir,
                toml_content=_TOML_WITH_TEAM,
                team_config={"leadSessionId": "lead-sess-123"},
            )
        self.assertEqual(rc, 2)

    def test_no_team_config_fail_open(self):
        """No config.json exists → exit 0 (fail open)."""
        with tempfile.TemporaryDirectory() as tmpdir:
            rc, _ = _run_gate(
                _make_tool_input(subagent_type="rust-developer", name="dev", team_name="atm-dev"),
                session_id="any-sess",
                tmpdir=tmpdir,
                toml_content=_TOML_WITH_TEAM,
                team_config=None,  # no config.json
            )
        self.assertEqual(rc, 0)

    def test_no_lead_session_id_fail_open(self):
        """config.json exists but no leadSessionId → exit 0 (fail open)."""
        with tempfile.TemporaryDirectory() as tmpdir:
            rc, _ = _run_gate(
                _make_tool_input(subagent_type="rust-developer", name="dev", team_name="atm-dev"),
                session_id="any-sess",
                tmpdir=tmpdir,
                toml_content=_TOML_WITH_TEAM,
                team_config={"someOtherField": "value"},  # no leadSessionId
            )
        self.assertEqual(rc, 0)


class TestGateRule3TeamNameMatch(unittest.TestCase):
    """Rule 3: team_name must match .atm.toml default_team."""

    def test_mismatched_team_name_blocked(self):
        """team_name='wrong' + .atm.toml has 'atm-dev' → exit 2."""
        with tempfile.TemporaryDirectory() as tmpdir:
            rc, _ = _run_gate(
                _make_tool_input(subagent_type="rust-developer", name="dev", team_name="wrong"),
                tmpdir=tmpdir,
                toml_content=_TOML_WITH_TEAM,
            )
        self.assertEqual(rc, 2)

    def test_matching_team_name_allowed(self):
        """team_name='atm-dev' + .atm.toml has 'atm-dev' → exit 0."""
        with tempfile.TemporaryDirectory() as tmpdir:
            rc, _ = _run_gate(
                _make_tool_input(subagent_type="rust-developer", name="dev", team_name="atm-dev"),
                session_id="lead-sess-123",
                tmpdir=tmpdir,
                toml_content=_TOML_WITH_TEAM,
                team_config={"leadSessionId": "lead-sess-123"},
            )
        self.assertEqual(rc, 0)

    def test_no_atm_toml_team_name_allowed(self):
        """No .atm.toml + team_name provided → exit 0 (no required_team)."""
        with tempfile.TemporaryDirectory() as tmpdir:
            rc, _ = _run_gate(
                _make_tool_input(subagent_type="rust-developer", name="dev", team_name="any-team"),
                tmpdir=tmpdir,
                toml_content=None,  # no .atm.toml
            )
        self.assertEqual(rc, 0)


class TestGateFailOpen(unittest.TestCase):
    """Fail-open and debug logging."""

    def test_malformed_stdin_exit_zero(self):
        """Bad JSON → exit 0."""
        with tempfile.TemporaryDirectory() as tmpdir:
            orig_dir = os.getcwd()
            try:
                os.chdir(tmpdir)
                from unittest.mock import patch
                with patch("sys.stdin", StringIO("not-json{{{")):
                    mod = _load_module("gate_agent_spawns", _GATE_PATH)
                    rc = mod.main()
            finally:
                os.chdir(orig_dir)
        self.assertEqual(rc, 0)

    def test_empty_payload_exit_zero(self):
        """Empty dict {} → exit 0."""
        with tempfile.TemporaryDirectory() as tmpdir:
            orig_dir = os.getcwd()
            try:
                os.chdir(tmpdir)
                from unittest.mock import patch
                with patch("sys.stdin", StringIO("{}")):
                    mod = _load_module("gate_agent_spawns", _GATE_PATH)
                    mod.DEBUG_LOG = Path(tmpdir) / "debug.jsonl"
                    mod.SESSION_ID_FILE = Path(tmpdir) / "atm-session-id"
                    rc = mod.main()
            finally:
                os.chdir(orig_dir)
        self.assertEqual(rc, 0)

    def test_debug_log_written(self):
        """Debug JSONL file gets an entry."""
        with tempfile.TemporaryDirectory() as tmpdir:
            rc, mod = _run_gate(
                _make_tool_input(subagent_type="rust-developer"),
                tmpdir=tmpdir,
            )
            debug_log = Path(tmpdir) / "debug.jsonl"
            self.assertTrue(debug_log.exists(), "debug.jsonl must be created")
            lines = debug_log.read_text().strip().splitlines()
            self.assertGreaterEqual(len(lines), 1)
            entry = json.loads(lines[0])
            self.assertIn("tool_input", entry)

    def test_debug_log_contains_process_id(self):
        """Debug log entry includes process_id."""
        with tempfile.TemporaryDirectory() as tmpdir:
            rc, mod = _run_gate(
                _make_tool_input(subagent_type="rust-developer"),
                tmpdir=tmpdir,
            )
            debug_log = Path(tmpdir) / "debug.jsonl"
            entry = json.loads(debug_log.read_text().strip().splitlines()[0])
            self.assertIn("process_id", entry)
            self.assertIsInstance(entry["process_id"], int)

    def test_session_id_file_written(self):
        """Session ID breadcrumb file is written."""
        with tempfile.TemporaryDirectory() as tmpdir:
            rc, mod = _run_gate(
                _make_tool_input(subagent_type="rust-developer"),
                session_id="breadcrumb-sess-42",
                tmpdir=tmpdir,
            )
            sid_file = Path(tmpdir) / "atm-session-id"
            self.assertTrue(sid_file.exists(), "atm-session-id breadcrumb must be written")
            self.assertEqual(sid_file.read_text(), "breadcrumb-sess-42")


if __name__ == "__main__":
    unittest.main()
