# Phase AH Planning — Observability Unification + AG Deferred Closure

> Historical planning record. OTel optionality described here is superseded by
> Phase AK (`docs/phase-ak-planning.md`), where OTel rollout is mandatory.

## Goal

Close AG deferred issues by extracting a shared observability crate and aligning logging/diagnostics behavior across `atm` and `sc-compose` with one implementation surface.

## Delivery Target

- Target version: `v0.43.0`
- Integration branch: `integrate/phase-AH`

## Inputs

- Deferred AG issues:
  - #555 — diagnostics enrichment (`MISSING_VAR` details)
  - #556 — logging feature parity gaps (levels/human mode/redaction)
  - #557 — render output path derivation without explicit `--output`
  - #558 — `sc-compose` install/documentation completeness
- Existing standalone docs:
  - `docs/observability/requirements.md`
  - `docs/observability/architecture.md`
  - `docs/sc-composer/requirements.md`
  - `docs/sc-composer/architecture.md`

## Out of Scope (Deferred to AI)

- #560 — `atm gh pr list` PR dashboard
- #561 — `atm gh pr report <PR>` detailed reporting + template extension
- See `docs/phase-ai-planning.md` for sprint sizing and design constraints.

## Phase Scope

1. Create shared crate `sc-observability` (workspace crate)
- Own canonical event envelope and helpers used by both ATM and `sc-compose`.
- Provide level filtering (`error|warn|info|debug|trace`).
- Provide output mode controls (JSONL + human-friendly).
- Provide truncation + redaction policy hooks.
- Provide host-injected sink interface so embedded `sc-composer` can emit to host logger.

2. Integrate `sc-observability` into `sc-compose`
- Replace local logger implementation in `crates/sc-compose/src/observability.rs`.
- Preserve default standalone path behavior for `sc-compose` logs.
- Add CLI flags/env wiring for log level and output mode.

3. Integrate `sc-observability` into ATM
- Route ATM CLI and daemon-facing command logging through shared crate.
- Ensure `sc_composer::compose()` calls in ATM use the same schema and field naming.
- Keep daemon/doctor behavior unchanged except for schema consistency improvements.

4. Close deferred AG issues
- #555: enrich missing-variable diagnostics with path + include chain and line/column when available.
- #556: complete FR-9 logging requirements (level control, human mode, redaction).
- #557: implement deterministic `.j2 -> output path` derivation logic for file output flows.
- #558: update install docs (README + quickstart + release docs) for `sc-compose` end-user path.

## AH Design Contracts (Locked)

### AH-OBS-1: One logger crate for all tools

`sc-observability` is the single structured-logging implementation used by:
- `atm`
- `atm-daemon`
- `atm-tui`
- `atm-agent-mcp`
- `scmux`
- `schook`
- `sc-compose`
- `sc-composer`

No parallel per-tool logger implementations are permitted.

### AH-OBS-2: Logging enabled by default

- Logging must be on by default for every tool using `sc-observability`.
- New projects/tools must get first-class structured logging on day one via a minimal init path.
- Canonical zero-config API target: `sc_observability::init("<tool-name>")`.

### AH-OBS-3: Per-tool log namespaces

All tools must write to separate subdirectories under a common log root:
- `.../logs/atm/`
- `.../logs/scmux/`
- `.../logs/schook/`
- `.../logs/sc-compose/`
- `.../logs/sc-composer/`

Schema, redaction, truncation, and envelope rules remain consistent across tools.

### AH-OBS-4: Rotation + retention defaults

- Default retention policy: 7 days.
- Rotation is enabled by default.
- Retention window is configurable per tool/project.
- Logging must not silently disable itself when retention config is invalid.
- Default queue/rotation constants (from observability requirements):
  - queue capacity: `4096`
  - max event size guard: `64 KiB`
  - rotation threshold: `50 MiB`
  - retained files: `5`

### AH-OBS-5: OpenTelemetry (optional)

- OTel support is required as an optional capability in `sc-observability`.
- OTel must be feature-gated (default off) to avoid mandatory dependency overhead.
- `atm` and `scmux` are first adopters for OTel emission once AH integration lands.
- File logging remains available regardless of OTel enablement.

### AH-OBS-5a: OpenTelemetry baseline telemetry set

OTel rollout in AH is intentionally scoped to a short baseline set:

- Traces:
  - `subagent.run` (priority trace for orchestration visibility)
  - `atm.send`
  - `atm.read`
  - `daemon.request` (selected command paths)
- Metrics:
  - `subagent_runs_total` (by status)
  - `subagent_run_duration_ms` (histogram)
  - `subagent_active_count` (gauge)
  - `atm_messages_total` (send/read + result)
  - `log_events_total` (by tool + level)
  - `warnings_total` / `errors_total` (by tool + code/category)
- Logs:
  - Existing structured events exported with shared correlation fields
    (`trace_id`, `span_id`, request/session IDs when available).

