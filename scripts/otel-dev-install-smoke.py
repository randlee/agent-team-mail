#!/usr/bin/env python3
"""Collector-backed OTel smoke for the installed dev binaries.

This script is intended to run after `scripts/dev-install` has updated the
shared dev channel. It verifies two AV.5 requirements:

1. Installed ATM binaries can export to a live OTLP/HTTP collector while
   preserving the local canonical log and `.otel.jsonl` mirror.
2. Collector outage remains fail-open: commands still succeed and local logging
   continues even when the collector is unavailable.
"""

from __future__ import annotations

import argparse
import http.server
import json
import os
import pathlib
import socket
import subprocess
import sys
import tempfile
import threading
import time


ROOT = pathlib.Path(__file__).resolve().parents[1]
DEFAULT_DEV_BIN = pathlib.Path.home() / ".local" / "atm-dev" / "bin"
DEFAULT_SHARED_HOME = pathlib.Path.home() / ".local" / "share" / "atm-dev" / "home"


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser(
        description="Collector-backed OTel smoke for installed dev binaries"
    )
    parser.add_argument(
        "--dry-run",
        action="store_true",
        help="Print resolved shared-dev inputs without running the smoke",
    )
    return parser.parse_args()


def resolve_dev_bin() -> pathlib.Path:
    daemon_bin = os.environ.get("ATM_DAEMON_BIN", "").strip()
    if daemon_bin:
        return pathlib.Path(daemon_bin).expanduser().resolve().parent
    return DEFAULT_DEV_BIN


def require_binary(path: pathlib.Path) -> pathlib.Path:
    if not path.exists():
        raise SystemExit(f"missing required installed binary: {path}")
    return path


def run(cmd: list[str], env: dict[str, str], cwd: pathlib.Path | None = None) -> subprocess.CompletedProcess[str]:
    return subprocess.run(
        cmd,
        cwd=str(cwd or ROOT),
        env=env,
        capture_output=True,
        text=True,
        timeout=30,
    )


def read_json_lines(path: pathlib.Path) -> list[dict]:
    if not path.exists():
        return []
    lines = []
    for raw in path.read_text().splitlines():
        raw = raw.strip()
        if raw:
            lines.append(json.loads(raw))
    return lines


class CollectorServer:
    def __init__(self) -> None:
        self.requests: list[str] = []
        self._lock = threading.Lock()
        self._server = http.server.ThreadingHTTPServer(("127.0.0.1", 0), self._handler())
        self.thread = threading.Thread(target=self._server.serve_forever, daemon=True)

    def _handler(self):
        outer = self

        class Handler(http.server.BaseHTTPRequestHandler):
            def do_POST(self) -> None:  # noqa: N802
                length = int(self.headers.get("Content-Length", "0"))
                body = self.rfile.read(length).decode("utf-8")
                with outer._lock:
                    outer.requests.append(body)
                self.send_response(200)
                self.send_header("Content-Length", "0")
                self.end_headers()

            def log_message(self, format: str, *args) -> None:  # noqa: A003
                return

        return Handler

    @property
    def endpoint(self) -> str:
        host, port = self._server.server_address
        return f"http://{host}:{port}"

    def start(self) -> None:
        self.thread.start()

    def stop(self) -> None:
        self._server.shutdown()
        self._server.server_close()
        self.thread.join(timeout=5)

    def payloads(self) -> list[str]:
        with self._lock:
            return list(self.requests)


def closed_local_endpoint() -> str:
    with socket.socket(socket.AF_INET, socket.SOCK_STREAM) as sock:
        sock.bind(("127.0.0.1", 0))
        host, port = sock.getsockname()
    return f"http://{host}:{port}"


def ensure_contains(payloads: list[str], needle: str, label: str) -> None:
    if not any(needle in payload for payload in payloads):
        raise SystemExit(f"{label}: missing `{needle}` in collector payloads")


def resolve_atm_home(dev_bin: pathlib.Path, env: dict[str, str]) -> pathlib.Path | None:
    atm_home = env.get("ATM_HOME", "").strip()
    if atm_home:
        return pathlib.Path(atm_home).expanduser().resolve()
    if dev_bin.resolve() == DEFAULT_DEV_BIN.resolve():
        return DEFAULT_SHARED_HOME
    return None


