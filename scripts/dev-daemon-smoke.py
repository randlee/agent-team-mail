#!/usr/bin/env python3
import json
import os
import pathlib
import shutil
import signal
import subprocess
import sys
import tempfile
import time


ROOT = pathlib.Path(__file__).resolve().parents[1]
DEV_HOME = pathlib.Path.home() / ".local" / "share" / "atm-dev" / "home"
DEV_BIN = pathlib.Path.home() / ".local" / "atm-dev" / "bin"
DEV_DAEMON = str(DEV_BIN / "atm-daemon")
TEAM = "atm-dev"
REPORT = pathlib.Path("/tmp/ar2_smoke_report.md")

base_env = os.environ.copy()
base_env["PATH"] = str(DEV_BIN) + os.pathsep + base_env.get("PATH", "")
base_env["ATM_HOME"] = str(DEV_HOME)


def run(cmd: str, *, env=None, timeout=30, cwd=None):
    env = env or base_env
    cwd = cwd or ROOT
    start = time.time()
    try:
        proc = subprocess.run(
            cmd,
            shell=True,
            cwd=str(cwd),
            env=env,
            text=True,
            capture_output=True,
            timeout=timeout,
        )
        return {
            "cmd": cmd,
            "code": proc.returncode,
            "stdout": proc.stdout,
            "stderr": proc.stderr,
            "secs": round(time.time() - start, 2),
            "timeout": False,
        }
    except subprocess.TimeoutExpired as exc:
        return {
            "cmd": cmd,
            "code": None,
            "stdout": exc.stdout or "",
            "stderr": exc.stderr or "",
            "secs": round(time.time() - start, 2),
            "timeout": True,
        }


def parse_json(text: str):
    text = (text or "").strip()
    if not text:
        return None
    attempts = []
    lines = [ln for ln in text.splitlines() if ln.strip()]
    if lines:
        attempts.append(lines[-1])
    attempts.append(text)
    for item in attempts:
        try:
            return json.loads(item)
        except Exception:
            pass
    for marker in "{[":
        idx = text.find(marker)
        if idx != -1:
            try:
                return json.loads(text[idx:])
            except Exception:
                pass
    return None


def ps_lines():
    proc = subprocess.run(
        "ps -axo pid=,command=",
        shell=True,
        text=True,
        capture_output=True,
    )
    lines = []
    for line in proc.stdout.splitlines():
        text = line.strip()
        if not text:
            continue
        if DEV_DAEMON in text or "/target/debug/atm-daemon" in text or "/target/release/atm-daemon" in text:
            lines.append(text)
    return lines


def dev_lines():
    return [line for line in ps_lines() if DEV_DAEMON in line]


def kill_dev_daemons():
    for pattern in [DEV_DAEMON, "/target/debug/atm-daemon", "/target/release/atm-daemon"]:
        subprocess.run(
            f"pkill -9 -f '{pattern}'",
            shell=True,
            stdout=subprocess.DEVNULL,
            stderr=subprocess.DEVNULL,
        )
    time.sleep(1)


def gh_rate():
    result = run("gh api rate_limit", env=os.environ.copy(), timeout=20)
    data = parse_json(result["stdout"]) or {}
    core = ((data.get("resources") or {}).get("core") or {}) if isinstance(data, dict) else {}
    return {
        "remaining": core.get("remaining"),
        "limit": core.get("limit"),
        "reset": core.get("reset"),
    }


def latest_pr():
    result = run(
        "gh pr list --state open --limit 10 --json number,state",
        env=os.environ.copy(),
        timeout=20,
    )
    data = parse_json(result["stdout"]) or []
    if data:
        return data[0].get("number")
    return 792


def latest_run():
    result = run(
        "gh run list --limit 10 --json databaseId,status,headBranch",
        env=os.environ.copy(),
        timeout=20,
    )
    data = parse_json(result["stdout"]) or []
    for item in data:
        if item.get("databaseId"):
            return item["databaseId"]
    return None


def read_status(home: pathlib.Path):
    for rel in ("daemon/status.json", ".atm/daemon/status.json"):
        path = home / rel
        if path.exists():
            try:
                return path, json.loads(path.read_text())
            except Exception:
                return path, None
    return None, None


def status_pid(home: pathlib.Path):
    _, data = read_status(home)
    if not isinstance(data, dict):
        return None
    if "pid" in data:
        return data["pid"]
    owner = data.get("owner") or {}
    if isinstance(owner, dict):
        return owner.get("pid") or owner.get("daemon_pid")
    return None


