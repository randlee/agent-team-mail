#!/usr/bin/env python3
"""Codex notify relay for ATM daemon hook watcher.

Codex notify invokes this script with one JSON payload argument describing the
completed turn. This script enriches that payload with ATM routing context and
appends one JSON object line to the active ATM daemon home:
  ${ATM_HOME}/.atm/daemon/hooks/events.jsonl
or, when ATM_HOME is unset, the OS home directory.

It is intentionally fail-open and exits 0 for non-ATM contexts.
"""

from __future__ import annotations

import argparse
import json
import os
import sys
from datetime import datetime, timezone
from pathlib import Path
from typing import Any


def _atm_home() -> Path:
    explicit = os.environ.get("ATM_HOME", "").strip()
    if explicit:
        return Path(explicit)
    env_home = os.environ.get("HOME", "").strip()
    if env_home:
        return Path(env_home)
    return Path.home()


def _read_atm_toml() -> dict[str, Any] | None:
    toml_path = Path(".atm.toml")
    if not toml_path.exists():
        return None
    try:
        try:
            import tomllib
        except ImportError:  # pragma: no cover
            import tomli as tomllib  # type: ignore[no-redef]

        with toml_path.open("rb") as f:
            parsed = tomllib.load(f)
        return parsed if isinstance(parsed, dict) else None
    except Exception:
        return None


def _first_str(*values: Any) -> str | None:
    for value in values:
        if isinstance(value, str) and value.strip():
            return value.strip()
    return None


def _parse_payload(raw: str | None) -> dict[str, Any]:
    if raw and raw.strip():
        try:
            parsed = json.loads(raw)
            if isinstance(parsed, dict):
                return parsed
        except Exception:
            return {}
    return {}


def _append_event(event: dict[str, Any]) -> None:
    events_file = _atm_home() / ".atm" / "daemon" / "hooks" / "events.jsonl"
    events_file.parent.mkdir(parents=True, exist_ok=True)
    with events_file.open("a", encoding="utf-8") as f:
        f.write(json.dumps(event, separators=(",", ":")) + "\n")


def main() -> int:
    parser = argparse.ArgumentParser(add_help=False)
    parser.add_argument("--agent", default="")
    parser.add_argument("--team", default="")
    parser.add_argument("payload", nargs="?")
    args = parser.parse_args()

    payload = _parse_payload(args.payload)
    atm_config = _read_atm_toml()
    core = atm_config.get("core", {}) if isinstance(atm_config, dict) else {}

    team = _first_str(args.team, os.environ.get("ATM_TEAM"), payload.get("team"), core.get("default_team"))
    agent = _first_str(
        args.agent,
        os.environ.get("ATM_IDENTITY"),
        payload.get("agent"),
        core.get("identity"),
    )

    # Non-ATM context: no team/identity information available.
    if not team or not agent:
        return 0

    payload_type = _first_str(payload.get("type"), "agent-turn-complete") or "agent-turn-complete"
    turn_id = _first_str(payload.get("turn-id"), payload.get("turn_id"), payload.get("turnId"), "no-turn") or "no-turn"
    received_at = datetime.now(timezone.utc).strftime("%Y-%m-%dT%H:%M:%SZ")

    event = {
        "type": payload_type,
        "agent": agent,
        "team": team,
        "thread-id": payload.get("thread-id") or payload.get("thread_id"),
        "turn-id": payload.get("turn-id") or payload.get("turn_id") or payload.get("turnId"),
        "received_at": received_at,
        "timestamp": received_at,
        "state": "idle",
        "idempotency_key": f"{team}:{agent}:{turn_id}",
        "payload": payload,
    }

    try:
        _append_event(event)
    except Exception as exc:  # pragma: no cover - fail-open path
        sys.stderr.write(f"[atm-hook] failed to append relay event: {exc}\n")

    return 0


if __name__ == "__main__":
    raise SystemExit(main())
