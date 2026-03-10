#!/usr/bin/env python3
"""Release artifact manifest utilities.

Single source of truth for crates + release binaries used by release workflows.
"""

from __future__ import annotations

import argparse
import json
import sys
import tomllib
import urllib.error
import urllib.request
from datetime import datetime, timezone
from pathlib import Path
from time import sleep

PUBLISH_TARGET = "crates.io"
PREFLIGHT_FULL = "full"
PREFLIGHT_LOCKED = "locked"


def _load_manifest(path: Path) -> dict:
    data = tomllib.loads(path.read_text(encoding="utf-8"))

    schema_version = data.get("schema_version")
    if schema_version != 1:
        raise SystemExit(f"unsupported manifest schema_version: {schema_version!r}")

    crates = data.get("crates")
    if not isinstance(crates, list) or not crates:
        raise SystemExit("manifest must define non-empty [[crates]] list")

    release_binaries = data.get("release_binaries")
    if not isinstance(release_binaries, list) or not release_binaries:
        raise SystemExit("manifest must define non-empty [[release_binaries]] list")

    _validate_crates(crates)
    _validate_binaries(release_binaries)

    return {
        "crates": sorted(crates, key=lambda item: (item["publish_order"], item["artifact"])),
        "release_binaries": release_binaries,
    }


def _validate_crates(crates: list[dict]) -> None:
    seen_artifacts: set[str] = set()
    seen_packages: set[str] = set()
    seen_paths: set[str] = set()

    required_fields = {
        "artifact",
        "package",
        "cargo_toml",
        "required",
        "publish",
        "publish_order",
        "preflight_check",
        "wait_after_publish_seconds",
        "verify_install",
    }
    for idx, item in enumerate(crates):
        if not isinstance(item, dict):
            raise SystemExit(f"crates[{idx}] must be a table")
        missing = sorted(required_fields - set(item.keys()))
        if missing:
            raise SystemExit(f"crates[{idx}] missing required fields: {', '.join(missing)}")

        artifact = _require_non_empty_str(item, "artifact", f"crates[{idx}]")
        package = _require_non_empty_str(item, "package", f"crates[{idx}]")
        cargo_toml = _require_non_empty_str(item, "cargo_toml", f"crates[{idx}]")
        preflight_check = _require_non_empty_str(item, "preflight_check", f"crates[{idx}]")

        if artifact in seen_artifacts:
            raise SystemExit(f"duplicate crate artifact in manifest: {artifact}")
        if package in seen_packages:
            raise SystemExit(f"duplicate crate package in manifest: {package}")
        if cargo_toml in seen_paths:
            raise SystemExit(f"duplicate crate cargo_toml in manifest: {cargo_toml}")
        seen_artifacts.add(artifact)
        seen_packages.add(package)
        seen_paths.add(cargo_toml)

        if not isinstance(item["required"], bool):
            raise SystemExit(f"{artifact}: required must be boolean")
        if not isinstance(item["publish"], bool):
            raise SystemExit(f"{artifact}: publish must be boolean")
        if not isinstance(item["verify_install"], bool):
            raise SystemExit(f"{artifact}: verify_install must be boolean")
        if not isinstance(item["publish_order"], int):
            raise SystemExit(f"{artifact}: publish_order must be integer")
        if not isinstance(item["wait_after_publish_seconds"], int):
            raise SystemExit(f"{artifact}: wait_after_publish_seconds must be integer")
        if item["wait_after_publish_seconds"] < 0:
            raise SystemExit(f"{artifact}: wait_after_publish_seconds must be >= 0")
        if preflight_check not in {PREFLIGHT_FULL, PREFLIGHT_LOCKED}:
            raise SystemExit(
                f"{artifact}: preflight_check must be '{PREFLIGHT_FULL}' or '{PREFLIGHT_LOCKED}'"
            )


