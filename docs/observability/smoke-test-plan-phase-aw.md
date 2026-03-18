# Phase AW Smoke Test Plan

**Branch**: `develop` (after Phase AW merge)
**Purpose**: Post-merge verification of AW logs, traces, metrics, and fail-open behavior before live dogfooding.

## Review Notes

This plan intentionally expands beyond the AW.5 automated Loki smoke:

- `scripts/grafana-verify-smoke.py` remains the lowest-friction log-path check.
- This manual plan adds the missing daemon trace/metric verification and the Grafana read-path checks that AW.5 did not automate.
- Grafana Cloud read endpoints must be operator-supplied. Do not hardcode guessed Tempo or Mimir hostnames in the live run.

## Preconditions

- `develop` contains the merged AW work.
- AW-capable binaries are built from `develop`.
- Grafana Cloud credentials are available from `.private/grafana-otel-config.md`.
- A local team/runtime is available for a daemon-backed `send`/`read` flow.

Build from the current repo:

```bash
cd /Users/randlee/Documents/github/agent-team-mail
cargo build --release -p agent-team-mail -p agent-team-mail-daemon
export AW_ATM=./target/release/atm
export AW_DAEMON=./target/release/atm-daemon
```

Export the write path:

```bash
export ATM_OTEL_ENABLED=true
export ATM_OTEL_ENDPOINT=https://otlp-gateway-prod-us-west-0.grafana.net/otlp
export ATM_OTEL_PROTOCOL=otlp_http
export ATM_OTEL_AUTH_HEADER="Authorization: Basic <grafana-write-header>"
```

Export the read/query path separately:

```bash
export ATM_LOKI_ENDPOINT="https://logs-prod-us-west-0.grafana.net/loki/v1/query_range"
export ATM_LOKI_AUTH_HEADER="Authorization: Basic <grafana-read-header>"

# Confirm these from Grafana Cloud -> Connections -> Data sources before use.
export ATM_TEMPO_SEARCH_ENDPOINT="<tempo-search-endpoint>"
export ATM_TEMPO_AUTH_HEADER="Authorization: Basic <grafana-read-header>"
export ATM_MIMIR_QUERY_ENDPOINT="<mimir-query-endpoint>"
export ATM_MIMIR_AUTH_HEADER="Authorization: Basic <grafana-read-header>"
export ATM_MIMIR_SCOPE_ORGID="1551041"
```

## Area A — No Rogue Daemon Spawns

**Goal**: confirm normal CLI and daemon-backed flows do not leak shared or isolated daemons.

### A.1 — Baseline process inventory

```bash
pgrep -af "atm-daemon" || echo "(none)"
BEFORE=$(pgrep -c -f "atm-daemon" 2>/dev/null || echo 0)
echo "daemon processes before: $BEFORE"
```

### A.2 — Non-daemon-heavy command sequence

```bash
$AW_ATM config --json >/dev/null
$AW_ATM inbox >/dev/null || true
$AW_ATM read >/dev/null || true
$AW_ATM members >/dev/null
```

### A.3 — Post-sequence daemon count

```bash
sleep 2
AFTER=$(pgrep -c -f "atm-daemon" 2>/dev/null || echo 0)
echo "daemon processes after: $AFTER"
pgrep -af "atm-daemon" || echo "(none)"
```

**PASS criteria**: `AFTER == BEFORE`.

### A.4 — Explicit start/stop cleanup

```bash
ATM_HOME="$(mktemp -d)/atm-home" $AW_ATM inbox >/dev/null || true
sleep 3
pgrep -af "atm-daemon" || echo "(none)"
$AW_ATM daemon stop >/dev/null 2>&1 || true
sleep 2
pgrep -af "atm-daemon" || echo "(daemon stopped cleanly)"
```

**PASS criteria**: no additional daemon remains after stop.

## Area B — GH Rate Limiting Gate

**Goal**: confirm GH command paths and monitor paths respect the budget gate and do not amplify token use under parallel load.

### B.1 — Baseline core quota

```bash
R_before=$(gh api rate_limit | python3 -c "import json,sys; print(json.load(sys.stdin)['resources']['core']['remaining'])")
echo "remaining before: $R_before"
```

### B.2 — Single GH CLI path

```bash
$AW_ATM gh pr list >/dev/null
R_after_1=$(gh api rate_limit | python3 -c "import json,sys; print(json.load(sys.stdin)['resources']['core']['remaining'])")
echo "consumed by single call: $((R_before - R_after_1))"
```

