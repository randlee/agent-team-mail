#!/usr/bin/env python3
"""Grafana-backed OTLP smoke for AW.5.

This script runs a small ATM command set with OTel enabled, then queries a Loki
read endpoint to verify the remote logs path exposes the expected service name
and correlation fields. It does not hardcode credentials; all collector and
Loki configuration is read from environment variables.
"""

from __future__ import annotations

import argparse
import json
import os
import pathlib
import subprocess
import sys
import tempfile
import time
import urllib.parse
import urllib.request
import uuid


ROOT = pathlib.Path(__file__).resolve().parents[1]
DEFAULT_ATM_BIN = pathlib.Path.home() / ".local" / "atm-dev" / "bin" / "atm"
QUERY_WINDOW_SECONDS = 300
QUERY_RETRY_SECONDS = 20
QUERY_POLL_INTERVAL = 2


def env_nonempty(name: str) -> str | None:
    value = os.environ.get(name, "").strip()
    return value or None


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--dry-run", action="store_true", help="print the planned actions without running commands or network calls")
    parser.add_argument("--atm-bin", default=str(DEFAULT_ATM_BIN), help="path to the ATM binary to exercise")
    parser.add_argument("--team", default=os.environ.get("ATM_TEAM", "atm-dev"), help="team value to inject for correlation")
    parser.add_argument("--agent", default=os.environ.get("ATM_IDENTITY", "arch-ctm"), help="agent value to inject for correlation")
    parser.add_argument("--runtime", default=os.environ.get("ATM_RUNTIME", "codex"), help="runtime value to inject for correlation")
    parser.add_argument("--session-id", default=f"aw5-smoke-{uuid.uuid4()}", help="session identifier to inject for correlation")
    return parser.parse_args()


def require_env(name: str) -> str:
    value = env_nonempty(name)
    if value is None:
        raise SystemExit(f"missing required environment variable: {name}")
    return value


def parse_auth_header(raw: str | None) -> tuple[str, str] | None:
    if raw is None:
        return None
    if ":" not in raw:
        raise SystemExit(f"invalid auth header format for value: {raw!r}")
    name, value = raw.split(":", 1)
    return name.strip(), value.strip()


def run_command(cmd: list[str], env: dict[str, str]) -> subprocess.CompletedProcess[str]:
    return subprocess.run(
        cmd,
        cwd=str(ROOT),
        env=env,
        capture_output=True,
        text=True,
        timeout=30,
    )


def build_smoke_env(args: argparse.Namespace, log_path: pathlib.Path, endpoint: str) -> dict[str, str]:
    env = os.environ.copy()
    env["ATM_OTEL_ENABLED"] = "true"
    env["ATM_OTEL_ENDPOINT"] = endpoint
    env.setdefault("ATM_OTEL_PROTOCOL", "otlp_http")
    env["ATM_LOG_FILE"] = str(log_path)
    env["ATM_TEAM"] = args.team
    env["ATM_IDENTITY"] = args.agent
    env["ATM_RUNTIME"] = args.runtime
    env["CLAUDE_SESSION_ID"] = args.session_id
    return env


def loki_query(endpoint: str, auth_header: tuple[str, str] | None, query: str, start_ns: int, end_ns: int) -> dict:
    params = urllib.parse.urlencode(
        {
            "query": query,
            "start": str(start_ns),
            "end": str(end_ns),
            "limit": "50",
            "direction": "forward",
        }
    )
    request = urllib.request.Request(f"{endpoint}?{params}")
    if auth_header is not None:
        request.add_header(auth_header[0], auth_header[1])
    with urllib.request.urlopen(request, timeout=15) as response:  # noqa: S310
        return json.loads(response.read().decode("utf-8"))


def find_match(result: dict, expected_service: str) -> dict | None:
    data = result.get("data", {})
    for stream in data.get("result", []):
        labels = stream.get("stream", {})
        service = labels.get("service_name") or labels.get("source_binary")
        if service != expected_service:
            continue
        return {
            "service_name": service,
            "labels": labels,
            "sample_count": len(stream.get("values", [])),
        }
    return None


