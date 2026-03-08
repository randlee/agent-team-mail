#!/usr/bin/env python3
"""PID parent-chain reporter with ATM session/task enrichment.

Usage examples:
  python3 scripts/pid_chain_report.py
  python3 scripts/pid_chain_report.py 27068
  python3 scripts/pid_chain_report.py 27068 17033 --team atm-dev --json
"""

from __future__ import annotations

import argparse
import json
import os
import subprocess
import sys
from dataclasses import dataclass
from pathlib import Path
from typing import Any


@dataclass
class ProcInfo:
    pid: int
    ppid: int
    user: str
    state: str
    elapsed: str
    cputime: str
    command: str


def run_ps(pid: int) -> ProcInfo | None:
    cmd = [
        "ps",
        "-o",
        "pid=,ppid=,user=,state=,etime=,time=,command=",
        "-p",
        str(pid),
    ]
    try:
        out = subprocess.check_output(cmd, text=True, stderr=subprocess.DEVNULL).strip()
    except Exception:
        return None

    if not out:
        return None

    parts = out.split(None, 6)
    if len(parts) < 7:
        return None

    try:
        parsed_pid = int(parts[0])
        parsed_ppid = int(parts[1])
    except ValueError:
        return None

    return ProcInfo(
        pid=parsed_pid,
        ppid=parsed_ppid,
        user=parts[2],
        state=parts[3],
        elapsed=parts[4],
        cputime=parts[5],
        command=parts[6],
    )


def load_json(path: Path) -> Any:
    try:
        return json.loads(path.read_text(encoding="utf-8"))
    except Exception:
        return None


def home_dir() -> Path:
    return Path.home()


def load_daemon_registry() -> dict[str, Any]:
    reg_path = home_dir() / ".claude" / "daemon" / "session-registry.json"
    data = load_json(reg_path)
    if not isinstance(data, dict):
        return {}
    sessions = data.get("sessions")
    if not isinstance(sessions, dict):
        return {}
    return sessions


def load_team_sessions(team: str) -> list[dict[str, Any]]:
    sessions_dir = home_dir() / ".claude" / "teams" / team / "sessions"
    if not sessions_dir.is_dir():
        return []

    out: list[dict[str, Any]] = []
    for p in sorted(sessions_dir.glob("*.json")):
        data = load_json(p)
        if isinstance(data, dict):
            data["_file"] = str(p)
            out.append(data)
    return out


def load_team_config(team: str) -> dict[str, Any] | None:
    cfg_path = home_dir() / ".claude" / "teams" / team / "config.json"
    data = load_json(cfg_path)
    return data if isinstance(data, dict) else None


def load_task_summary(team: str) -> dict[str, Any]:
    tasks_dir = home_dir() / ".claude" / "tasks" / team
    summary = {
        "team": team,
        "path": str(tasks_dir),
        "counts": {"pending": 0, "in_progress": 0, "completed": 0, "other": 0},
        "open": [],
    }
    if not tasks_dir.is_dir():
        return summary

    for p in sorted(tasks_dir.glob("*.json")):
        data = load_json(p)
        if not isinstance(data, dict):
            continue
        status = str(data.get("status", "other"))
        sid = str(data.get("id", p.stem))
        subject = str(data.get("subject", ""))
        if status in summary["counts"]:
            summary["counts"][status] += 1
        else:
            summary["counts"]["other"] += 1

        if status in ("pending", "in_progress"):
            summary["open"].append({"id": sid, "status": status, "subject": subject})

    return summary


def registry_matches_for_pid(registry: dict[str, Any], team: str, pid: int) -> list[dict[str, Any]]:
    matches: list[dict[str, Any]] = []
    for key, rec in registry.items():
        if not isinstance(rec, dict):
            continue
        if str(rec.get("team", "")) != team:
            continue
        if int(rec.get("process_id", -1)) != pid:
            continue
        matches.append(
            {
                "key": key,
                "agent_name": rec.get("agent_name"),
                "session_id": rec.get("session_id"),
                "state": rec.get("state"),
                "runtime": rec.get("runtime"),
                "updated_at": rec.get("updated_at"),
            }
        )
    return matches


def team_session_matches_for_pid(team_sessions: list[dict[str, Any]], pid: int) -> list[dict[str, Any]]:
    out: list[dict[str, Any]] = []
    for rec in team_sessions:
        try:
            rec_pid = int(rec.get("pid", -1))
        except Exception:
            rec_pid = -1
        if rec_pid != pid:
            continue
        out.append(
            {
                "file": rec.get("_file"),
                "session_id": rec.get("session_id"),
                "identity": rec.get("identity"),
                "team": rec.get("team"),
                "updated_at": rec.get("updated_at"),
            }
        )
    return out


