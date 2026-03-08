"""Unit tests for scripts/launch-worker.py."""

from __future__ import annotations

import importlib.util
import shutil
import sys
from pathlib import Path


SCRIPT_PATH = Path(__file__).resolve().parents[2] / "scripts" / "launch-worker.py"


def _load_module():
    spec = importlib.util.spec_from_file_location("launch_worker_under_test", SCRIPT_PATH)
    assert spec and spec.loader
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


def test_parse_args_defaults(monkeypatch):
    mod = _load_module()
    monkeypatch.setattr(sys, "argv", ["launch-worker.py", "arch-ctm"])
    args = mod._parse_args()
    assert args.agent_name == "arch-ctm"
    assert args.command == "codex --yolo"


def test_missing_atm_toml_is_passive_noop(monkeypatch, tmp_path):
    mod = _load_module()
    repo_root = tmp_path / "repo"
    repo_root.mkdir()
    monkeypatch.setenv("LAUNCH_REPO_ROOT", str(repo_root))
    monkeypatch.setattr(sys, "argv", ["launch-worker.py", "arch-ctm"])
    assert mod.main() == 0


def test_tmux_missing_returns_error(monkeypatch, tmp_path, capsys):
    mod = _load_module()
    repo_root = tmp_path / "repo"
    repo_root.mkdir()
    (repo_root / ".atm.toml").write_text('[core]\ndefault_team = "atm-dev"\n', encoding="utf-8")
    monkeypatch.setenv("LAUNCH_REPO_ROOT", str(repo_root))
    monkeypatch.setattr(sys, "argv", ["launch-worker.py", "arch-ctm"])

    original_which = shutil.which

    def fake_which(name):
        if name == "tmux":
            return None
        return original_which(name)

    monkeypatch.setattr(shutil, "which", fake_which)
    rc = mod.main()
    captured = capsys.readouterr()
    assert rc == 1
    assert "tmux is not installed" in captured.err