def shared_dev_mode(dev_bin: pathlib.Path, env: dict[str, str]) -> bool:
    resolved_home = resolve_atm_home(dev_bin, env)
    if "ATM_HOME" in env and env["ATM_HOME"].strip():
        return resolved_home == DEFAULT_SHARED_HOME.resolve()
    return dev_bin.resolve() == DEFAULT_DEV_BIN.resolve()


def canonical_log_paths(home_dir: pathlib.Path) -> tuple[pathlib.Path, pathlib.Path]:
    atm_log = home_dir / ".config" / "atm" / "logs" / "atm" / "atm.log.jsonl"
    sc_compose_log = (
        home_dir / ".config" / "sc-compose" / "logs" / "sc-compose.log"
    )
    return atm_log, sc_compose_log


def count_events(path: pathlib.Path) -> int:
    return len(read_json_lines(path))


def wait_for_count_increase(path: pathlib.Path, baseline: int, label: str) -> bool:
    deadline = time.time() + 5
    while time.time() < deadline:
        if path.exists() and count_events(path) > baseline:
            return True
        time.sleep(0.1)
    return False


def build_base_env(dev_bin: pathlib.Path) -> dict[str, str]:
    env = os.environ.copy()
    env["PATH"] = f"{dev_bin}{os.pathsep}{env.get('PATH', '')}"
    env["ATM_OTEL_ENABLED"] = "true"
    return env


def run_sc_compose(sc_compose_bin: pathlib.Path, env: dict[str, str], workspace: pathlib.Path) -> subprocess.CompletedProcess[str]:
    include = workspace / "include.md.j2"
    template = workspace / "template.md.j2"
    include.write_text("included {{ name }}")
    template.write_text("@<include.md.j2>\nhello {{ name }}")
    return run(
        [
            str(sc_compose_bin),
            "--root",
            str(workspace),
            "--var",
            "name=Kai",
            "render",
            str(template),
        ],
        env,
        cwd=workspace,
    )


