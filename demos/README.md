# ATM TUI Demo Scripts

## `tui-demo.sh` — Sprint D.3 User Demo

**Status**: Team-lead reviewed and signed off.
**Sprint**: D.3 — Identifier Cleanup + User Demo
**Date**: 2026-02-20

### Prerequisites

```bash
# Build workspace
cargo build --workspace

# Add debug binaries to PATH
export PATH="$PWD/target/debug:$PATH"

# Optional: set team/agent for demo
export ATM_TEAM=atm-dev
export ATM_AGENT=arch-ctm
```

### Running

```bash
./demos/tui-demo.sh
```

### Scenarios

| # | Scenario | Covers |
|---|----------|--------|
| 1 | Dashboard: team + member status | `atm teams`, `atm members` |
| 2 | Agent Terminal: session state | Session log directory, stream preview |
| 3 | Control Protocol send/ack | `control.stdin.request` shape (public API, no `thread_id`) |
| 4 | Degraded: daemon unavailable | `result=internal_error` from missing socket |
| 4b | Degraded: not_live target | `result=not_live` control response |
| 5 | Regression audit | `rg thread_id` approved-exceptions-only check |

### Team-Lead Sign-Off

Sprint D.3 identifier cleanup verified:
- `thread_id` removed from public payload examples in `docs/tui-control-protocol.md`
- `thread_id` annotated as `[MCP-internal adapter only]` in both TUI docs
- `HookEvent.thread_id` doc comment clarifies adapter-only scope
- `docs/thread-id-audit.md` documents all approved exceptions
- Demo script covers all required scenarios including degraded paths

**Signed**: team-lead (ARCH-ATM) — Sprint D.3
