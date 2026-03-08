"""Unit tests for scripts/spawn-teammate.py."""

from __future__ import annotations

import importlib.util
import sys
from pathlib import Path


SCRIPT_PATH = Path(__file__).resolve().parents[2] / "scripts" / "spawn-teammate.py"


def _load_module():
    spec = importlib.util.spec_from_file_location("spawn_teammate_under_test", SCRIPT_PATH)
    assert spec and spec.loader
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


def test_parse_args_defaults(monkeypatch):
    mod = _load_module()
    monkeypatch.setattr(sys, "argv", ["spawn-teammate.py", "arch-ctm", "atm-dev"])
    args = mod._parse_args()
    assert args.agent_name == "arch-ctm"
    assert args.team_name == "atm-dev"
    assert args.color == ""
    assert args.model == ""
    assert args.repo_root == ""


def test_no_atm_context_is_passive_noop(monkeypatch, tmp_path):
    mod = _load_module()
    repo_root = tmp_path / "repo"
    repo_root.mkdir()
    monkeypatch.setenv("ATM_TEAM", "")
    monkeypatch.setenv("ATM_IDENTITY", "")
    monkeypatch.setattr(sys, "argv", ["spawn-teammate.py", "arch-ctm", "atm-dev", "--repo-root", str(repo_root)])
    monkeypatch.setattr(mod, "_run", lambda *args, **kwargs: (_ for _ in ()).throw(AssertionError("_run must not be called")))
    assert mod.main() == 0


def test_error_when_claude_binary_missing(monkeypatch, tmp_path, capsys):
    mod = _load_module()
    repo_root = tmp_path / "repo"
    repo_root.mkdir()
    (repo_root / ".atm.toml").write_text('[core]\ndefault_team = "atm-dev"\nidentity = "arch-ctm"\n', encoding="utf-8")
    monkeypatch.setenv("ATM_TEAM", "")
    monkeypatch.setenv("ATM_IDENTITY", "")
    monkeypatch.setattr(sys, "argv", ["spawn-teammate.py", "arch-ctm", "atm-dev", "--repo-root", str(repo_root)])
    monkeypatch.setattr(mod, "_find_claude_binary", lambda: (_ for _ in ()).throw(RuntimeError("no claude binary")))
    rc = mod.main()
    captured = capsys.readouterr()
    assert rc == 1
    assert "no claude binary" in captured.err