def _validate_binaries(binaries: list[dict]) -> None:
    seen: set[str] = set()
    for idx, entry in enumerate(binaries):
        if not isinstance(entry, dict):
            raise SystemExit(f"release_binaries[{idx}] must be a table")
        name = _require_non_empty_str(entry, "name", f"release_binaries[{idx}]")
        if name in seen:
            raise SystemExit(f"duplicate release binary in manifest: {name}")
        seen.add(name)


def _require_non_empty_str(obj: dict, key: str, label: str) -> str:
    value = obj.get(key)
    if not isinstance(value, str) or not value.strip():
        raise SystemExit(f"{label}.{key} must be a non-empty string")
    return value


def _inventory_item(crate: dict, version: str, source_ref: str) -> dict:
    package = crate["package"]
    verify_commands = [
        f"cargo search {package} --limit 1 | grep -F '{package} = \"{version}\"'",
    ]
    if crate["verify_install"]:
        verify_commands.append(
            f"cargo install {package} --version {version} --locked --force",
        )
    return {
        "artifact": crate["artifact"],
        "version": version,
        "sourceRef": source_ref,
        "publishTarget": PUBLISH_TARGET,
        "publish": crate["publish"],
        "required": crate["required"],
        "verifyCommands": verify_commands,
    }


def _cmd_emit_inventory(args: argparse.Namespace) -> int:
    manifest = _load_manifest(Path(args.manifest))
    crates = manifest["crates"]
    generated_at = args.generated_at or datetime.now(timezone.utc).isoformat().replace("+00:00", "Z")

    inventory = {
        "releaseVersion": args.version,
        "releaseTag": args.tag,
        "releaseCommit": args.commit,
        "generatedAt": generated_at,
        "items": [_inventory_item(crate, args.version, args.source_ref) for crate in crates],
    }
    inventory["items"].sort(key=lambda item: item["artifact"])

    output_path = Path(args.output)
    output_path.parent.mkdir(parents=True, exist_ok=True)
    output_path.write_text(json.dumps(inventory, indent=2) + "\n", encoding="utf-8")
    return 0


def _cmd_list_cargo_tomls(args: argparse.Namespace) -> int:
    crates = _load_manifest(Path(args.manifest))["crates"]
    for crate in crates:
        print(crate["cargo_toml"])
    return 0


def _cmd_list_artifacts(args: argparse.Namespace) -> int:
    crates = _load_manifest(Path(args.manifest))["crates"]
    for crate in crates:
        if args.publishable_only and not crate["publish"]:
            continue
        print(crate["artifact"])
    return 0


def _cmd_list_preflight(args: argparse.Namespace) -> int:
    crates = _load_manifest(Path(args.manifest))["crates"]
    for crate in crates:
        if not crate["publish"]:
            continue
        if crate["preflight_check"] == args.mode:
            print(crate["package"])
    return 0


def _cmd_list_publish_plan(args: argparse.Namespace) -> int:
    manifest = _load_manifest(Path(args.manifest))
    crates = [crate for crate in manifest["crates"] if crate["publish"]]

    inventory = None
    if args.inventory:
        inventory = json.loads(Path(args.inventory).read_text(encoding="utf-8"))
        item_by_artifact = {
            item.get("artifact"): item
            for item in inventory.get("items", [])
            if isinstance(item, dict)
        }
        filtered: list[dict] = []
        for crate in crates:
            item = item_by_artifact.get(crate["artifact"])
            if item is None:
                raise SystemExit(f"inventory missing artifact: {crate['artifact']}")
            if args.version and item.get("version") != args.version:
                raise SystemExit(
                    f"{crate['artifact']}: inventory version mismatch for publish step",
                )
            if item.get("publish", True):
                filtered.append(crate)
        crates = filtered

    for crate in crates:
        print(f"{crate['package']}|{crate['wait_after_publish_seconds']}")
    return 0


def _cmd_list_release_binaries(args: argparse.Namespace) -> int:
    release_binaries = _load_manifest(Path(args.manifest))["release_binaries"]
    for entry in release_binaries:
        print(entry["name"])
    return 0


