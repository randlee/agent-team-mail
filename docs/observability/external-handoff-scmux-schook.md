# External Handoff: `scmux` / `schook` OTel Follow-On

Issue: `#624`

Phase AV delivers live OTLP collector export only for binaries in this
repository. `scmux` and `schook` remain explicit follow-on work in their own
repositories and must not be treated as implicitly covered by ATM rollout.

## Required Carry-Forward Contract

`scmux` and `schook` must adopt the same partition rules established here:

- `sc-observability` remains the only shared observability facade used by
  feature code.
- `sc-observability-otlp` remains the only collector-facing adapter layer.
- feature modules must not import OTLP SDK/client crates directly.
- local JSONL plus `.otel.jsonl` mirror remain fail-open and authoritative when
  collector export is degraded.

## Required Configuration Surface

External follow-on work must honor the same canonical environment controls:

- `ATM_OTEL_ENABLED`
- `ATM_OTEL_ENDPOINT`
- `ATM_OTEL_PROTOCOL`
- `ATM_OTEL_AUTH_HEADER`
- `ATM_OTEL_CA_FILE`
- `ATM_OTEL_INSECURE_SKIP_VERIFY`
- `ATM_OTEL_TIMEOUT_MS`
- `ATM_OTEL_RETRY_MAX_ATTEMPTS`
- `ATM_OTEL_RETRY_BACKOFF_MS`
- `ATM_OTEL_RETRY_MAX_BACKOFF_MS`
- `ATM_OTEL_DEBUG_LOCAL_EXPORT`

## Minimum Delivery Checklist

1. Each repo adds collector-backed smoke on its installed binary path.
2. Each repo adds explicit outage/fail-open regression coverage.
3. Each repo exposes canonical OTel health for:
   - collector endpoint
   - collector state
   - local mirror state/path
   - last export error
4. Each repo adds a boundary check that blocks direct OTLP construction outside
   the dedicated adapter layer.

## Recommended Smoke Shape

Use the same pattern added in Phase AV:

- run the installed binary with `ATM_LOG_FILE` and `ATM_OTEL_ENDPOINT` pointed
  at a local test collector
- verify collector payload receipt
- verify canonical local logging still writes
- rerun with the collector unavailable and confirm command success remains
  intact

## ATM Deliverables Already Available

- canonical collector adapter: `crates/sc-observability-otlp`
- local fail-open mirror and health contract: `crates/sc-observability`
- real dev-install smoke harness: `scripts/otel-dev-install-smoke.py`

Close `#624` only after both external repos deliver the checklist above.