def main() -> int:
    args = parse_args()
    dev_bin = resolve_dev_bin()
    atm_bin = require_binary(dev_bin / "atm")
    sc_compose_bin = require_binary(dev_bin / "sc-compose")

    if args.dry_run:
        env = build_base_env(dev_bin)
        resolved_home = resolve_atm_home(dev_bin, env)
        print(
            json.dumps(
                {
                    "dev_bin": str(dev_bin),
                    "atm_bin": str(atm_bin),
                    "sc_compose_bin": str(sc_compose_bin),
                    "shared_dev_mode": shared_dev_mode(dev_bin, env),
                    "resolved_atm_home": str(resolved_home) if resolved_home else None,
                },
                indent=2,
            )
        )
        return 0

    with tempfile.TemporaryDirectory(prefix="av5-otel-smoke-") as tmpdir:
        root = pathlib.Path(tmpdir)
        collector = CollectorServer()
        collector.start()

        live_env = build_base_env(dev_bin)
        live_env["ATM_OTEL_ENDPOINT"] = collector.endpoint
        live_shared_dev = shared_dev_mode(dev_bin, live_env)
        if live_shared_dev:
            resolved_home = resolve_atm_home(dev_bin, live_env)
            if resolved_home is None:
                raise SystemExit("shared-dev smoke: failed to resolve ATM_HOME")
            live_env["ATM_HOME"] = str(resolved_home)
            atm_log, sc_log = canonical_log_paths(resolved_home)
        else:
            atm_log = root / "atm.log.jsonl"
            sc_log = root / "sc-compose.log"
            live_env["ATM_LOG_FILE"] = str(atm_log)
            live_env["SC_COMPOSE_LOG_FILE"] = str(sc_log)
        live_atm_before = count_events(atm_log)
        live_atm_otel_before = count_events(atm_log.with_suffix(".otel.jsonl"))
        live_sc_before = count_events(sc_log)
        live_sc_otel_before = count_events(sc_log.with_suffix(".otel.jsonl"))

        atm_result = run([str(atm_bin), "config", "--json"], live_env)
        sc_compose_result = run_sc_compose(sc_compose_bin, live_env, root)
        time.sleep(0.3)
        collector.stop()

        if atm_result.returncode != 0:
            raise SystemExit(f"live collector atm command failed: {atm_result.stderr.strip()}")
        if sc_compose_result.returncode != 0:
            raise SystemExit(
                f"live collector sc-compose failed: {sc_compose_result.stderr.strip()}"
            )

        payloads = collector.payloads()
        ensure_contains(payloads, "command_start", "live collector smoke")
        ensure_contains(payloads, "compose", "live collector smoke")

        if not atm_log.exists():
            raise SystemExit("live collector smoke: ATM local log missing")
        if not sc_log.exists():
            raise SystemExit("live collector smoke: sc-compose local log missing")
        if not atm_log.with_suffix(".otel.jsonl").exists():
            raise SystemExit("live collector smoke: atm .otel.jsonl mirror missing")
        if not sc_log.with_suffix(".otel.jsonl").exists():
            raise SystemExit("live collector smoke: sc-compose .otel.jsonl mirror missing")
        live_atm_log_advanced = wait_for_count_increase(
            atm_log, live_atm_before, "live collector smoke: atm local log"
        )
        live_atm_otel_advanced = wait_for_count_increase(
            atm_log.with_suffix(".otel.jsonl"),
            live_atm_otel_before,
            "live collector smoke: atm .otel.jsonl mirror",
        )
        if not live_atm_log_advanced:
            raise SystemExit("live collector smoke: atm local log did not receive a new event")
        if not live_atm_otel_advanced:
            raise SystemExit("live collector smoke: atm .otel.jsonl mirror did not advance")
        if not wait_for_count_increase(
            sc_log, live_sc_before, "live collector smoke: sc-compose local log"
        ):
            raise SystemExit(
                "live collector smoke: sc-compose local log did not receive a new event"
            )
        if not wait_for_count_increase(
            sc_log.with_suffix(".otel.jsonl"),
            live_sc_otel_before,
            "live collector smoke: sc-compose .otel.jsonl mirror",
        ):
            raise SystemExit(
                "live collector smoke: sc-compose .otel.jsonl mirror did not advance"
            )

        outage_env = build_base_env(dev_bin)
        outage_env["ATM_OTEL_ENDPOINT"] = closed_local_endpoint()
        outage_shared_dev = shared_dev_mode(dev_bin, outage_env)
        if outage_shared_dev:
            resolved_home = resolve_atm_home(dev_bin, outage_env)
            if resolved_home is None:
                raise SystemExit("outage smoke: failed to resolve ATM_HOME")
            outage_env["ATM_HOME"] = str(resolved_home)
            outage_atm_log, outage_sc_log = canonical_log_paths(resolved_home)
        else:
            outage_atm_log = root / "atm-outage.log.jsonl"
            outage_sc_log = root / "sc-compose-outage.log"
            outage_env["ATM_LOG_FILE"] = str(outage_atm_log)
            outage_env["SC_COMPOSE_LOG_FILE"] = str(outage_sc_log)
        outage_atm_before = count_events(outage_atm_log)
        outage_sc_before = count_events(outage_sc_log)

        outage_atm = run([str(atm_bin), "config", "--json"], outage_env)
        outage_sc = run_sc_compose(sc_compose_bin, outage_env, root)

        if outage_atm.returncode != 0:
            raise SystemExit(f"outage smoke atm command failed: {outage_atm.stderr.strip()}")
        if outage_sc.returncode != 0:
            raise SystemExit(f"outage smoke sc-compose failed: {outage_sc.stderr.strip()}")

        if not outage_atm_log.exists():
            raise SystemExit("outage smoke: ATM local log missing")
        if not outage_sc_log.exists():
            raise SystemExit("outage smoke: sc-compose local log missing")
        if not wait_for_count_increase(
            outage_atm_log, outage_atm_before, "outage smoke: ATM local log"
        ):
            raise SystemExit("outage smoke: ATM local log did not receive a new event")
        if not wait_for_count_increase(
            outage_sc_log, outage_sc_before, "outage smoke: sc-compose local log"
        ):
            raise SystemExit("outage smoke: sc-compose local log did not receive a new event")

        summary = {
            "collector_endpoint": collector.endpoint,
            "live_payloads": len(payloads),
            "atm_log_events": len(read_json_lines(atm_log)),
            "atm_otel_events": len(read_json_lines(atm_log.with_suffix(".otel.jsonl"))),
            "sc_compose_otel_events": len(
                read_json_lines(sc_log.with_suffix(".otel.jsonl"))
            ),
            "outage_endpoint": outage_env["ATM_OTEL_ENDPOINT"],
            "status": "PASS",
        }
        print(json.dumps(summary, indent=2))
        return 0


if __name__ == "__main__":
    sys.exit(main())