def _cmd_cargo_build_bin_args(args: argparse.Namespace) -> int:
    release_binaries = _load_manifest(Path(args.manifest))["release_binaries"]
    print(" ".join(f"--bin {entry['name']}" for entry in release_binaries))
    return 0


def _cratesio_version_exists(crate: str, version: str) -> bool:
    url = f"https://crates.io/api/v1/crates/{crate}/{version}"
    request = urllib.request.Request(
        url,
        headers={"User-Agent": "agent-team-mail-release-artifacts/1"},
        method="GET",
    )
    attempts = 3
    for attempt in range(1, attempts + 1):
        try:
            with urllib.request.urlopen(request) as response:
                return response.getcode() == 200
        except urllib.error.HTTPError as exc:
            if exc.code == 404:
                return False
            if attempt == attempts:
                raise SystemExit(f"{crate}@{version}: crates.io query failed with HTTP {exc.code}") from exc
            sleep(2)
        except urllib.error.URLError as exc:
            if attempt == attempts:
                raise SystemExit(f"{crate}@{version}: crates.io query failed ({exc.reason})") from exc
            sleep(2)
    return False


def check_version_unpublished(manifest_path: Path, version: str) -> list[str]:
    crates = _load_manifest(manifest_path)["crates"]
    published: list[str] = []
    for crate in crates:
        if not crate["publish"]:
            continue
        if _cratesio_version_exists(crate["package"], version):
            published.append(crate["artifact"])
    return published


def _cmd_check_version_unpublished(args: argparse.Namespace) -> int:
    published = check_version_unpublished(Path(args.manifest), args.version)
    if published:
        raise SystemExit(
            "release version already published for: " + ", ".join(sorted(published)),
        )
    print(f"ok: no publishable artifacts found at version {args.version}")
    return 0


def _workspace_members(workspace_toml: Path) -> list[str]:
    """Return the list of workspace member directory paths from the root Cargo.toml."""
    data = tomllib.loads(workspace_toml.read_text(encoding="utf-8"))
    members = data.get("workspace", {}).get("members", [])
    if not isinstance(members, list):
        raise SystemExit("Cargo.toml [workspace].members must be a list")
    return members


def _crate_is_publishable(crate_toml: Path) -> bool:
    """Return True unless the crate explicitly sets ``publish = false``."""
    data = tomllib.loads(crate_toml.read_text(encoding="utf-8"))
    publish = data.get("package", {}).get("publish")
    if publish is False:
        return False
    # publish = [] (empty list) also means "do not publish to crates.io"
    if isinstance(publish, list) and len(publish) == 0:
        return False
    return True


def _crate_name(crate_toml: Path) -> str | None:
    """Return the crate package name, or None if unreadable."""
    data = tomllib.loads(crate_toml.read_text(encoding="utf-8"))
    return data.get("package", {}).get("name")


def _cmd_validate_manifest(args: argparse.Namespace) -> int:
    """Check that every publishable workspace crate is listed in the manifest.

    Exits with code 0 when all publishable crates are present.
    Prints ``MISSING: <crate-name>`` for each absent crate and exits 1.
    """
    workspace_toml = Path(args.workspace_toml)
    manifest_path = Path(args.manifest)
    workspace_root = workspace_toml.parent

    members = _workspace_members(workspace_toml)
    manifest = _load_manifest(manifest_path)
    manifest_packages: set[str] = {c["package"] for c in manifest["crates"]}

    missing: list[str] = []
    for member in members:
        crate_toml = workspace_root / member / "Cargo.toml"
        if not crate_toml.exists():
            print(f"WARNING: workspace member {member!r} has no Cargo.toml at {crate_toml}")
            continue
        if not _crate_is_publishable(crate_toml):
            continue
        name = _crate_name(crate_toml)
        if name is None:
            print(f"WARNING: could not determine package name for {crate_toml}")
            continue
        if name not in manifest_packages:
            print(f"MISSING: {name}")
            missing.append(name)

    if missing:
        print(
            f"\n{len(missing)} publishable crate(s) missing from {manifest_path}.",
            file=sys.stderr,
        )
        return 1
    print(f"ok: all publishable workspace crates are present in {manifest_path}")
    return 0