def fields(data):
    if not isinstance(data, dict):
        return {}
    owner = data.get("owner") or {}
    health = data.get("health_record") or data.get("health") or {}
    return {
        "enabled": data.get("enabled"),
        "availability_state": data.get("availability_state"),
        "lifecycle_state": data.get("lifecycle_state"),
        "message": data.get("message"),
        "in_flight": health.get("in_flight", data.get("in_flight")),
        "owner_runtime_kind": owner.get("runtime_kind") or data.get("owner_runtime_kind"),
        "owner_binary_path": owner.get("binary_path") or data.get("owner_binary_path"),
        "owner_home": owner.get("home") or data.get("owner_atm_home"),
        "updated_at": health.get("updated_at") or data.get("updated_at") or data.get("last_updated_at"),
        "freshness": health.get("freshness") or data.get("freshness"),
        "repo_state_keys": sorted((data.get("repo_state") or {}).keys()) if isinstance(data.get("repo_state"), dict) else None,
    }


def add_case(results, name, purpose, passed, details):
    results.append({"name": name, "purpose": purpose, "pass": bool(passed), "details": details})
    print(f"[{name}] {'PASS' if passed else 'FAIL'}")
    sys.stdout.flush()


def daemon_probe(team: str) -> str:
    return f"atm gh --team {team} status --json"


