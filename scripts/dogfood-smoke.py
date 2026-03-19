#!/usr/bin/env python3
"""Live Grafana dogfood smoke for AY.2.

This script verifies the current shared dev-daemon flow against real Loki,
Tempo, and Mimir read endpoints using the operator's configured Grafana Cloud
credentials. It assumes OTLP write auth is already configured in the shell.
"""

from __future__ import annotations

import argparse
import json
import os
import pathlib
import subprocess
import sys
import time
import urllib.parse
import urllib.request
import uuid


DEFAULT_ATM_BIN = pathlib.Path.home() / ".local" / "atm-dev" / "bin" / "atm"
DEFAULT_ATM_HOME = pathlib.Path.home() / ".local" / "share" / "atm-dev" / "home"
ROOT = pathlib.Path(__file__).resolve().parents[1]
QUERY_WAIT_SECONDS = 45
QUERY_POLL_SECONDS = 3


def env_nonempty(name: str) -> str | None:
    value = os.environ.get(name, "").strip()
    return value or None


def require_env(name: str) -> str:
    value = env_nonempty(name)
    if value is None:
        raise SystemExit(f"missing required environment variable: {name}")
    return value


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--dry-run", action="store_true")
    parser.add_argument("--atm-bin", default=str(DEFAULT_ATM_BIN))
    parser.add_argument("--team", default=os.environ.get("ATM_TEAM", "atm-dev"))
    parser.add_argument("--agent", default=os.environ.get("ATM_IDENTITY", "arch-ctm"))
    parser.add_argument("--runtime", default=os.environ.get("ATM_RUNTIME", "codex"))
    parser.add_argument(
        "--session-id",
        default=f"ay2-dogfood-{uuid.uuid4()}",
        help="session identifier injected into CLI and shared-daemon smoke flows",
    )
    parser.add_argument("--wait-seconds", type=int, default=QUERY_WAIT_SECONDS)
    return parser.parse_args()


def parse_auth_header(raw: str) -> tuple[str, str]:
    if ":" not in raw:
        raise SystemExit(f"invalid auth header format: {raw!r}")
    name, value = raw.split(":", 1)
    return name.strip(), value.strip()


def run_command(cmd: list[str], env: dict[str, str], check: bool = True) -> subprocess.CompletedProcess[str]:
    result = subprocess.run(
        cmd,
        cwd=str(ROOT),
        env=env,
        capture_output=True,
        text=True,
        timeout=60,
    )
    if check and result.returncode != 0:
        raise SystemExit(
            json.dumps(
                {
                    "status": "FAIL",
                    "reason": "command failed",
                    "cmd": cmd,
                    "returncode": result.returncode,
                    "stdout": result.stdout,
                    "stderr": result.stderr,
                },
                indent=2,
            )
        )
    return result


def http_get_json(
    endpoint: str,
    auth_header: tuple[str, str] | None,
    params: dict[str, str],
) -> dict:
    url = f"{endpoint}?{urllib.parse.urlencode(params)}"
    request = urllib.request.Request(url)
    if auth_header is not None:
        request.add_header(auth_header[0], auth_header[1])
    with urllib.request.urlopen(request, timeout=30) as response:  # noqa: S310
        return json.loads(response.read().decode("utf-8"))


def normalize_tempo_search_endpoint(endpoint: str) -> str:
    endpoint = endpoint.rstrip("/")
    if endpoint.endswith("/api/search"):
        return endpoint
    if endpoint.endswith("/tempo"):
        return f"{endpoint}/api/search"
    return f"{endpoint}/api/search"


def loki_query(endpoint: str, auth: tuple[str, str], query: str) -> dict:
    end_ns = time.time_ns()
    start_ns = end_ns - (600 * 1_000_000_000)
    return http_get_json(
        f"{endpoint.rstrip('/')}/loki/api/v1/query_range",
        auth,
        {
            "query": query,
            "start": str(start_ns),
            "end": str(end_ns),
            "limit": "50",
            "direction": "forward",
        },
    )


def tempo_query(endpoint: str, auth: tuple[str, str], query: str) -> dict:
    return http_get_json(
        normalize_tempo_search_endpoint(endpoint),
        auth,
        {"q": query, "limit": "20"},
    )


def mimir_query(endpoint: str, auth: tuple[str, str], query: str) -> dict:
    return http_get_json(
        f"{endpoint.rstrip('/')}/api/v1/query",
        auth,
        {"query": query},
    )