### B.3 — Parallel GH CLI path

```bash
R_before_b3=$(gh api rate_limit | python3 -c "import json,sys; print(json.load(sys.stdin)['resources']['core']['remaining'])")
for i in $(seq 1 5); do $AW_ATM gh pr list >/dev/null 2>&1 & done
wait
R_after_b3=$(gh api rate_limit | python3 -c "import json,sys; print(json.load(sys.stdin)['resources']['core']['remaining'])")
echo "consumed by five parallel calls: $((R_before_b3 - R_after_b3))"
```

### B.4 — Monitor gate state written

```bash
ATM_HOME="$(mktemp -d)/atm-b4-home" $AW_ATM gh pr list >/dev/null 2>&1 || true
find "$ATM_HOME" -name '*.json' -path '*/gh-state/*' -print -exec cat {} \;
```

**PASS criteria**:

- single-call consumption is bounded
- five parallel calls do not produce unbounded amplification
- GH state records include the budget/rate fields used by the gate

## Area C — OTel Log Field Correctness in Grafana

**Goal**: verify the AX.4 log-field fixes through the real Loki read path.

### C.1 — Emit a tagged ATM log event

```bash
SESSION_TAG="aw-smoke-log-$(date +%s)"
CLAUDE_SESSION_ID="$SESSION_TAG" \
ATM_TEAM=atm-dev \
ATM_IDENTITY=arch-ctm \
ATM_RUNTIME=codex \
$AW_ATM config --json >/dev/null
sleep 10
```

### C.2 — Query Loki with the read endpoint

```bash
curl -s -G "$ATM_LOKI_ENDPOINT" \
  -H "$ATM_LOKI_AUTH_HEADER" \
  --data-urlencode "query={service_name=\"atm\",team=\"atm-dev\",agent=\"arch-ctm\",runtime=\"codex\",session_id=\"$SESSION_TAG\"} | json" \
  --data-urlencode "limit=20" \
  --data-urlencode "start=$(python3 -c 'import time; print(int((time.time()-180)*1e9))')" \
  | python3 -c "
import json,sys
d=json.load(sys.stdin)
streams=d.get('data',{}).get('result',[])
print(f'streams found: {len(streams)}')
for s in streams[:3]:
    print('labels:', s.get('stream',{}))
    for ts,line in s.get('values',[])[:2]:
        print(line[:240])
"
```

**PASS criteria**:

- at least one ATM log stream is returned
- `service_name=atm`
- the event exposes `team`, `agent`, `runtime`, and `session_id`
- the event exposes a concrete level mapping rather than `unknown`

## Area D — Traces and Metrics in Grafana

**Goal**: verify the actual AW-emitted trace and metric paths, including daemon-owned signals that the prior draft did not cover.

### D.1 — Emit CLI and daemon-backed signals

```bash
SESSION_TAG="aw-smoke-signals-$(date +%s)"
export CLAUDE_SESSION_ID="$SESSION_TAG"
export ATM_TEAM=atm-dev
export ATM_IDENTITY=arch-ctm
export ATM_RUNTIME=codex

$AW_ATM status --json >/dev/null
$AW_ATM send arch-ctm "aw smoke $SESSION_TAG" >/dev/null 2>&1 || true
$AW_ATM read >/dev/null 2>&1 || true

sleep 20
```

This sequence is intended to cover:

- CLI trace root spans such as `atm.command.status`, `atm.command.send`, `atm.command.read`
- CLI metrics such as `atm.commands_total`, `atm.command_duration_ms`, and message counters
- daemon trace spans such as `atm-daemon.dispatch_message`
- daemon metrics such as `atm_daemon.request_count` and `atm_daemon.request_duration_ms`

### D.2 — Query Tempo for CLI traces

```bash
curl -s -G "$ATM_TEMPO_SEARCH_ENDPOINT" \
  -H "$ATM_TEMPO_AUTH_HEADER" \
  --data-urlencode 'q={ resource.service.name = "atm" && session_id = "'"$SESSION_TAG"'" && name =~ "atm.command.(status|send|read)" }' \
  --data-urlencode "limit=20" \
  | python3 -c "
import json,sys
d=json.load(sys.stdin)
print(json.dumps(d, indent=2)[:2000])
"
```

**PASS criteria**:

- at least one trace is returned for `resource.service.name="atm"`
- returned span names include one or more of `atm.command.status`, `atm.command.send`, `atm.command.read`
- no returned ATM trace uses `unknown_service`

