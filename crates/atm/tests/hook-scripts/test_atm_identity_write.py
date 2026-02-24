"""Tests for .claude/scripts/atm-identity-write.py (PreToolUse Bash hook)."""
import json
import os
import platform
import stat
import subprocess
import sys
import tempfile
import time
from pathlib import Path

import pytest

# ---------------------------------------------------------------------------
# Locate the script under test
# ---------------------------------------------------------------------------

# Navigate: tests/hook-scripts -> tests -> atm -> crates -> repo root
REPO_ROOT = Path(__file__).parents[4]
SCRIPT_PATH = REPO_ROOT / ".claude" / "scripts" / "atm-identity-write.py"
SCRIPTS_DIR = SCRIPT_PATH.parent


def _run_script(
    stdin_payload: dict, *, extra_env: dict | None = None
) -> subprocess.CompletedProcess:
    """Run atm-identity-write.py as a subprocess with the given stdin payload."""
    env = {**os.environ, "PYTHONPATH": str(SCRIPTS_DIR)}
    if extra_env:
        env.update(extra_env)
    return subprocess.run(
        [sys.executable, str(SCRIPT_PATH)],
        input=json.dumps(stdin_payload).encode(),
        capture_output=True,
        env=env,
    )


def _recent_hook_files(before: set[Path], max_age_secs: float = 5.0) -> list[Path]:
    """Return newly created hook files since *before* that are very fresh."""
    tmp = Path(tempfile.gettempdir())
    now = time.time()
    after = set(tmp.glob("atm-hook-*.json"))
    return [
        f
        for f in (after - before)
        if now - f.stat().st_mtime < max_age_secs
    ]


# ---------------------------------------------------------------------------
# Tests
# ---------------------------------------------------------------------------


def test_writes_hook_file_for_atm_command():
    """Hook writes a JSON file when the command invokes 'atm' as a token."""
    tmp = Path(tempfile.gettempdir())
    before = set(tmp.glob("atm-hook-*.json"))

    payload = {
        "session_id": "test-session-write",
        "tool_input": {"command": "atm send foo bar"},
    }
    result = _run_script(payload)
    assert result.returncode == 0, f"stderr: {result.stderr.decode()}"

    new_files = _recent_hook_files(before)
    try:
        assert new_files, "No hook file created by atm-identity-write.py for 'atm send'"
        hook_file = max(new_files, key=lambda f: f.stat().st_mtime)
        data = json.loads(hook_file.read_text())
        assert "session_id" in data, "Missing 'session_id' field"
        assert "created_at" in data, "Missing 'created_at' field"
        assert "pid" in data, "Missing 'pid' field"
        assert data["session_id"] == "test-session-write"
    finally:
        for f in new_files:
            f.unlink(missing_ok=True)


def test_skips_non_atm_commands():
    """Hook does NOT write a file for commands that don't invoke atm."""
    tmp = Path(tempfile.gettempdir())
    before = set(tmp.glob("atm-hook-*.json"))

    payload = {
        "session_id": "test-session-skip",
        "tool_input": {"command": "ls -la"},
    }
    result = _run_script(payload)
    assert result.returncode == 0

    new_files = _recent_hook_files(before)
    for f in new_files:
        f.unlink(missing_ok=True)
    assert not new_files, f"Hook file unexpectedly created for 'ls -la': {new_files}"


def test_skips_partial_atm_match():
    """'cat atm-log.txt' has 'atm' as a substring but is NOT an atm invocation."""
    tmp = Path(tempfile.gettempdir())
    before = set(tmp.glob("atm-hook-*.json"))

    payload = {
        "session_id": "test-session-partial",
        "tool_input": {"command": "cat atm-log.txt"},
    }
    result = _run_script(payload)
    assert result.returncode == 0

    new_files = _recent_hook_files(before)
    for f in new_files:
        f.unlink(missing_ok=True)
    assert not new_files, f"Hook file unexpectedly created for 'cat atm-log.txt': {new_files}"


def test_handles_missing_command_field():
    """Empty payload must not crash — fail-open, exit 0."""
    result = _run_script({})
    assert result.returncode == 0


def test_handles_missing_tool_input():
    """Payload without tool_input should not crash — fail-open, exit 0."""
    result = _run_script({"session_id": "s1"})
    assert result.returncode == 0


@pytest.mark.skipif(platform.system() == "Windows", reason="chmod not applicable on Windows")
def test_file_permissions_unix():
    """On Unix, the created hook file must have mode 0o600."""
    tmp = Path(tempfile.gettempdir())
    before = set(tmp.glob("atm-hook-*.json"))

    payload = {
        "session_id": "perm-test-session",
        "tool_input": {"command": "atm read"},
    }
    result = _run_script(payload)
    assert result.returncode == 0

    new_files = _recent_hook_files(before)
    if not new_files:
        pytest.skip("No hook file created — cannot verify permissions")

    hook_file = max(new_files, key=lambda f: f.stat().st_mtime)
    try:
        mode = oct(stat.S_IMODE(hook_file.stat().st_mode))
        assert mode == oct(0o600), f"Expected permissions 0o600, got {mode}"
    finally:
        for f in new_files:
            f.unlink(missing_ok=True)
