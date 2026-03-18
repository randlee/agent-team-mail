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
    dev_bin = resolve_dev_bin()
    atm_bin = require_binary(dev_bin / "atm")
    sc_compose_bin = require_binary(dev_bin / "sc-compose")

    with tempfile.TemporaryDirectory(prefix="av5-otel-smoke-") as tmpdir:
        root = pathlib.Path(tmpdir)
        collector = CollectorServer()
        collector.start()

        live_env = build_base_env(dev_bin)
        live_env["ATM_OTEL_ENDPOINT"] = collector.endpoint
        live_env["ATM_LOG_FILE"] = str(root / "atm.log.jsonl")
        live_env["SC_COMPOSE_LOG_FILE"] = str(root / "sc-compose.log")

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

        atm_log = root / "atm.log.jsonl"
        sc_log = root / "sc-compose.log"
        if not atm_log.exists() or not sc_log.exists():
            raise SystemExit("live collector smoke: canonical local logs were not written")
        if not atm_log.with_suffix(".otel.jsonl").exists():
            raise SystemExit("live collector smoke: atm .otel.jsonl mirror missing")
        if not sc_log.with_suffix(".otel.jsonl").exists():
            raise SystemExit("live collector smoke: sc-compose .otel.jsonl mirror missing")

        outage_env = build_base_env(dev_bin)
        outage_env["ATM_OTEL_ENDPOINT"] = closed_local_endpoint()
        outage_env["ATM_LOG_FILE"] = str(root / "atm-outage.log.jsonl")
        outage_env["SC_COMPOSE_LOG_FILE"] = str(root / "sc-compose-outage.log")

        outage_atm = run([str(atm_bin), "config", "--json"], outage_env)
        outage_sc = run_sc_compose(sc_compose_bin, outage_env, root)

        if outage_atm.returncode != 0:
            raise SystemExit(f"outage smoke atm command failed: {outage_atm.stderr.strip()}")
        if outage_sc.returncode != 0:
            raise SystemExit(f"outage smoke sc-compose failed: {outage_sc.stderr.strip()}")

        if not pathlib.Path(outage_env["ATM_LOG_FILE"]).exists():
            raise SystemExit("outage smoke: ATM local log missing")
        if not pathlib.Path(outage_env["SC_COMPOSE_LOG_FILE"]).exists():
            raise SystemExit("outage smoke: sc-compose local log missing")

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