def query_count(response: dict) -> int:
    traces = response.get("traces")
    if isinstance(traces, list):
        return len(traces)
    data = response.get("data", {})
    result = data.get("result", [])
    return len(result) if isinstance(result, list) else 0


def build_smoke_env(args: argparse.Namespace) -> dict[str, str]:
    env = os.environ.copy()
    atm_bin = pathlib.Path(args.atm_bin).expanduser().resolve()
    env["ATM_OTEL_ENABLED"] = "true"
    env.setdefault("ATM_OTEL_PROTOCOL", "otlp_http")
    env["ATM_TEAM"] = args.team
    env["ATM_IDENTITY"] = args.agent
    env["ATM_RUNTIME"] = args.runtime
    env["CLAUDE_SESSION_ID"] = args.session_id
    env["ATM_SESSION_ID"] = args.session_id
    env.setdefault("ATM_HOME", str(DEFAULT_ATM_HOME))
    env.setdefault("ATM_DAEMON_BIN", str(atm_bin.parent / "atm-daemon"))
    env["ATM_OTEL_ENDPOINT"] = require_env("ATM_OTEL_ENDPOINT")
    auth = env_nonempty("ATM_OTEL_AUTH_HEADER")
    if auth is not None:
        env["ATM_OTEL_AUTH_HEADER"] = auth
    return env


def main() -> int:
    args = parse_args()
    atm_bin = pathlib.Path(args.atm_bin).expanduser().resolve()
    if not atm_bin.exists():
        raise SystemExit(f"missing atm binary: {atm_bin}")

    loki_auth = parse_auth_header(require_env("ATM_LOKI_READ_AUTH"))
    tempo_auth = parse_auth_header(require_env("ATM_TEMPO_READ_AUTH"))
    mimir_auth = parse_auth_header(require_env("ATM_MIMIR_READ_AUTH"))
    loki_url = require_env("ATM_LOKI_URL")
    tempo_url = require_env("ATM_TEMPO_SEARCH_ENDPOINT")
    mimir_url = require_env("ATM_MIMIR_QUERY_ENDPOINT")

    if args.dry_run:
        print(
            json.dumps(
                {
                    "status": "DRY_RUN",
                    "atm_bin": str(atm_bin),
                    "atm_home": str(DEFAULT_ATM_HOME),
                    "session_id": args.session_id,
                    "commands": [
                        [str(atm_bin), "daemon", "stop"],
                        [str(atm_bin), "daemon", "restart"],
                        [str(atm_bin), "config", "--json"],
                    ],
                    "queries": {
                        "loki": f'{{service_name="atm"}} | session_id="{args.session_id}"',
                        "tempo": (
                            '{ resource.service.name = "atm-daemon" && '
                            f'resource.session_id = "{args.session_id}" }}'
                        ),
                        "mimir": f'atm_commands_count_total{{session_id="{args.session_id}"}}',
                    },
                },
                indent=2,
            )
        )
        return 0

    env = build_smoke_env(args)

    run_command([str(atm_bin), "daemon", "stop"], env, check=False)
    run_command([str(atm_bin), "daemon", "restart"], env)
    run_command([str(atm_bin), "config", "--json"], env)
    deadline = time.time() + max(args.wait_seconds, 1)
    summary = None
    while True:
        loki_response = loki_query(
            loki_url,
            loki_auth,
            f'{{service_name="atm"}} | session_id="{args.session_id}"',
        )
        tempo_response = tempo_query(
            tempo_url,
            tempo_auth,
            '{ resource.service.name = "atm-daemon" && '
            f'resource.session_id = "{args.session_id}" }}',
        )
        mimir_response = mimir_query(
            mimir_url,
            mimir_auth,
            f'atm_commands_count_total{{session_id="{args.session_id}"}}',
        )

        summary = {
            "session_id": args.session_id,
            "loki_streams": query_count(loki_response),
            "tempo_traces": query_count(tempo_response),
            "mimir_series": query_count(mimir_response),
            "status": "PASS",
        }
        if (
            summary["loki_streams"] >= 1
            and summary["tempo_traces"] >= 1
            and summary["mimir_series"] >= 1
        ):
            break
        if time.time() >= deadline:
            summary["status"] = "FAIL"
            summary["loki_response"] = loki_response
            summary["tempo_response"] = tempo_response
            summary["mimir_response"] = mimir_response
            print(json.dumps(summary, indent=2))
            return 1
        time.sleep(QUERY_POLL_SECONDS)

    print(json.dumps(summary, indent=2))
    return 0


if __name__ == "__main__":
    sys.exit(main())
