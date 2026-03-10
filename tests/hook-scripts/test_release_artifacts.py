"""Unit tests for scripts/release_artifacts.py."""

from __future__ import annotations

import argparse
import importlib.util
import json
from pathlib import Path


SCRIPT_PATH = Path(__file__).resolve().parents[2] / "scripts" / "release_artifacts.py"


def _load_module():
    spec = importlib.util.spec_from_file_location("release_artifacts_under_test", SCRIPT_PATH)
    assert spec and spec.loader
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


def _write_manifest(tmp_path: Path) -> Path:
    manifest = tmp_path / "publish-artifacts.toml"
    manifest.write_text(
        """
schema_version = 1

[[crates]]
artifact = "b-crate"
package = "b-crate"
cargo_toml = "crates/b/Cargo.toml"
required = true
publish = true
publish_order = 20
preflight_check = "locked"
wait_after_publish_seconds = 60
verify_install = false

[[crates]]
artifact = "a-crate"
package = "a-crate"
cargo_toml = "crates/a/Cargo.toml"
required = true
publish = true
publish_order = 10
preflight_check = "full"
wait_after_publish_seconds = 0
verify_install = true

[[release_binaries]]
name = "atm"
""".strip()
        + "\n",
        encoding="utf-8",
    )
    return manifest


def test_emit_inventory_sorted_and_verify_install(tmp_path):
    mod = _load_module()
    manifest = _write_manifest(tmp_path)
    out_path = tmp_path / "release-inventory.json"
    args = argparse.Namespace(
        manifest=str(manifest),
        version="1.2.3",
        tag="v1.2.3",
        commit="1234567",
        source_ref="refs/tags/v1.2.3",
        generated_at="2026-03-08T00:00:00Z",
        output=str(out_path),
    )

    assert mod._cmd_emit_inventory(args) == 0

    payload = json.loads(out_path.read_text(encoding="utf-8"))
    assert [item["artifact"] for item in payload["items"]] == ["a-crate", "b-crate"]
    assert payload["items"][0]["verifyCommands"][-1].startswith("cargo install a-crate")


def test_list_publish_plan_filters_on_inventory_publish_flag(tmp_path, capsys):
    mod = _load_module()
    manifest = _write_manifest(tmp_path)
    inventory = tmp_path / "release-inventory.json"
    inventory.write_text(
        json.dumps(
            {
                "items": [
                    {"artifact": "a-crate", "version": "1.2.3", "publish": False},
                    {"artifact": "b-crate", "version": "1.2.3", "publish": True},
                ]
            }
        ),
        encoding="utf-8",
    )

    args = argparse.Namespace(
        manifest=str(manifest),
        inventory=str(inventory),
        version="1.2.3",
    )
    assert mod._cmd_list_publish_plan(args) == 0
    output = capsys.readouterr().out.strip().splitlines()
    assert output == ["b-crate|60"]


def test_check_version_unpublished_detects_existing_versions(tmp_path, monkeypatch):
    mod = _load_module()
    manifest = _write_manifest(tmp_path)
    monkeypatch.setattr(
        mod,
        "_cargo_search_version_exists",
        lambda crate, version: crate == "a-crate" and version == "1.2.3",
    )
    published = mod.check_version_unpublished(manifest, "1.2.3")
    assert published == ["a-crate"]


def test_check_version_unpublished_command_success(tmp_path, monkeypatch, capsys):
    mod = _load_module()
    manifest = _write_manifest(tmp_path)
    monkeypatch.setattr(mod, "_cargo_search_version_exists", lambda crate, version: False)
    args = argparse.Namespace(manifest=str(manifest), version="9.9.9")
    assert mod._cmd_check_version_unpublished(args) == 0
    assert "ok: no publishable artifacts found at version 9.9.9" in capsys.readouterr().out
