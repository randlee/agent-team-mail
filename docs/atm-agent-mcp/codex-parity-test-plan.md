# Codex CLI Parity Test Plan (M.3 + M.7)

Status: Approved baseline for M.3/M.7 implementation
Owner: `arch-ctm`
Scope: `atm-agent-mcp` + `atm-tui` Codex watch parity validation

## 1. Goal

Provide objective, automatable parity checks so Codex watch mode can be validated before user testing.

Out of scope:
- subjective preference tuning (font/theme/personal UX taste),
- feature expansion beyond Codex parity baseline.

## 2. Test Layers

### Layer A: Contract/Schema Tests

Purpose:
- Ensure transport payloads remain compatible and detect drift early.

Coverage:
- `mcp`, `cli-json`, `app-server` inbound event shapes.
- required fields, enum values, and error envelope shape.

Mechanics:
- Validate sample payload fixtures against pinned schema definitions.
- Treat unknown required-field changes and enum incompatibility as failures.

### Layer B: Adapter Golden Tests (`CodexAdapter`)

Purpose:
- Verify ATM normalization preserves Codex semantics.

Coverage:
- ordering and grouping,
- lifecycle transitions (`turn_started`, `item_started`, deltas, completion, idle),
- source metadata (`client_prompt`, `atm_mail`, `user_steer`),
- approval/interrupt/error paths,
- unknown event handling + counters.

Mechanics:
- input fixture stream -> adapter -> normalized output stream.
- compare output to checked-in golden files.

### Layer C: Renderer Snapshot/Frame Tests

Purpose:
- Verify visible terminal parity behavior.

Coverage:
- markdown/code block formatting,
- status rows and progress states,
- width/height reflow on resize,
- attach replay (last N lines),
- reconnect redraw continuity.

Mechanics:
- normalized events -> renderer -> terminal frame snapshots.
- compare snapshots against expected outputs per viewport profile.

## 3. Fixture Format

Directory layout:

```text
crates/atm-agent-mcp/tests/fixtures/parity/
  contract/
    cli-json/
    app-server/
    mcp/
  adapter/
    <scenario>/
      input.events.jsonl
      expected.normalized.jsonl
      meta.toml
  renderer/
    <scenario>/
      normalized.events.jsonl
      viewport-120x36.snap
      viewport-80x24.snap
      meta.toml
```

`meta.toml` minimum fields:

```toml
name = "approval-interrupt"
transport = "cli-json"
codex_version = "0.15.0"
schema_version = "v1"
notes = "approval request, reject, retry, then cancel"
```

Event record requirements:
- one JSON object per line (JSONL),
- deterministic ordering,
- timestamps normalized or removed where not semantically relevant,
- opaque IDs replaced with stable fixture IDs where needed.

## 4. Scenario Matrix (Minimum)

### Core happy path
1. New turn, simple response, completed, idle.
2. Tool call with streamed deltas and final output.
3. Multi-item turn with mixed text/tool items.

### Control flow and safety
4. Approval requested -> approved -> tool executes.
5. Approval requested -> rejected -> fallback response.
6. Interrupt/cancel mid-stream.
7. Child process fatal error (no auto-restart per FR-11.2) -> surfaced terminal state.
8. Unknown event type encountered (count + continue).

### ATM-specific integration
9. ATM mail injection turn with source labels.
10. Primary client prompt and local user steer in same session.
11. Session attach with 50-line replay.
12. Detach/reattach and reconnect continuity.

### Transport coverage
13. Repeat representative scenarios on all transports:
   - `mcp`
   - `cli-json`
   - `app-server`

## 5. CI Matrix

Run parity suites in CI on:
- Ubuntu latest
- macOS latest
- Windows latest

Job gates:
1. `parity-contract` (schema/contract fixtures)
2. `parity-adapter` (golden normalization)
3. `parity-render` (snapshot/frame tests)

Failure policy:
- any parity diff is blocking,
- snapshot updates require explicit review and rationale in PR notes.

## 6. Drift Management

When upgrading Codex version:
1. capture fresh transcripts for baseline scenarios,
2. regenerate candidate fixtures,
3. inspect diffs for semantic regressions vs acceptable upstream changes,
4. update `codex_version` in fixture metadata,
5. re-approve golden/snapshots with reviewer sign-off.

## 7. Pre-User-Testing Exit Criteria

Automated pass requirements:
- contract tests pass for all supported transports,
- adapter golden tests pass for full scenario matrix,
- renderer snapshots pass for standard viewports on all OS targets,
- replay/reconnect stress tests pass without state/order corruption.

Only after these are green should subjective user validation begin.

## 8. Implementation Notes

- Prefer reusing captured real Codex transcripts over synthetic-only fixtures.
- Keep fixture payloads minimal but semantically complete.
- Keep deterministic sanitization rules in one helper module to avoid fixture churn.
- Do not route continuous stream through daemon for parity tests; keep MCP->TUI stream path direct per Phase L/M architecture.
