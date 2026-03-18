# External Consumer Adoption Checklist

**Status**: Active for AW.6
**See also**:
- `docs/observability/external-consumer-contract.md`
- `docs/observability/architecture.md`
- `docs/observability/troubleshooting.md`
- `scripts/validate-external-consumer.sh`

1. Add `sc-observability` as the shared observability facade dependency in the
   consumer repo's relevant `Cargo.toml` files.

2. Confirm feature crates do not add `sc-observability-otlp` or raw
   `opentelemetry*` dependencies.

3. Restrict any collector/bootstrap setup to the approved entry point, usually
   `src/main.rs`.

4. Wire the canonical env/config surface only:
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

5. Verify local JSONL logging and `.otel.jsonl` mirroring remain available when
   OTel is enabled.

6. Add a collector-backed smoke test for the installed binary path, following
   the shape in `scripts/otel-dev-install-smoke.py`.

7. Add an outage/fail-open regression that proves command/process success is
   preserved when the collector is unavailable, misconfigured, or unauthorized.

8. Surface OTel health for operators:
   - collector endpoint
   - collector state
   - local mirror state/path
   - last export error

9. Run the validator before sign-off:

```bash
scripts/validate-external-consumer.sh --repo /path/to/consumer-repo
```

10. Run the validator dry-run first when bootstrapping or reviewing the repo:

```bash
scripts/validate-external-consumer.sh --repo /path/to/consumer-repo --dry-run
```

11. Only mark the rollout complete once both the contract validator and the
    collector-backed smoke pass cleanly.

If validation fails, consult `docs/observability/troubleshooting.md` before
declaring the rollout blocked so the operator path and expected fail-open
behavior are reviewed first.