def build_chain(root_pid: int) -> list[ProcInfo]:
    chain: list[ProcInfo] = []
    seen: set[int] = set()
    cursor = root_pid

    while cursor > 0 and cursor not in seen:
        seen.add(cursor)
        info = run_ps(cursor)
        if info is None:
            break
        chain.append(info)
        if info.ppid <= 0 or info.ppid == info.pid:
            break
        cursor = info.ppid

    return chain


def guess_default_team() -> str:
    env_team = os.environ.get("ATM_TEAM", "").strip()
    if env_team:
        return env_team

    # Try .atm.toml in cwd (minimal parsing without dependency).
    atm_toml = Path.cwd() / ".atm.toml"
    if atm_toml.is_file():
        try:
            for raw in atm_toml.read_text(encoding="utf-8").splitlines():
                line = raw.strip()
                if line.startswith("default_team") and "=" in line:
                    _, rhs = line.split("=", 1)
                    return rhs.strip().strip('"').strip("'")
        except Exception:
            pass

    return "atm-dev"


def render_text(report: dict[str, Any]) -> str:
    lines: list[str] = []
    lines.append(f"Team: {report['team']}")
    counts = report["task_summary"]["counts"]
    lines.append(
        "Task summary: pending={pending} in_progress={in_progress} completed={completed} other={other}".format(
            **counts
        )
    )
    if report["task_summary"]["open"]:
        lines.append("Open tasks:")
        for t in report["task_summary"]["open"]:
            lines.append(f"  - #{t['id']} [{t['status']}] {t['subject']}")

    for item in report["roots"]:
        lines.append("")
        lines.append(f"Root PID: {item['root_pid']}")
        chain_ids = " -> ".join(str(row["pid"]) for row in item["chain"])
        lines.append(f"Chain: {chain_ids}")
        for idx, row in enumerate(item["chain"]):
            lines.append(
                f"  [{idx}] pid={row['pid']} ppid={row['ppid']} user={row['user']} state={row['state']} etime={row['elapsed']} cpu={row['cputime']}"
            )
            lines.append(f"       cmd: {row['command']}")
            if row["daemon_matches"]:
                for m in row["daemon_matches"]:
                    lines.append(
                        "       daemon: agent={agent_name} session={session_id} state={state} runtime={runtime} updated={updated_at}".format(
                            **m
                        )
                    )
            if row["session_file_matches"]:
                for m in row["session_file_matches"]:
                    lines.append(
                        "       session-file: identity={identity} session={session_id} pid={pid} file={file}".format(
                            identity=m.get("identity", ""),
                            session_id=m.get("session_id", ""),
                            pid=row["pid"],
                            file=m.get("file", ""),
                        )
                    )
    return "\n".join(lines)


def main() -> int:
    parser = argparse.ArgumentParser(description="Report PID parent chains with ATM context.")
    parser.add_argument("pids", nargs="*", type=int, help="Root PID(s) to inspect")
    parser.add_argument("--team", default=None, help="ATM team name (default: env/.atm.toml/atm-dev)")
    parser.add_argument("--json", action="store_true", help="Emit JSON report")
    args = parser.parse_args()

    team = args.team or guess_default_team()
    root_pids = args.pids if args.pids else [os.getppid()]

    registry = load_daemon_registry()
    team_sessions = load_team_sessions(team)
    task_summary = load_task_summary(team)

    roots: list[dict[str, Any]] = []
    for root_pid in root_pids:
        chain_infos = build_chain(root_pid)
        rows: list[dict[str, Any]] = []
        for info in chain_infos:
            rows.append(
                {
                    "pid": info.pid,
                    "ppid": info.ppid,
                    "user": info.user,
                    "state": info.state,
                    "elapsed": info.elapsed,
                    "cputime": info.cputime,
                    "command": info.command,
                    "daemon_matches": registry_matches_for_pid(registry, team, info.pid),
                    "session_file_matches": team_session_matches_for_pid(team_sessions, info.pid),
                }
            )

        roots.append({"root_pid": root_pid, "chain": rows})

    report = {
        "team": team,
        "task_summary": task_summary,
        "roots": roots,
    }

    if args.json:
        print(json.dumps(report, indent=2))
    else:
        print(render_text(report))

    return 0


if __name__ == "__main__":
    sys.exit(main())