def main():
    kill_dev_daemons()
    pre_pgrep = ps_lines()
    rate_before = gh_rate()
    pr_num = latest_pr()
    run_id = latest_run()
    results = []

    r1 = run(f"atm gh --team {TEAM} status --json", timeout=20)
    j1 = parse_json(r1["stdout"])
    r2 = run(f"atm gh --team {TEAM} monitor pr {pr_num} --json", timeout=25)
    j2 = parse_json(r2["stdout"])
    r3 = run(f"atm gh --team {TEAM} status --json", timeout=20)
    j3 = parse_json(r3["stdout"])
    add_case(results, "AN.1", "GH monitor start / status round trip", r2["code"] == 0 and fields(j3).get("enabled") is True and fields(j3).get("availability_state") == "healthy", [f"initial={fields(j1)}", f"monitor code={r2['code']} timeout={r2['timeout']} secs={r2['secs']} out={(r2['stdout'] or '').strip()[:220]}", f"final={fields(j3)}"])

    r1 = run(f"atm gh --team {TEAM} monitor stop --json", timeout=20)
    j1 = parse_json(r1["stdout"])
    r2 = run(f"atm gh --team {TEAM} status --json", timeout=20)
    j2 = parse_json(r2["stdout"])
    r3 = run(f"atm gh --team {TEAM} monitor restart --json", timeout=20)
    j3 = parse_json(r3["stdout"])
    r4 = run(f"atm gh --team {TEAM} status --json", timeout=20)
    j4 = parse_json(r4["stdout"])
    add_case(results, "AN.2", "GH monitor lifecycle control", (not r1["timeout"]) and (not r3["timeout"]) and fields(j2).get("lifecycle_state") in ("stopped", "idle") and fields(j4).get("lifecycle_state") in ("running", "active"), [f"stop code={r1['code']} timeout={r1['timeout']} secs={r1['secs']}", f"after-stop={fields(j2)}", f"restart code={r3['code']} timeout={r3['timeout']} secs={r3['secs']}", f"after-restart={fields(j4)}"])
    kill_dev_daemons()

    r1 = run(f"atm gh --team {TEAM} monitor run {run_id} --json", timeout=20)
    r2 = run(f"atm gh --team {TEAM} status --json", timeout=20)
    j2 = parse_json(r2["stdout"])
    r3 = run(f"atm gh --team {TEAM} --repo randlee/agent-team-mail monitor run {run_id} --json", timeout=20)
    r4 = run(f"atm gh --team {TEAM} status --json", timeout=20)
    j4 = parse_json(r4["stdout"])
    add_case(results, "AN.3", "Multi-repo repo-scope resolution", r1["code"] == 0 and r3["code"] == 0, [f"default code={r1['code']} timeout={r1['timeout']} out={(r1['stdout'] or '').strip()[:200]}", f"status-default={fields(j2)}", f"override code={r3['code']} timeout={r3['timeout']} out={(r3['stdout'] or '').strip()[:200]}", f"status-override={fields(j4)}"])
    kill_dev_daemons()

    before = len(dev_lines())
    r1 = run(daemon_probe(TEAM), timeout=20)
    after1 = dev_lines()
    r2 = run(daemon_probe(TEAM), timeout=20)
    after2 = dev_lines()
    add_case(results, "AO.1", "Shared runtime admission", before == 0 and r1["code"] == 0 and r2["code"] == 0 and len(after1) == 1 and len(after2) == 1, [f"before={before}", f"first code={r1['code']} timeout={r1['timeout']}", f"after1={after1}", f"second code={r2['code']} timeout={r2['timeout']}", f"after2={after2}"])
    kill_dev_daemons()

    iso = pathlib.Path(tempfile.mkdtemp(prefix="ar2-iso-"))
    iso_env = base_env.copy()
    iso_env["ATM_HOME"] = str(iso)
    r1 = run(daemon_probe(TEAM), env=iso_env, timeout=20)
    j1 = parse_json(r1["stdout"])
    iso_files = sorted(str(p.relative_to(iso)) for p in iso.rglob("*") if p.is_file())
    ipid = status_pid(iso)
    add_case(results, "AO.2", "Isolated runtime TTL / non-shared state", r1["code"] == 0 and bool(iso_files) and fields(j1).get("availability_state") == "disabled_config_error", [f"code={r1['code']} timeout={r1['timeout']}", f"status={fields(j1)}", f"iso_files={iso_files[:20]}", f"iso_pid={ipid}"])
    if ipid:
        try:
            os.kill(int(ipid), signal.SIGKILL)
        except Exception:
            pass
    kill_dev_daemons()
    shutil.rmtree(iso, ignore_errors=True)

    r1 = run(f"atm gh --team {TEAM} status --json", timeout=20)
    j1 = parse_json(r1["stdout"])
    add_case(results, "AO.3", "Repo-state budget observability", r1["code"] == 0 and bool(fields(j1).get("updated_at")), [f"fields={fields(j1)}"])

    r1 = run(f"atm gh --team {TEAM} monitor stop --json", timeout=20)
    r2 = run(f"atm gh --team {TEAM} status --json", timeout=20)
    j2 = parse_json(r2["stdout"])
    r3 = run(f"atm gh --team {TEAM} monitor restart --json", timeout=20)
    r4 = run(f"atm gh --team {TEAM} status --json", timeout=20)
    j4 = parse_json(r4["stdout"])
    add_case(results, "AO.4", "Operator shutdown / restart", (not r1["timeout"]) and (not r3["timeout"]) and fields(j2).get("lifecycle_state") in ("stopped", "idle") and fields(j4).get("lifecycle_state") in ("running", "active"), [f"stop code={r1['code']} timeout={r1['timeout']}", f"after-stop={fields(j2)}", f"restart code={r3['code']} timeout={r3['timeout']}", f"after-restart={fields(j4)}"])
    kill_dev_daemons()

    before = ps_lines()
    r1 = run(daemon_probe(TEAM), timeout=20)
    after = ps_lines()
    add_case(results, "AP.1", "Clean start / no rogue daemons", len(before) == 0 and r1["code"] == 0 and len(dev_lines()) == 1, [f"before={before}", f"code={r1['code']} timeout={r1['timeout']}", f"after={after}"])

    pid1 = status_pid(DEV_HOME) or (dev_lines()[0].split()[0] if dev_lines() else None)
    if pid1:
        try:
            os.kill(int(pid1), signal.SIGKILL)
        except Exception:
            pass
        time.sleep(1)
    r1 = run(daemon_probe(TEAM), timeout=20)
    j1 = parse_json(r1["stdout"])
    pid2 = status_pid(DEV_HOME) or (dev_lines()[0].split()[0] if dev_lines() else None)
    add_case(results, "AP.2", "Stale lock / PID recovery", r1["code"] == 0 and fields(j1).get("configured") is True and pid1 and pid2 and str(pid1) != str(pid2) and len(dev_lines()) == 1, [f"pid_before={pid1}", f"code={r1['code']} timeout={r1['timeout']}", f"status={fields(j1)}", f"pid_after={pid2}"])

    pid1 = status_pid(DEV_HOME) or (dev_lines()[0].split()[0] if dev_lines() else None)
    if pid1:
        try:
            os.kill(int(pid1), signal.SIGKILL)
        except Exception:
            pass
        time.sleep(1)
    after_stop = dev_lines()
    r1 = run(daemon_probe(TEAM), timeout=20)
    pid2 = status_pid(DEV_HOME) or (dev_lines()[0].split()[0] if dev_lines() else None)
    add_case(results, "AP.3", "PID tracking / teardown discipline", r1["code"] == 0 and pid1 and pid2 and str(pid1) != str(pid2) and len(after_stop) == 0 and len(dev_lines()) == 1, [f"pid_before={pid1}", f"after_stop={after_stop}", f"restart code={r1['code']} timeout={r1['timeout']}", f"pid_after={pid2}"])

    kill_dev_daemons()
    r1 = run(f"atm gh --team {TEAM} status --json", timeout=20)
    j1 = parse_json(r1["stdout"])
    dlines = dev_lines()
    add_case(results, "AQ.1", "Daemon autostart reliability", r1["code"] == 0 and len(dlines) == 1 and fields(j1).get("owner_binary_path") == DEV_DAEMON, [f"status={fields(j1)}", f"dev_lines={dlines}"])

    r1 = run(f"atm gh --team {TEAM} monitor pr {pr_num} --json", timeout=15)
    r2 = run(f"atm gh --team {TEAM} monitor stop --json", timeout=20)
    r3 = run(f"atm gh --team {TEAM} monitor restart --json", timeout=20)
    add_case(results, "AQ.2", "Const-driven timeout behavior", r1["code"] == 0 and not r2["timeout"] and not r3["timeout"] and r1["secs"] < 10 and r2["secs"] < 15 and r3["secs"] < 15, [f"monitor secs={r1['secs']} code={r1['code']} timeout={r1['timeout']}", f"stop secs={r2['secs']} code={r2['code']} timeout={r2['timeout']}", f"restart secs={r3['secs']} code={r3['code']} timeout={r3['timeout']}"])
    kill_dev_daemons()

    before = ps_lines()
    r1 = run(daemon_probe(TEAM), timeout=20)
    during = ps_lines()
    kill_dev_daemons()
    after = ps_lines()
    add_case(results, "AQ.3", "Rogue daemon spawn elimination", r1["code"] == 0 and len(before) == 0 and len([line for line in during if DEV_DAEMON in line]) == 1 and len(after) == 0, [f"before={before}", f"workflow code={r1['code']} timeout={r1['timeout']}", f"during={during}", f"after={after}"])

    kill_dev_daemons()
    rb = gh_rate()
    r1 = run(f"atm gh --team {TEAM} monitor pr {pr_num} --json", timeout=20)
    time.sleep(130)
    rm = gh_rate()
    run(f"atm gh --team {TEAM} monitor stop --json", timeout=20)
    time.sleep(5)
    rs = gh_rate()
    time.sleep(70)
    rs2 = gh_rate()
    consumption = (rb["remaining"] - rm["remaining"]) if rb["remaining"] is not None and rm["remaining"] is not None else None
    post = (rm["remaining"] - rs2["remaining"]) if rm["remaining"] is not None and rs2["remaining"] is not None and rm["reset"] == rs2["reset"] else None
    add_case(results, "CI.Token", "Dedicated GH token-consumption verification", consumption is not None and 2 <= consumption <= 8 and post is not None and post <= 2, [f"before={rb}", f"monitor code={r1['code']} timeout={r1['timeout']}", f"after_active={rm}", f"after_stop_short={rs}", f"after_stop_long={rs2}", f"consumption_active={consumption}", f"post_stop_delta={post}"])

    final_pgrep = ps_lines()
    rate_final = gh_rate()
    overall = all(item["pass"] for item in results)
    lines = []
    lines.append("# AR.2 Smoke Run Results")
    lines.append("")
    lines.append(f"- Repo: `{ROOT}`")
    head = subprocess.check_output(["git", "rev-parse", "--short", "HEAD"], cwd=str(ROOT), text=True).strip()
    lines.append(f"- Head: `{head}`")
    lines.append(f"- Shared dev ATM_HOME: `{DEV_HOME}`")
    lines.append(f"- PR used: `{pr_num}`")
    lines.append(f"- Run ID used: `{run_id}`")
    lines.append("")
    lines.append("## Preflight")
    lines.append(f"- `pgrep` before cleanup: `{pre_pgrep}`")
    lines.append(f"- GH rate before: `{rate_before}`")
    lines.append("")
    for result in results:
        lines.append(f"## {result['name']} — {'PASS' if result['pass'] else 'FAIL'}")
        lines.append(f"Purpose: {result['purpose']}")
        for detail in result["details"]:
            lines.append(f"- {detail}")
        lines.append("")
    lines.append("## Final State")
    lines.append(f"- `pgrep` after cleanup: `{final_pgrep}`")
    lines.append(f"- GH rate final: `{rate_final}`")
    lines.append("")
    lines.append(f"## Overall Verdict: {'PASS' if overall else 'FAIL'}")
    if not overall:
        lines.append("Do not publish based on this smoke run.")

    REPORT.write_text("\n".join(lines) + "\n")
    print("\n".join(lines))
    return 0 if overall else 1


if __name__ == "__main__":
    raise SystemExit(main())
