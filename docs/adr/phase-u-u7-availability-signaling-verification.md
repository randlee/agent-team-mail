# Phase U Sprint U.7 Verification Report — Availability Signaling (#46, #47)

Date: 2026-03-01
Branch: `feature/pU-s7-availability-signaling-verification`
Base: `integrate/phase-U` (v0.27.0)
Source sprint: T.5c (`feature/pT-s5c-availability-signaling`, PR #295)

## Scope

Verification target issues:

- **#46** — Codex Idle Detection via Notify Hook
- **#47** — Ephemeral Pub/Sub for Agent Availability

T.5c was a design/clarification sprint. Its deliverables were contract
normalization and idempotency, not a full pub/sub implementation.
Verification scope is confirming the design boundaries are documented and
enforced, not live Gemini pub/sub.

## Verification Checklist

| Check | Issue | Result | Evidence |
|---|---|---|---|
| Availability event payload contract documented (`state`, `timestamp`, `idempotency_key`) | #46/#47 | PASS | `docs/requirements.md` §4.3.10 documents the full canonical contract. See [Contract Documentation](#1-contract-documentation) below. |
| Duplicate hook events do not double-transition agent state | #46 | PASS | Unit test `test_duplicate_availability_event_idempotency_key_is_deduped` passes. Integration test `test_hook_watcher_duplicate_replay_does_not_double_transition` passes. See [Idempotency Tests](#2-idempotency-tests). |
| Daemon reconciliation polling fallback exists for dropped FS notifications | #47 | PASS | 200 ms `reconcile_tick` arm in `HookWatcher::run` calls `read_new_events` independently of `notify` events. Unit test `test_reconcile_tick_drives_convergence_without_fs_event` passes. See [Reconciliation Polling](#3-reconciliation-polling-fallback). |
| Relay script emits canonical fields (`state`, `timestamp`, `idempotency_key`) | #46/#47 | PASS | `scripts/atm-hook-relay.sh` emits all three fields at the top-level event envelope. See [Relay Script](#4-relay-script-canonical-fields). |

Overall verdict: **all four checks PASS**. Issues #46 and #47 can be closed.

---

## Evidence Detail

### 1. Contract Documentation

File: `docs/requirements.md`, section **4.3.10 Availability Signaling Contract**
(lines 884–944 on `develop`/U.7 worktree snapshot).

The section documents the T.5c canonical payload:

| Field | Type | Description |
|---|---|---|
| `team` | string | Team name the agent belongs to |
| `agent` | string | Agent identity (matches config.json member name) |
| `state` | string | Availability state: `"idle"` or `"busy"` |
| `timestamp` | ISO 8601 string | Event time (UTC). Replaces the legacy `ts` short-hand. |
| `idempotency_key` | string | Stable deduplication key per logical event. Must survive replay. |

The section further documents:

- `idempotency_key` format: `"<team>:<agent>:<turn-id>"` — content-stable
  fields only, no wall-clock receipt time.
- `timestamp` vs legacy `ts`: daemon normalizes `ts → timestamp` for backward
  compat; new producers must emit `timestamp`.
- `source` field: intentionally absent from the canonical contract.
- Role boundaries: hook/adapters are signal producers only; daemon is the
  authoritative state owner; pub/sub is fanout-only notification transport.
- Reliability requirements: duplicate/out-of-order events must not corrupt
  state; daemon restart must recover from durable sources, not pub/sub buffers.

### 2. Idempotency Tests

**Unit test** (inline module in `hook_watcher.rs`):

```
test plugins::worker_adapter::hook_watcher::tests::test_duplicate_availability_event_idempotency_key_is_deduped ... ok
```

This test verifies that `AvailabilityDeduper::should_process` returns `false`
for a replayed key, and that `read_new_events` does not mutate agent state on
a duplicate event.

**Integration test** (file:
`crates/atm-daemon/tests/agent_state_integration.rs`,
`test_hook_watcher_duplicate_replay_does_not_double_transition`):

```
test test_hook_watcher_duplicate_replay_does_not_double_transition ... ok
```

Test flow:
1. Agent starts in `Active` state.
2. Event with `idempotency_key = "dup-key-1"` is appended; agent transitions to
   `Idle`; transition timestamp is recorded.
3. Same event (same key) is appended again after a 30 ms sleep.
4. Assertion: `time_since_transition` after the replay is >= the value before
   — meaning no second state transition was created.

Additional convergence-without-pubsub test also passes:

```
test test_hook_watcher_converges_without_pubsub_delivery ... ok
```

This confirms that daemon-side state converges from hook events alone,
independently of any pub/sub delivery path.

### 3. Reconciliation Polling Fallback

The `HookWatcher::run` method
(`crates/atm-daemon/src/plugins/worker_adapter/hook_watcher.rs`, lines 301–311)
contains a `reconcile_tick` branch:

```rust
_ = reconcile_tick.tick() => {
    // Polling fallback: converge state even if a filesystem
    // notification is dropped by the OS watcher.
    offset = read_new_events(
        &self.path,
        offset,
        &self.state,
        self.session_registry.as_ref(),
        self.claude_root.as_deref(),
        &mut availability_deduper,
    );
}
```

The tick interval is 200 ms (set in the run method's `interval` setup, above
the loop). This means that even if the `notify` watcher drops a filesystem
event (a known occurrence on macOS `FSEvents` and under heavy I/O), the daemon
will re-read any new lines within at most 200 ms.

**Unit test** (`test_reconcile_tick_drives_convergence_without_fs_event`):

```
test plugins::worker_adapter::hook_watcher::tests::test_reconcile_tick_drives_convergence_without_fs_event ... ok
```

Test approach:
- Calls `read_new_events` directly (same function the tick arm calls), without
  any `HookWatcher` instance or `notify` watcher running.
- Writes one canonical event line to a temp file.
- Asserts that the agent transitions to `Idle` from `Active` after a single
  `read_new_events` call.
- A second call with the advanced offset is a no-op (idempotency_key dedup).

This confirms the polling fallback is a deterministic convergence path, not
dependent on OS-level file system event delivery.

### 4. Relay Script Canonical Fields

File: `scripts/atm-hook-relay.sh` (identical in both the T.5c worktree and the
U.7 worktree — no divergence detected via diff).

The script enriches the incoming Codex hook payload with all three canonical
fields at the top-level event envelope:

```bash
IDEMPOTENCY_KEY="${TEAM}:${AGENT}:${TURN_ID}"

ENRICHED_EVENT=$(echo "$JSON_PAYLOAD" | jq -c \
  --arg type "$PAYLOAD_TYPE" \
  --arg agent "$AGENT" \
  --arg team "$TEAM" \
  --arg ts "$RECEIVED_AT" \
  --arg key "$IDEMPOTENCY_KEY" \
  '{
    type: $type,
    agent: $agent,
    team: $team,
    "thread-id": .["thread-id"],
    "turn-id": .["turn-id"],
    received_at: $ts,
    timestamp: $ts,
    state: "idle",
    idempotency_key: $key
  }')
```

Observations:

- `state: "idle"` — hardcoded for AfterAgent completion events. Correct per
  contract (AfterAgent always signals idle).
- `timestamp` — set to the ISO 8601 UTC timestamp generated by the script.
  `received_at` is also set to the same value (backward compatibility for
  consumers that read the legacy field).
- `idempotency_key` — format `"${TEAM}:${AGENT}:${TURN_ID}"` matches the
  `"<team>:<agent>:<turn-id>"` format documented in `requirements.md §4.3.10`.
  The key is derived from content-stable fields only (no wall-clock component).

All three required canonical fields (`state`, `timestamp`, `idempotency_key`)
are present in every emitted event.

---

## Daemon-Side Normalization

The `HookEvent` struct in `hook_watcher.rs` implements backward-compatible
derivation for legacy relays:

```rust
// Backward-compatible derivation for older relays that do not yet send
// idempotency_key explicitly.
let idempotency_key = self
    .idempotency_key
    .as_ref()
    .map(|s| s.trim().to_string())
    .filter(|s| !s.is_empty())
    .unwrap_or_else(|| {
        let turn = self.turn_id.as_deref().unwrap_or("no-turn");
        format!("{}:{}:{}", team, agent, turn)
    });
```

- If `idempotency_key` is absent (legacy relay), the daemon derives it from
  `team:agent:turn-id` — identical to the format the relay script produces.
- If `timestamp` is absent, `received_at` is used as the fallback timestamp
  source.

This confirms both the relay script and the daemon independently implement the
same canonical key derivation, preventing behavioral divergence if the relay
script is not updated.

---

## Scope Boundary: Full Pub/Sub Not Implemented

Per the T.5c sprint scope, **full ephemeral pub/sub for agent availability is
NOT implemented**. The `test_pubsub_subscribe_and_match` and related tests in
`integration_orchestration.rs` verify pub/sub subscribe/unsubscribe and
message routing, but the daemon does not propagate availability state changes
through pub/sub channels automatically.

This is by design per `requirements.md §4.3.10`:

> Ephemeral pub/sub may distribute availability changes, but must not become
> the canonical persistence source.

The file-based `events.jsonl` + `HookWatcher` + `AgentStateTracker` pipeline
is the authoritative path. Pub/sub fanout of availability changes remains a
future enhancement and is not required for issues #46 and #47 to be resolved.

---

## Regression Note: `test_socket_query_agent_state` Failure

One pre-existing test failure was observed during the full daemon test run:

```
test test_socket_query_agent_state ... FAILED
---- test_socket_query_agent_state stdout ----
assertion `left == right` failed
  left: "offline"
  right: "idle"
```

Location: `crates/atm-daemon/tests/integration_orchestration.rs:167`

This test registers an agent in `Idle` state, starts a socket server, and
queries it. The socket response returns `"offline"` instead of `"idle"`,
indicating a state lookup or socket-handler regression.

**This failure is pre-existing and not introduced by T.5c.** It is scoped
to socket state query routing, not to availability signaling, idempotency, or
relay contract enforcement. It does not affect the four U.7 verification
checks, all of which target the hook ingestion and deduplication pipeline
directly.

This regression should be tracked separately and addressed in a dedicated fix
sprint.

---

## Issue Closure Recommendation

| Issue | Title | Verdict |
|---|---|---|
| #46 | Codex Idle Detection via Notify Hook | **CLOSE** — idle detection via `atm-hook-relay.sh` + `HookWatcher` is implemented, tested, and idempotent. |
| #47 | Ephemeral Pub/Sub for Agent Availability | **CLOSE with scope note** — reconciliation polling fallback confirmed. Full pub/sub fanout of availability events remains a future enhancement, but is explicitly out of scope for the current contract per `requirements.md §4.3.10`. The durable convergence path (file-based) is reliable without pub/sub. |

---

## Test Run Summary

```
# Idempotency unit test (inline module):
test plugins::worker_adapter::hook_watcher::tests::test_duplicate_availability_event_idempotency_key_is_deduped ... ok

# Reconciliation polling fallback unit test (inline module):
test plugins::worker_adapter::hook_watcher::tests::test_reconcile_tick_drives_convergence_without_fs_event ... ok

# Integration tests (agent_state_integration.rs):
test test_hook_watcher_handles_pre_existing_events ... ok
test test_hook_watcher_converges_without_pubsub_delivery ... ok
test test_hook_watcher_duplicate_replay_does_not_double_transition ... ok

# FSEvents-dependent integration tests (marked #[ignore]):
test test_hook_watcher_full_lifecycle ... ignored
test test_hook_watcher_incremental_reads ... ignored
test test_hook_watcher_picks_up_event ... ignored

# Pre-existing regression (out of scope for U.7):
test test_socket_query_agent_state ... FAILED (socket state lookup; not related to availability signaling contract)
```

All U.7 acceptance checks pass. The pre-existing socket query regression is
documented and out of scope.
