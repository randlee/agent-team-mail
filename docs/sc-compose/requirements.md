# sc-compose Requirements

## 1. Dual-Mode Observability Injection

`sc-compose` operates in two distinct modes with fundamentally different observability behavior. This distinction is **foundational**: all observability features (logging, OTel, spool, health reporting) MUST be designed around it from the start.

### 1.1 Library Mode (embedded in a caller)

When `sc-compose` or `sc-composer` is used as a library dependency by a host process:

- **FR-SCO-001 MUST**: The caller MUST inject the log file path. `sc-compose` MUST NOT resolve, default, or independently discover a log path.
- **FR-SCO-002 MUST**: The caller MUST inject OTel project settings (exporter endpoint, resource attributes, session context, team/agent identity). `sc-compose` MUST NOT initialize or configure OTel independently.
- **FR-SCO-003 MUST**: `sc-compose` MUST expose a `ProjectSettings` (or equivalent) injection interface accepted at initialization time. At minimum this interface must accept:
  - log sink path
  - spool directory path
  - OTel exporter endpoint (or disabled signal)
  - session identity attributes (`session_id`, `team`, `agent`)
- **FR-SCO-004 MUST NOT**: `sc-compose` MUST NOT fall back to `ATM_HOME`, `SC_COMPOSE_HOME`, or any other environment variable for log or OTel path resolution when operating in library mode. All paths and settings are caller-supplied. Environment variable fallback is permitted only in standalone CLI mode (see §1.2).
- **FR-SCO-005 MUST NOT**: `sc-compose` MUST NOT emit to any observability sink that the caller did not explicitly supply. Silent no-op behavior is acceptable when no sink is injected; silent default behavior is not.

### 1.2 Standalone CLI Mode

When `sc-compose` is invoked directly as a command-line tool:

- **FR-SCO-006 MUST**: `sc-compose` MUST use the default per-tool log path: `${home_dir}/.config/sc-compose/logs/sc-compose.log.jsonl`
- **FR-SCO-007 MAY**: An explicit operator override (environment variable or config file entry) MAY override the default log root. Sink and spool paths still derive from the root.
- **FR-SCO-008 MUST**: OTel defaults for standalone mode are: local `.otel.jsonl` sidecar enabled by default; OTLP export enabled if `SC_COMPOSE_OTEL_ENDPOINT` (or equivalent) is set.
- **FR-SCO-009 MUST**: Standalone CLI mode MUST initialize its own `ProjectSettings` from environment and config before delegating to the library core.

### 1.3 Mode Detection

- **FR-SCO-010 MUST**: The mode (library vs standalone) MUST be determined at `ProjectSettings` construction time, not at call sites. Library callers pass `ProjectSettings` explicitly; standalone CLI constructs `ProjectSettings` from env/config during startup.
- **FR-SCO-011 MUST NOT**: There MUST NOT be a shared mutable global that auto-initializes observability on first use. Global/thread-local state for observability is prohibited; all state flows through the injected `ProjectSettings`.

## 2. OTel Injection Contract

The same injection principle applies to all OTel features:

- **FR-SCO-012 MUST**: When embedded as a library, all OTel spans, metrics, and log records emitted by `sc-compose`/`sc-composer` MUST carry the `session_id`, `team`, and `agent` attributes injected by the caller. `sc-compose` MUST NOT synthesize these values independently.
- **FR-SCO-013 MUST**: The OTel exporter (endpoint, transport, batch config) MUST be caller-supplied in library mode. `sc-compose` MUST accept a pre-configured exporter or a no-op exporter; it MUST NOT open its own OTLP connection.
- **FR-SCO-014 MUST**: OTel partition boundary applies: `sc-observability` owns neutral event shaping and `OtelRecord` contracts; `sc-compose` consumes that interface. `sc-compose` MUST NOT import `sc-observability-otlp` directly — the caller wires the transport.

## 3. Rationale

This requirement exists because:

1. When ATM embeds `sc-compose` as a library, ATM's log file and OTel session must be used — not a separate `sc-compose`-scoped file that pollutes ATM's observability surface.
2. OTel correlation (`session_id`, `team`, `agent`) must flow end-to-end. If `sc-compose` initializes its own OTel context, trace continuity breaks.
3. Standalone callers (shell scripts, CI, third-party tools) need `sc-compose` to work out-of-the-box without configuring injection.

This requirement supersedes the earlier vague `"host-injected sink/path configuration"` language in `docs/observability/requirements.md §10`. Phase BD implementation MUST satisfy FR-SCO-001 through FR-SCO-014 before extraction to a standalone repository.

## 4. Cross-References

- `docs/observability/requirements.md §10` — Cross-tool integration (embedding contract)
- `docs/sc-composer/requirements.md §12` — Embedded usage / host-injected sink (aligns with FR-SCO-001/003)
- `docs/phase-bd-sc-compose-extraction.md` — Phase BD extraction plan (must be updated to reference these FRs)
- `scripts/ci/observability_boundary_check.sh` — CI gate enforcing OTel partition boundary (FR-SCO-014)