def _has_workspace_path_deps(crate_toml: Path, workspace_root: Path) -> list[str]:
    """Return a list of dependency names that are workspace path dependencies.

    A dependency is a workspace path dep when its entry in either
    ``[dependencies]`` or the resolved ``[workspace.dependencies]`` contains a
    ``path`` key that resolves to a directory inside the workspace root.
    """
    data = tomllib.loads(crate_toml.read_text(encoding="utf-8"))
    crate_dir = crate_toml.parent

    # Also load workspace deps for resolution.
    ws_toml = workspace_root / "Cargo.toml"
    ws_data = tomllib.loads(ws_toml.read_text(encoding="utf-8")) if ws_toml.exists() else {}
    workspace_deps: dict = ws_data.get("workspace", {}).get("dependencies", {})

    path_deps: list[str] = []

    def _check_dep_table(deps: dict) -> None:
        for dep_name, dep_spec in deps.items():
            if isinstance(dep_spec, dict):
                if "workspace" in dep_spec:
                    # Resolve via workspace.dependencies
                    ws_dep = workspace_deps.get(dep_name, {})
                    if isinstance(ws_dep, dict) and "path" in ws_dep:
                        dep_path = (workspace_root / ws_dep["path"]).resolve()
                        if dep_path.is_relative_to(workspace_root.resolve()):
                            path_deps.append(dep_name)
                elif "path" in dep_spec:
                    dep_path = (crate_dir / dep_spec["path"]).resolve()
                    if dep_path.is_relative_to(workspace_root.resolve()):
                        path_deps.append(dep_name)

    _check_dep_table(data.get("dependencies", {}))
    # Also check target-specific deps
    for _target, target_data in data.get("target", {}).items():
        if isinstance(target_data, dict):
            _check_dep_table(target_data.get("dependencies", {}))

    return path_deps


def _cmd_validate_preflight_checks(args: argparse.Namespace) -> int:
    """Validate that crates with ``preflight_check = 'full'`` have no workspace path deps.

    A crate using ``full`` preflight is compiled and tested from source without
    ``--locked``.  If it depends on another workspace crate via a ``path``
    dependency, the local (unpublished) version would be used instead of the
    crates.io version, making the check meaningless.  Such crates must use
    ``preflight_check = 'locked'`` instead.

    Exits 0 when all full-preflight crates are genuine leaf crates.
    Exits 1 and prints errors for any violations.
    """
    manifest_path = Path(args.manifest)
    workspace_toml = Path(args.workspace_toml)
    workspace_root = workspace_toml.parent

    manifest = _load_manifest(manifest_path)
    full_crates = [c for c in manifest["crates"] if c["preflight_check"] == PREFLIGHT_FULL]

    errors: list[str] = []
    for crate in full_crates:
        crate_toml = workspace_root / crate["cargo_toml"]
        if not crate_toml.exists():
            errors.append(
                f"ERROR: {crate['artifact']}: cargo_toml not found at {crate_toml}"
            )
            continue
        path_deps = _has_workspace_path_deps(crate_toml, workspace_root)
        if path_deps:
            errors.append(
                f"ERROR: {crate['artifact']} has workspace path deps "
                f"({', '.join(sorted(path_deps))}) but preflight_check='full' "
                f"— must use 'locked'"
            )

    for err in errors:
        print(err)

    if errors:
        print(
            f"\n{len(errors)} preflight_check violation(s) found.",
            file=sys.stderr,
        )
        return 1
    print("ok: all preflight_check='full' crates are genuine leaf crates")
    return 0


