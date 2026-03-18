# Grafana OTel Rollout Smoke

**Status**: Planned
**Phase**: AV rollout verification
**Purpose**: Verify that the AV-era OTLP HTTP logs export can be connected to a
Grafana-compatible collector endpoint without breaking canonical local logging
or fail-open behavior.

## Preconditions

- Phase AV code is merged into the integration branch under test.
- The target environment has a Grafana-compatible OTLP HTTP logs receiver
  available, either:
  - Grafana Alloy / OTel collector forwarding to Grafana
  - Grafana Cloud OTLP logs endpoint
  - another OTLP HTTP logs receiver used as the staging ingress
- The test host can still inspect local ATM log files.
- The test operator has a way to query the remote logs, ideally Grafana Explore.

## Required Config

Set the test environment explicitly:

- `ATM_OTEL_ENABLED=true`
- `ATM_OTEL_ENDPOINT=<collector-base-or-v1-logs-endpoint>`
- `ATM_OTEL_PROTOCOL=otlp_http`
- `ATM_OTEL_AUTH_HEADER=<header:value>` if the receiver requires auth
- `ATM_OTEL_CA_FILE=<path>` if a custom CA bundle is required
- `ATM_OTEL_INSECURE_SKIP_VERIFY=true` only in controlled staging/debug setups

Keep local logging enabled. The smoke is invalid if local JSONL logging is
disabled.

## Preflight

1. Confirm the installed binaries are the AV-capable build.
2. Clear or isolate the test log root so new events are easy to identify.
3. Record the canonical local sink path and `.otel.jsonl` mirror path.
4. Confirm the remote endpoint is reachable before the first command.
5. Open Grafana Explore or equivalent query UI scoped to the test tenant.

## Smoke Cases

### 1. Collector Connectivity

Run one low-risk command from an installed binary, for example:

- `atm config --json`

Verify:

- the command succeeds
- a new local JSONL event is written
- a new `.otel.jsonl` mirror event is written
- the corresponding event appears in Grafana/collector logs

### 2. Daemon Producer Path

Run one daemon-backed ATM flow, for example:

- `atm status --json`
- or `atm send ...` / `atm read ...` in a controlled local team setup

Verify:

- the command succeeds
- daemon-backed producer events appear remotely
- local canonical logs remain present

### 3. Correlation Field Queryability

Using the remote log UI, confirm you can filter or inspect by:

- `team`
- `agent`
- `runtime`
- `session_id`

If present for the exercised path, also confirm:

- `trace_id`
- `span_id`

### 4. Tool Coverage

Exercise at least one non-ATM producer already covered by AV, such as:

- `sc-compose render ...`

Verify:

- the remote event arrives
- the event is namespaced correctly
- local logging remains intact for that producer

### 5. Collector Outage Fail-Open

Break the remote collector path intentionally by:

- pointing `ATM_OTEL_ENDPOINT` at a closed local port
- or blocking the test receiver temporarily

Then rerun:

- one ATM command
- one non-ATM producer command

Verify:

- both commands still succeed
- local JSONL logging still occurs
- `.otel.jsonl` mirror still occurs when configured
- the failure is observable via local diagnostics/logging but does not block use

### 6. Auth / TLS Failure Fail-Open

Using a staging-safe endpoint, intentionally misconfigure either:

- `ATM_OTEL_AUTH_HEADER`
- `ATM_OTEL_CA_FILE`

Verify:

- command flow still succeeds
- local logging still succeeds
- exporter failure is visible in diagnostics/logs

### 7. Dogfood Dev Install

After `scripts/dev-install`, run:

- `scripts/otel-dev-install-smoke.py`

This confirms the installed dev binaries can:

- export to a live OTLP HTTP receiver
- preserve local canonical logging
- remain fail-open under endpoint outage

## Grafana Acceptance

The rollout is considered Grafana-ready for AV if:

- logs are queryable in Grafana Explore
- ATM and at least one secondary producer both arrive
- correlation fields are visible and usable for filtering
- local logging remains intact
- outage/auth/TLS failures do not break command execution

## Explicit Non-Goals

This smoke does **not** certify:

- native Tempo trace views
- native Prometheus/Mimir metrics dashboards
- full traces/metrics parity across all producers

Those belong to Phase AW.