### D.3 — Query Tempo for daemon traces

```bash
curl -s -G "$ATM_TEMPO_SEARCH_ENDPOINT" \
  -H "$ATM_TEMPO_AUTH_HEADER" \
  --data-urlencode 'q={ resource.service.name = "atm-daemon" && session_id = "'"$SESSION_TAG"'" && name =~ "atm-daemon.(dispatch_message|plugin..*)" }' \
  --data-urlencode "limit=20" \
  | python3 -c "
import json,sys
d=json.load(sys.stdin)
print(json.dumps(d, indent=2)[:2000])
"
```

**PASS criteria**:

- at least one trace is returned for `resource.service.name="atm-daemon"`
- a daemon-owned span such as `atm-daemon.dispatch_message` is present

### D.4 — Query Mimir for metrics

Use regexes so the smoke stays resilient if the backend normalizes dotted names to underscored equivalents.

```bash
curl -s -G "$ATM_MIMIR_QUERY_ENDPOINT" \
  -H "$ATM_MIMIR_AUTH_HEADER" \
  -H "X-Scope-OrgID: $ATM_MIMIR_SCOPE_ORGID" \
  --data-urlencode 'query={__name__=~"atm[._](commands_total|command_duration_ms|messages_sent_total|messages_read_total)|atm_daemon[._](request_count|request_duration_ms|spool_size|dropped_events_total|export_failures_total)",session_id="'"$SESSION_TAG"'"}' \
  | python3 -c "
import json,sys
d=json.load(sys.stdin)
print(json.dumps(d, indent=2)[:2400])
"
```

**PASS criteria**:

- at least one CLI metric series is present for the session
- at least one daemon metric series is present for the session or runtime
- metric labels expose the expected correlation dimensions where applicable

### D.5 — Fail-open on unreachable collector

```bash
DEAD_ENDPOINT="http://127.0.0.1:1"
ATM_OTEL_ENDPOINT="$DEAD_ENDPOINT" $AW_ATM status --json >/tmp/aw-failopen-status.json
ATM_OTEL_ENDPOINT="$DEAD_ENDPOINT" $AW_ATM read >/dev/null 2>&1 || true
echo "status exit=$?"
python3 -c "import json; print('ok' if json.load(open('/tmp/aw-failopen-status.json')) else 'bad')"
```

**PASS criteria**:

- commands still succeed
- local JSON output remains valid
- local canonical logging remains present

### D.6 — Fail-open on collector rejection

Run the same CLI and daemon-backed flow with a bad auth header or a known staging-safe 401/403 endpoint:

```bash
ATM_OTEL_AUTH_HEADER="Authorization: Basic deliberately-bad" \
$AW_ATM status --json >/tmp/aw-failopen-auth.json
```

Then inspect logging health:

```bash
$AW_ATM logging-health --json | python3 -c "
import json,sys
d=json.load(sys.stdin)
print(json.dumps(d.get('otel_health',{}), indent=2))
"
```

**PASS criteria**:

- command flow still succeeds
- `otel_health.last_error` or equivalent diagnostics capture the collector failure
- local JSONL logging and `.otel.jsonl` mirroring continue

## Pass/Fail Summary

| Area | Focus | PASS criteria |
|---|---|---|
| A | Rogue daemon regression | no net-new daemon after smoke flows |
| B | GH gate verification | bounded consumption; budget/rate fields written |
| C | Log field correctness | `service_name`, severity mapping, and correlation labels visible in Loki |
| D.2 | CLI traces | Tempo returns `atm.command.*` traces for the smoke session |
| D.3 | Daemon traces | Tempo returns daemon-owned traces such as `atm-daemon.dispatch_message` |
| D.4 | Metrics | Mimir returns both CLI and daemon metric series for the smoke session/runtime |
| D.5 | Connect-failure fail-open | commands still succeed with dead collector endpoint |
| D.6 | HTTP/auth fail-open | commands still succeed and diagnostics capture rejection |

## Known Constraints

- The Loki read endpoint is documented in `.private/grafana-otel-config.md`; Tempo and Mimir read endpoints must be confirmed from Grafana Cloud before running D.2-D.4.
- Use read credentials for Loki/Tempo/Mimir queries. Do not reuse the OTLP write header for read APIs.
- `sc-compose` remains part of the logs rollout, but the AW trace/metric smoke for this phase should focus on the signals actually emitted today by `atm` and `atm-daemon`.