def _build_parser() -> argparse.ArgumentParser:
    parser = argparse.ArgumentParser(description="Release artifact manifest utilities")
    subparsers = parser.add_subparsers(dest="command", required=True)

    emit_inventory = subparsers.add_parser("emit-inventory", help="Generate release inventory JSON")
    emit_inventory.add_argument("--manifest", required=True)
    emit_inventory.add_argument("--version", required=True)
    emit_inventory.add_argument("--tag", required=True)
    emit_inventory.add_argument("--commit", required=True)
    emit_inventory.add_argument("--source-ref", required=True)
    emit_inventory.add_argument("--generated-at", required=False)
    emit_inventory.add_argument("--output", required=True)
    emit_inventory.set_defaults(func=_cmd_emit_inventory)

    list_tomls = subparsers.add_parser("list-cargo-tomls", help="List crate Cargo.toml paths")
    list_tomls.add_argument("--manifest", required=True)
    list_tomls.set_defaults(func=_cmd_list_cargo_tomls)

    list_artifacts = subparsers.add_parser("list-artifacts", help="List crate artifact names")
    list_artifacts.add_argument("--manifest", required=True)
    list_artifacts.add_argument("--publishable-only", action="store_true")
    list_artifacts.set_defaults(func=_cmd_list_artifacts)

    list_preflight = subparsers.add_parser(
        "list-preflight",
        help="List crates by preflight mode",
    )
    list_preflight.add_argument("--manifest", required=True)
    list_preflight.add_argument("--mode", required=True, choices=[PREFLIGHT_FULL, PREFLIGHT_LOCKED])
    list_preflight.set_defaults(func=_cmd_list_preflight)

    list_publish_plan = subparsers.add_parser(
        "list-publish-plan",
        help="List publish plan as package|wait_after_publish_seconds",
    )
    list_publish_plan.add_argument("--manifest", required=True)
    list_publish_plan.add_argument("--inventory", required=False)
    list_publish_plan.add_argument("--version", required=False)
    list_publish_plan.set_defaults(func=_cmd_list_publish_plan)

    list_release_bins = subparsers.add_parser(
        "list-release-binaries",
        help="List release binaries",
    )
    list_release_bins.add_argument("--manifest", required=True)
    list_release_bins.set_defaults(func=_cmd_list_release_binaries)

    cargo_build_bin_args = subparsers.add_parser(
        "cargo-build-bin-args",
        help="Emit cargo build --bin args for release binaries",
    )
    cargo_build_bin_args.add_argument("--manifest", required=True)
    cargo_build_bin_args.set_defaults(func=_cmd_cargo_build_bin_args)

    check_unpublished = subparsers.add_parser(
        "check-version-unpublished",
        help="Fail when any publishable artifact version already exists on crates.io",
    )
    check_unpublished.add_argument("--manifest", required=True)
    check_unpublished.add_argument("--version", required=True)
    check_unpublished.set_defaults(func=_cmd_check_version_unpublished)

    validate_manifest = subparsers.add_parser(
        "validate-manifest",
        help=(
            "Fail when any publishable workspace crate is missing from "
            "the publish-artifacts manifest"
        ),
    )
    validate_manifest.add_argument(
        "--manifest",
        required=True,
        help="Path to publish-artifacts.toml",
    )
    validate_manifest.add_argument(
        "--workspace-toml",
        required=True,
        help="Path to workspace root Cargo.toml",
    )
    validate_manifest.set_defaults(func=_cmd_validate_manifest)

    validate_preflight = subparsers.add_parser(
        "validate-preflight-checks",
        help=(
            "Fail when a crate with preflight_check='full' has workspace "
            "path dependencies (it must use 'locked' instead)"
        ),
    )
    validate_preflight.add_argument(
        "--manifest",
        required=True,
        help="Path to publish-artifacts.toml",
    )
    validate_preflight.add_argument(
        "--workspace-toml",
        required=True,
        help="Path to workspace root Cargo.toml",
    )
    validate_preflight.set_defaults(func=_cmd_validate_preflight_checks)

    return parser


def main() -> int:
    parser = _build_parser()
    args = parser.parse_args()
    return args.func(args)


if __name__ == "__main__":
    sys.exit(main())