def query_until_match(endpoint: str, auth_header: tuple[str, str] | None, selectors: list[str], expected_service: str) -> dict:
    deadline = time.time() + QUERY_RETRY_SECONDS
    last_results: list[dict] = []
    while time.time() < deadline:
        end_ns = time.time_ns()
        start_ns = end_ns - (QUERY_WINDOW_SECONDS * 1_000_000_000)
        for selector in selectors:
            result = loki_query(endpoint, auth_header, selector, start_ns, end_ns)
            last_results.append({"query": selector, "result": result})
            matched = find_match(result, expected_service)
            if matched is not None:
                return {
                    "query": selector,
                    "match": matched,
                }
        time.sleep(QUERY_POLL_INTERVAL)

    raise SystemExit(
        json.dumps(
            {
                "status": "FAIL",
                "reason": "no matching Loki stream found",
                "queries": [entry["query"] for entry in last_results[-4:]],
            },
            indent=2,
        )
    )


def main() -> int:
    args = parse_args()
    atm_bin = pathlib.Path(args.atm_bin).expanduser().resolve()

    selectors = [
        f'{{service_name="atm"}} | session_id="{args.session_id}"',
        f'{{service_name="atm"}} | command="config"',
        '{service_name="atm"}',
    ]

    if args.dry_run:
        print(
            json.dumps(
                {
                    "status": "DRY_RUN",
                    "atm_bin": str(atm_bin),
                    "required_env": [
                        "ATM_OTEL_ENDPOINT",
                        "ATM_LOKI_ENDPOINT",
                    ],
                    "optional_env": [
                        "ATM_OTEL_AUTH_HEADER",
                        "ATM_LOKI_AUTH_HEADER",
                    ],
                    "commands": [
                        [str(atm_bin), "config", "--json"],
                        [str(atm_bin), "doctor", "--json"],
                    ],
                    "selectors": selectors,
                    "correlation": {
                        "team": args.team,
                        "agent": args.agent,
                        "runtime": args.runtime,
                        "session_id": args.session_id,
                    },
                },
                indent=2,
            )
        )
        return 0

    if not atm_bin.exists():
        raise SystemExit(f"missing atm binary: {atm_bin}")

    otel_endpoint = require_env("ATM_OTEL_ENDPOINT")
    loki_endpoint = require_env("ATM_LOKI_ENDPOINT")
    otel_auth_header = env_nonempty("ATM_OTEL_AUTH_HEADER")
    loki_auth_header = parse_auth_header(env_nonempty("ATM_LOKI_AUTH_HEADER"))

    with tempfile.TemporaryDirectory(prefix="aw5-grafana-smoke-") as tmpdir:
        temp_root = pathlib.Path(tmpdir)
        log_path = temp_root / "atm.log.jsonl"
        env = build_smoke_env(args, log_path, otel_endpoint)
        if otel_auth_header is not None:
            env["ATM_OTEL_AUTH_HEADER"] = otel_auth_header

        commands = [
            [str(atm_bin), "config", "--json"],
            [str(atm_bin), "doctor", "--json"],
        ]
        results = []
        for cmd in commands:
            result = run_command(cmd, env)
            results.append(
                {
                    "cmd": cmd,
                    "returncode": result.returncode,
                    "stdout": result.stdout.strip(),
                    "stderr": result.stderr.strip(),
                }
            )
            if result.returncode != 0:
                raise SystemExit(json.dumps({"status": "FAIL", "reason": "command failed", "result": results[-1]}, indent=2))

        otel_mirror_path = log_path.with_suffix(".otel.jsonl")
        if not log_path.exists():
            raise SystemExit(
                json.dumps(
                    {
                        "status": "FAIL",
                        "reason": "canonical local log missing",
                        "path": str(log_path),
                    },
                    indent=2,
                )
            )
        if not otel_mirror_path.exists():
            raise SystemExit(
                json.dumps(
                    {
                        "status": "FAIL",
                        "reason": "local otel mirror missing",
                        "path": str(otel_mirror_path),
                    },
                    indent=2,
                )
            )

        query_result = query_until_match(
            loki_endpoint,
            loki_auth_header,
            selectors,
            expected_service="atm",
        )

        summary = {
            "status": "PASS",
            "commands": [{"cmd": r["cmd"], "returncode": r["returncode"]} for r in results],
            "otel_endpoint": otel_endpoint,
            "loki_endpoint": loki_endpoint,
            "service_name": query_result["match"]["service_name"],
            "matched_query": query_result["query"],
            "matched_labels": query_result["match"]["labels"],
            "sample_count": query_result["match"]["sample_count"],
            "local_log_path": str(log_path),
            "local_otel_mirror_path": str(otel_mirror_path),
        }
        print(json.dumps(summary, indent=2))
        return 0


if __name__ == "__main__":
    sys.exit(main())
