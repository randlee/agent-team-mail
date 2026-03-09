# Phase AJ Planning — Caller Identity and Runtime Session Resolution SSoT

## Goal

Eliminate caller/session ambiguity in ATM command paths by making daemon-backed
resolution the single source of truth and removing synthetic/fallback session
identity behavior.

## Delivery Target

- Target version: `v0.44.x` (planning-level; final tag decided at release)
- Integration branch: `integrate/phase-AJ`

## Inputs

- #593 — `atm send` requires manual `CLAUDE_SESSION_ID` in ambiguous paths
- #594 — stale session entries accumulate (no safe cleanup lifecycle)
- #595 — `CLAUDE_SESSION_ID` propagation gaps in bash subshell/tool flows
- #596 — `atm doctor` session-id display format inconsistency (8-char prefix)
- #597 — Codex/Gemini runtime session IDs not resolved correctly at registration

## Root Cause Summary (from dogfooding + architecture review)

- `send` currently relies on layered fallback/session-file scan behavior that can
  yield ambiguity for the same `(team, identity)` after multiple sessions.
- Runtime-specific session identity is not resolved through one shared path.
- Terminology drift (`session-id`, `thread-id`, and occasional `agent-id` usage)
  causes inconsistent handling and operator confusion.
- CLI command paths duplicate resolution behavior instead of using one resolver.
- Doctor output formatting and runtime-session surfaces are not uniformly derived
  from the same canonical source.

## Phase Scope

1. Shared resolver SSoT
- Introduce centralized `caller_identity.rs` contract used by `send/read/register/doctor`.
- Daemon session registry is authoritative for active `(team, agent, runtime, session)` state.
- ATM boundary terminology is canonicalized to `session_id`; runtime-specific
  names are adapter internals only.

2. Synthetic/fallback removal
- Remove synthetic session-id fabrication (`local:<...>` patterns).
- Session-file scans become bootstrap-only, not authoritative resolution.

3. Runtime-aware resolution closure
- Claude: hook/session + `CLAUDE_SESSION_ID`.
- Codex: `CODEX_THREAD_ID`.
- Gemini: hook/session ID, then project-scoped `gemini --list-sessions`, then file fallback.
- OpenCode: deferred implementation; explicit unresolved behavior until adapter lands.

4. Spawn contract normalization
- `atm teams spawn` sets canonical env context (`ATM_TEAM`, `ATM_IDENTITY`,
  `ATM_RUNTIME`, `ATM_PROJECT_DIR`, optional `ATM_SESSION_ID`).
- Support `--resume <session_id>` and `--continue` with runtime-native semantics.
- Prefix session IDs allowed only if uniquely resolvable to full ID.

5. Diagnostics/output consistency
- `atm doctor`/`atm members` session display uses stable short format (8-char prefix).
- Resolver failures return explicit stable error codes and actionable fixes.

## Proposed Sprint Map

| Sprint | Focus | Issues | Size |
|---|---|---|---|
| AJ.1 | Resolver SSoT + `send`/`read` integration (no synthetic IDs) | #593, #595 | M |
| AJ.2 | Runtime-specific session resolution (Codex + Gemini) | #597 | M |
| AJ.3 | Session lifecycle/cleanup reliability (`last_seen`, stale handling) | #594 | M |
| AJ.4 | Doctor/session display consistency + short-id formatting | #596 | S |
| AJ.5 | Spawn contract/env normalization + resume/continue semantics | #593, #597 | M |

## Dependency Graph

- AJ.1 is foundational for all command-path resolution changes.
- AJ.2 depends on AJ.1.
- AJ.3 depends on AJ.1.
- AJ.4 depends on AJ.1.
- AJ.5 depends on AJ.1 + AJ.2.

## Acceptance Criteria

1. No command path fabricates synthetic session IDs.
2. `send/read/register/doctor` share one caller/session resolver contract.
3. Ambiguous/unresolved identity cases fail deterministically with stable errors.
4. Codex/Gemini runtime session IDs resolve without requiring manual Claude-only env vars.
5. Doctor session ID presentation is consistent and human-readable (8-char prefix).

## Out of Scope

- OpenCode runtime session resolution implementation details (tracked for follow-on sprint).
- Release workflow changes unrelated to caller/session resolution.
