# Phase U: Doctor Hardening + Phase T Verification

**Goal**: (1) Fix all `atm doctor` reliability and UX gaps identified in post-Phase-T dogfooding. (2) Functionally verify all Phase T sprint deliverables against the v0.27.0 release — code was merged but issues were not closed because verification was not performed.

**Integration branch**: `integrate/phase-U` off `develop`.

**Priority order**: Doctor fixes first (blocking daily workflow), then verification sprints (close or reopen issues based on evidence).

---

## Part 1: Doctor Hardening (7 findings, 3 sprints)

### U.1 — Doctor scoping + teardown classification + recommendation routing

**Issues addressed**: Cross-team doctor bleed (`DAEMON_TRACKS_UNKNOWN_AGENT`), `PARTIAL_TEARDOWN` lead misclassification, cleanup recommendation loop.

**Root causes** (from arch-ctm's analysis):
- `check_roster_session_integrity` calls unscoped `query_list_agents()` — tracked agents from all teams leak into a single-team doctor run
- `check_mailbox_integrity` treats `team-lead` same as non-lead members; lead is intentionally retained by cleanup, so missing mailbox + dead session is expected transiently and over-classified as critical
- `build_recommendations` suggests `atm teams cleanup` for `PARTIAL_TEARDOWN` but cleanup intentionally skips team-lead — non-actionable recommendation loop

**Deliverables**:
1. `check_roster_session_integrity`: pass team context to `query_list_agents()`, filter results to team members only
2. `check_mailbox_integrity`: split lead vs non-lead teardown classification; lead gets explicit guidance (register/recreate), not critical-stale
3. `build_recommendations`: route lead findings to session repair command, non-lead findings to cleanup
4. Tests: no cross-team bleed for any check; lead teardown classified correctly; recommendation text verified per finding class

**Acceptance criteria**:
- `atm doctor --team atm-dev` does not report agents from other teams
- `PARTIAL_TEARDOWN` on team-lead classified as actionable session drift (not critical stale-member)
- Recommendation for lead teardown is `atm register` / session repair, not `atm teams cleanup`

---

### U.2 — Lifecycle teardown hardening (ACTIVE_WITHOUT_SESSION + TERMINAL_MEMBER_NOT_CLEANED)

**Issues addressed**: `ACTIVE_WITHOUT_SESSION` after restore/recreate, `TERMINAL_MEMBER_NOT_CLEANED` for dead non-lead members.

**Root causes**:
- `check_pid_session_reconciliation` warns when `is_active == true` and session registry returns `None`; restore/recreate paths leave `is_active` stale while daemon session registry is reset/rebuilt
- `check_mailbox_integrity` detects dead session + mailbox + roster for non-lead correctly, but cleanup is not guaranteed to complete across all kill/timeout termination paths

**Deliverables**:
1. Add deterministic reconciliation step after restore/recreate (and daemon startup): recompute `is_active` from live session registry before first doctor check
2. Harden termination/cleanup orchestration: dead non-lead members removed from roster + mailbox atomically across all kill/timeout paths
3. Tests: restore/recreate transitions leave no stale `is_active`; teardown convergence tests across kill/timeout paths

**Acceptance criteria**:
- `atm doctor` shows no `ACTIVE_WITHOUT_SESSION` warnings immediately after team restore
- Dead non-lead members do not persist in roster or mailbox after any termination path

---

### U.3 — Doctor UX: member snapshot header + context-aware recommendations

**Issues addressed**: Missing member-status snapshot header in doctor output, `atm register` recommendation when `CLAUDE_SESSION_ID` unavailable.

**Root causes**:
- Human output prints findings first without `atm members`-style table — weakens triage clarity in degraded states
- `build_recommendations` unconditionally emits `atm register <team>` for `ACTIVE_WITHOUT_SESSION`/`ACTIVE_FLAG_STALE` regardless of whether session id is resolvable

**Deliverables**:
1. Prepend doctor human output with concise member snapshot table (name/type/status) before findings; JSON schema unchanged
2. `build_recommendations`: check session id availability; emit `--as` guidance or managed-session fallback when `CLAUDE_SESSION_ID` is not resolvable
3. Tests: human output format verified (snapshot before findings); recommendation text per environment context

**Acceptance criteria**:
- `atm doctor` human output shows member table before findings list
- In plain shell (no `CLAUDE_SESSION_ID`), recommendation includes `--as` or actionable fallback — not bare `atm register`
- JSON output schema unchanged

---

## Part 2: Phase T Verification Sprints

Each sprint verifies Phase T deliverables against v0.27.0 on `develop`. Verification agents run tests, CLI commands, and behavioral checks. If a check passes → close the GitHub issue. If it fails → issue stays open, bug is documented, fix sprint added to Phase U backlog.

### U.4 — Daemon reliability verification (#181, #182, #183)

**Sprint PRs**: #288 (T.1), #289 (T.2)

**Verification checklist**:

| Check | Issue | Command / Test |
|-------|-------|----------------|
| Daemon auto-starts on `atm status` with no running daemon | #181 | Kill daemon, run `atm status`, confirm daemon started, no manual intervention |
| Daemon auto-starts on `atm doctor` | #181 | Kill daemon, run `atm doctor`, confirm daemon-backed result |
| `atm members` shows all config.json roster on fresh daemon start | #182 | Start fresh daemon with populated config.json, verify all members appear |
| Config.json member add reflected within one watch cycle | #182 | `atm teams add-member`, verify daemon roster updated |
| Agent state transitions after registration | #183 | Register agent, trigger event, verify state transitions (idle→active→idle) |
| `cargo test -p agent-team-mail-daemon daemon_autostart` | #181 | All daemon autostart tests pass |
| `cargo test -p agent-team-mail-daemon roster` | #182/#183 | All roster/state tests pass |

**Pass criteria**: All checks pass → close #181, #182, #183. Any failure → document regression, add fix sprint.

---

### U.5 — Gemini runtime verification (#281, #282)

**Sprint PRs**: #296 (T.3), #297 (T.4)

**Verification checklist**:

| Check | Issue | Command / Test |
|-------|-------|----------------|
| GeminiAdapter present in daemon codebase | #282 | `grep -r "GeminiAdapter" crates/atm-daemon/src/` |
| Gemini spawn env vars set correctly (`GEMINI_CLI_HOME`, `ATM_RUNTIME_HOME`) | #282 | `cargo test -p agent-team-mail-daemon test_handle_launch_gemini_` |
| Runtime metadata persisted and queryable | #282 | Test `runtime`, `runtime_session_id`, `runtime_home` fields |
| Resume binds to correct prior session | #281 | `cargo test -p agent-team-mail-daemon test_resume_` |
| Resume does not drift to wrong session/flags | #281 | Explicit override + mismatch tests |

**Pass criteria**: All tests pass, adapter present → close #281, #282.

---

### U.6 — CLI publishability + atm-monitor verification (#284, #286)

**Sprint PRs**: #290 (T.5a), #294 (T.5b)

**Verification checklist**:

| Check | Issue | Command / Test |
|-------|-------|----------------|
| `cargo package -p agent-team-mail --locked` succeeds | #284 | No `include_str!` path errors outside crate |
| `cargo publish -p agent-team-mail --dry-run --locked` succeeds | #284 | Dry run clean |
| `atm monitor` subcommand exists in binary | #286 | `atm monitor --help` |
| Monitor polls doctor and sends ATM mail on findings | #286 | `cargo test -p agent-team-mail monitor` |
| Alert deduplication: same finding suppressed within cooldown window | #286 | Unit tests for dedupe logic |
| v0.27.0 published on crates.io | #284 | `cargo search agent-team-mail` shows 0.27.0 |

**Pass criteria**: All checks pass → close #284, #286.

---

### U.7 — Availability signaling verification (#46, #47)

**Sprint PR**: #295 (T.5c)

**Note**: T.5c was a design/clarification sprint — deliverables are contract normalization and idempotency, not a full pub/sub implementation. Verification scope is confirming the design boundaries are documented and enforced, not live Gemini pub/sub.

**Verification checklist**:

| Check | Issue | Command / Test |
|-------|-------|----------------|
| Availability event payload contract documented (state, timestamp, idempotency_key) | #46/#47 | Check requirements.md and hook relay script |
| Duplicate hook events do not double-transition agent state | #46 | `cargo test -p agent-team-mail-daemon hook_watcher` idempotency tests |
| Daemon reconciliation polling fallback exists for dropped FS notifications | #47 | `cargo test -p agent-team-mail-daemon agent_state_integration` |
| Relay script emits canonical fields | #46/#47 | Check relay script for `state`, `timestamp`, `idempotency_key` |

**Pass criteria**: Design boundaries documented, idempotency tests pass → close #46, #47 (or re-scope as separate enhancement if full pub/sub is still unimplemented).

---

### U.8 — TUI verification (#184, #185, #187)

**Sprint PR**: #299 (T.6)

**Verification checklist**:

| Check | Issue | Command / Test |
|-------|-------|----------------|
| TUI left/right panel state derives from same source | #184 | `cargo test -p agent-team-mail-tui` panel consistency tests |
| Message list view shows inbox messages | #185 | `cargo test -p agent-team-mail-tui` message list tests |
| Message detail view renders full content | #185 | Detail view test |
| Mark-read persists to inbox file with lock-protected write | #185 | Lock write test |
| TUI header shows version number | #187 | `cargo test -p agent-team-mail-tui` header render test |

**Pass criteria**: All TUI tests pass, visual checks confirm → close #184, #185, #187.

---

## Part 3: Deferred Items (Phase U Backlog)

| Item | Source | Priority | Notes |
|------|--------|----------|-------|
| Tmux sentinel injection (#45) | T.11 (never started) | Medium | Runtime signaling improvement — schedule if capacity allows |
| S.2a/S.1 plan accuracy (#283) | T.16 (never started) | Medium | Doc-only — can be done outside sprint structure |
| Env-var `#[serial]` violations in daemon integration tests | Tech debt | Medium | 27 tests across 3 files; flakiness risk on parallel CI |
| `atm teams add-member` does not create inbox file | New bug (observed) | High | Blocks reliable team member onboarding |
| Version bump cherry-pick (v0.27.0 → develop) | Release hygiene | High | `release/v0.27.0` has version bump not yet on develop |

---

## Sprint Summary

| Sprint | Description | Issues | Type | Depends On |
|--------|-------------|--------|------|------------|
| U.1 | Doctor scoping + teardown classification + recommendation routing | Doctor findings 1,2,3 | Fix | — |
| U.2 | Lifecycle teardown hardening | Doctor findings 4,5 | Fix | — |
| U.3 | Doctor UX: member snapshot + context-aware recommendations | Doctor findings 6,7 | Fix/UX | — |
| U.4 | Daemon reliability verification | #181, #182, #183 | Verify | U.1, U.2, U.3 |
| U.5 | Gemini runtime verification | #281, #282 | Verify | U.1, U.2, U.3 |
| U.6 | CLI publishability + atm-monitor verification | #284, #286 | Verify | U.1, U.2, U.3 |
| U.7 | Availability signaling verification | #46, #47 | Verify | U.1, U.2, U.3 |
| U.8 | TUI verification | #184, #185, #187 | Verify | U.1, U.2, U.3 |

**Sequencing constraint**: U.1/U.2/U.3 (doctor hardening) MUST complete and merge before U.4–U.8 begin. Doctor is used as a verification tool in the Phase T verification sprints — it must be reliable first.

**Parallel tracks within each wave**: U.1/U.2/U.3 can run in parallel with each other. U.4–U.8 can run in parallel with each other once the doctor wave is complete.

