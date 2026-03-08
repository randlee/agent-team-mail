# Phase AD Test Plan: Python Runtime Policy + Runtime-Aware Init

## Scope

This plan defines deterministic coverage for Phase AD contracts:
- Python-only product runtime script policy (`docs/requirements.md` §4.9.3a)
- Runtime detection + auto-install contract (`docs/requirements.md` §4.9.5)
- GH init and disabled guidance requirements (`docs/requirements.md` §4.11)
- GH monitor dogfood regressions (`docs/plugins/ci-monitor/requirements.md` GH-CI-FR-19..24, GH-CI-TR-7)

## Sprint Mapping

| Sprint | Focus | Primary Issues |
|---|---|---|
| AD.1 | Requirements + policy lock + test matrix | #500, #499 |
| AD.2 | Runtime config discovery parity | #499 |
| AD.3 | Status JSON and output consistency | #504, #507 |
| AD.4 | Reload/live-state/reachability consistency | #502, #503, #505 |
| AD.5 | Python migration for runtime scripts | #500 |
| AD.6 | Residual shell-wrapper cleanup (candidate) | #499, #500 |

## AD.1 Verification Matrix (Requirements Lock)

### 1) Python-Only Runtime Policy (§4.9.3a)

| Scenario | Method | Expected Result |
|---|---|---|
| Product hook wiring uses Python directly | Static assertion on generated hook command strings | No `bash -c`/`sh -c`/`pwsh` runtime wrappers in product paths |
| Runtime launcher/relay scripts are Python-based | File/path audit in CI + unit assertions | Product runtime script references resolve to `.py` assets |
| Dev-only shell exceptions are non-runtime | Documentation + path classification check | Shell scripts only in explicitly documented dev/CI exception sets |

### 2) Runtime Detection + Auto-Install Contract (§4.9.5)

| Scenario | Method | Expected Result |
|---|---|---|
| Claude detected via config path | Integration test with mocked project/global config files | Claude runtime marked detected |
| Codex detected via PATH binary | Integration test with injected PATH fixture | Codex runtime marked detected |
| Gemini detected via config directory | Integration test with `~/.gemini` fixture | Gemini runtime marked detected |
| Runtime absent | Integration test with no binary/config | Runtime status `skipped-not-detected`, command succeeds |
| Re-run idempotency | Execute `atm init` twice in same fixture | No duplicate hook entries; second run `already-configured`/`updated` only where needed |
| Dry-run semantics | `atm init --dry-run` | Reports per-runtime planned actions with zero writes |

### 3) GH Init + Disabled Guidance (§4.11 / GH-CI-FR-20, FR-24)

| Scenario | Method | Expected Result |
|---|---|---|
| Plugin disabled status surface | `atm gh` and `atm gh status` in unconfigured fixture | Explicit disabled reason + `atm gh init` guidance + required key hints |
| `atm gh init` happy path | Integration test with valid `gh` fixture | Writes required config keys and emits next-step summary |
| `atm gh init` dry-run | Integration test | Shows target config path/keys; no file mutation |
| Disabled command gating | `atm gh monitor ...` while disabled | Fast fail with actionable `atm gh init` remediation |

## AD.2-AD.6 Required Regression Coverage

### A) Config Discovery / Path Parity (AD.2)
- daemon-start context and CLI context resolve identical effective config.
- Repo-local and global fallback precedence is deterministic and test-verified.

### B) Status / JSON / Output Consistency (AD.3)
- `atm gh monitor status --json` schema is stable.
- Human status output has one canonical status block (no duplication).

### C) Reload + Live State + Reachability (AD.4)
- Restart/reload applies changed config without manual daemon kill.
- Status commands reflect live daemon state, not stale cache-only state.
- Reachability outcomes are consistent across status and monitor command paths.

### D) Script Migration (AD.5/AD.6)
- Migrated runtime scripts execute on macOS/Linux/Windows test fixtures.
- Hook install and runtime invocation paths remain idempotent after migration.
- No undocumented shell runtime dependency remains in product execution paths.

## CI Lane Expectations

1. `cargo test` coverage for CLI/daemon command contracts.
2. `python3 -m pytest tests/hook-scripts/ -q` as required runtime-script lane.
3. Platform matrix coverage (Linux/macOS/Windows) for cross-platform runtime behavior.