### AH-OBS-5b: Sub-agent-first instrumentation policy

- Sub-agent traceability is first-priority for AH observability.
- Task-tool lifecycle is the initial source for sub-agent span creation and
  completion.
- Canonical sub-agent correlation key is `subagent_id`; it must appear in both
  trace attributes and structured logs so operators can pivot to raw JSONL logs.
- Long-term deep hook-native sub-agent instrumentation is expected to migrate to
  `schooks`; AH establishes the shared schema and baseline transport.

### AH-OBS-6: Reliability and safety

- Logging/export paths must be non-blocking for tool execution.
- Redaction is required before persistence/export (no plaintext secret leaks by default).
- Failure to export (including OTel backend unavailable) must degrade gracefully with local logging continuity.

## Proposed Sprint Map

| Sprint | Focus | Primary Issues | Deliverables |
|---|---|---|---|
| AH.1 | `sc-observability` crate skeleton + contracts | #556 | New workspace crate, stable event schema/types, sink trait; `docs/logging-l1a-spec.md` authoring/update; socket contract error codes (`VERSION_MISMATCH`, `INVALID_PAYLOAD`, `INTERNAL_ERROR`); size guard (`64 KiB`); queue/rotation defaults; spool semantics (filename format, ordering, delete-after-merge); unit tests |
| AH.2 | `sc-compose` migration to shared observability | #556 | Remove local logger duplication, add level/output controls, integration tests |
| AH.3 | Diagnostics + render behavior closure | #555, #557 | Missing-var diagnostic enrichment and output derivation behavior with tests |
| AH.4 | ATM ecosystem integration + health surfaces | #556 | Integrate `atm`, `atm-daemon`, `atm-tui`, and `atm-agent-mcp` with shared crate; deliver `atm doctor --json` / `atm status --json` logging-health fields (state, dropped counter, spool path, last error); wire runtime env controls (`ATM_LOG`, `ATM_LOG_MSG`, `ATM_LOG_FILE`) |
| AH.5 | Docs + runbook + release/install closure | #558 | README/quickstart/PUBLISHING updates; `docs/logging-troubleshooting.md` runbook alignment to health states/remediations; final QA checklist |

## Sprint Dependency Graph

- AH.2 depends on AH.1.
- AH.3 depends on AH.1.
- AH.4 depends on AH.1 and AH.2.
- AH.5 depends on AH.3 and AH.4.

## Acceptance Criteria

1. No duplicate logging implementations remain between ATM and `sc-compose`.
2. `sc-compose` and ATM logging emit compatible schema fields for shared event types.
3. `sc-compose` supports configurable level + output mode and keeps logging enabled by default.
4. Diagnostics for missing variables include actionable source details (path/include chain, position when available).
5. Output-path derivation behavior is deterministic and covered by tests.
6. End-user docs explicitly cover `sc-compose` install/use flows.
7. `atm doctor --json` and `atm status --json` expose logging health state and required diagnostics fields (`state`, canonical log path, spool path, dropped counter, oldest spool age/count, last error).
8. OTel/scmux/schook integration was deferred beyond AH and tracked in later planning phases.

## Test Plan (Phase AH)

- Unit tests (`sc-observability`):
  - level filtering
  - truncation behavior
  - redaction behavior
  - sink routing semantics
  - serialized size guard (`64 KiB`)
  - spool merge semantics:
    - filename convention `{source_binary}-{pid}-{unix_millis}.jsonl`
    - merge ordering (timestamp then file order)
    - delete source spool file only after successful merge
  - queue/rotation defaults (`4096`, `50 MiB`, retained files `5`)
- `sc-compose` integration tests:
  - JSON vs human log modes
  - level-gated emission
  - output derivation cases (`.md.j2`, `.xml.j2`, `.j2`)
  - diagnostics payload richness for missing vars
- ATM integration tests:
  - shared event schema parity checks for compose-related operations
  - no subprocess shell-outs for compose behavior
  - OTel baseline trace/metric emission smoke tests (toggle enabled/disabled)
  - `doctor --json` and `status --json` logging-health payload presence and schema consistency
  - runtime env control behavior: `ATM_LOG`, `ATM_LOG_MSG`, `ATM_LOG_FILE`
- SCMUX integration tests:
  - shared schema parity checks for status/message operations
  - sub-agent trace correlation (`subagent_id`) propagation checks
- CI gates:
  - `cargo fmt --check --all`
  - `cargo clippy --workspace -- -D warnings`
  - targeted integration tests for AH deliverables

## Risks / Notes

- Full line/column fidelity for missing variables may require parser-side instrumentation in addition to render-time strict undefined behavior.
- Logging schema migration must preserve backward compatibility for existing log readers where required.
- Keep release-artifact manifest as SSoT for publish/install artifacts while expanding docs for new CLI surface.
