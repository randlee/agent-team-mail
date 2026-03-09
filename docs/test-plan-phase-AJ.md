# Test Plan — Phase AJ (Caller/Session Resolution SSoT)

## Scope

Validate that caller identity and runtime session resolution are daemon-backed,
runtime-aware, and deterministic across `atm send`, `atm read`, `atm register`,
and `atm doctor`.

## Requirement Mapping

| Issue | Sprint | Requirement Focus | Planned Test Coverage |
|---|---|---|---|
| #593 | AJ.1 | shared resolver + no ambiguity in send paths | unit tests for resolver precedence; integration tests for `send` with multiple stale/active contexts |
| #595 | AJ.1 | no dependence on `CLAUDE_SESSION_ID` in non-Claude runtime paths | command tests where Claude env var is absent but runtime session is resolvable |
| #597 | AJ.2 | Codex/Gemini runtime session resolution | unit tests for Codex/Gemini resolution adapters; Gemini list-sessions parse tests; fallback tests |
| #594 | AJ.3 | stale-session cleanup lifecycle | daemon/session-registry tests for last_seen + safe cleanup eligibility |
| #596 | AJ.4 | doctor session-id display format | snapshot tests for 8-char short format in doctor/members surfaces |
| #593/#597 | AJ.5 | spawn env + resume/continue contract | spawn tests for env injection and resume/continue behavior |

## Planned Test Suites

## AJ.1 — Resolver SSoT + Synthetic ID Removal

- `caller_identity` unit tests:
  - explicit identity/session wins
  - daemon query authoritative path
  - ambiguous daemon candidates -> `CALLER_AMBIGUOUS`
  - unresolved path -> `CALLER_UNRESOLVED`
  - runtime aliases (`thread-id`, runtime-native session fields) normalize to
    canonical `session_id`
  - `agent_id` never accepted as a substitute for `session_id`
- Regression guard:
  - assert no synthetic `local:` session IDs are emitted in resolver outputs.

## AJ.2 — Runtime Session Resolution (Codex/Gemini)

- Codex:
  - resolves via `CODEX_THREAD_ID` when set
  - unresolved when missing and no daemon/runtime hint exists
- Gemini:
  - resolves via hook/session payload when present
  - project-scoped `gemini --list-sessions` parse success/failure cases
  - file fallback parse (`chats/session-*.json`, `logs.json`)
  - ambiguity handling on multiple candidate matches

## AJ.3 — Session Lifecycle and Cleanup

- Session registry tests:
  - upsert replacement for same `(team, agent)` updates active session
  - `last_seen` heartbeat updates on successful resolution/send
  - stale cleanup skips active/living sessions and only removes eligible stale records
- Integration tests:
  - daemon restart + registry replay does not duplicate active session ownership.

## AJ.4 — Doctor/Display Consistency

- `doctor` and `members` output tests:
  - session id always rendered as short 8-char prefix in table surfaces
  - JSON output preserves full canonical IDs while human output stays short
  - no mixed-format rows in same report.

## AJ.5 — Spawn Env and Resume/Continue

- Spawn command tests:
  - required env vars always set: `ATM_TEAM`, `ATM_IDENTITY`, `ATM_RUNTIME`, `ATM_PROJECT_DIR`
  - optional `ATM_SESSION_ID` only when known
  - `--resume <id>` and `--continue` mutual exclusion enforcement
  - prefix ID normalization succeeds only for unique match
  - non-unique/unknown prefix -> `SESSION_ID_AMBIGUOUS` / `SESSION_ID_NOT_FOUND`

## CI Gates

- `cargo fmt --check --all`
- `cargo clippy --workspace -- -D warnings`
- targeted unit/integration tests for AJ suites above on macOS/Linux/Windows

## Exit Criteria

1. All AJ mapped tests are implemented and passing in CI.
2. No synthetic session-id behavior remains in CLI/daemon code paths.
3. Runtime-aware resolution works without manual CLAUDE-only environment hacks.
