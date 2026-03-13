# thread_id Identifier Audit

**Date**: 2026-02-20
**Sprint**: D.3 — Identifier Cleanup
**Status**: COMPLETE

## Summary

`thread_id` is a Codex-internal backend concept (a Codex conversation handle). It is NOT a public identifier in the ATM TUI/control protocol. The public identifiers for TUI routing are `session_id` and `agent_id`.

## Audit Command

To verify only approved MCP-internal exceptions remain:

```bash
rg "thread_id|threadId" docs/tui-*.md crates/atm/src crates/atm-daemon/src
```

## Expected Output (Approved Exceptions Only)

After Sprint D.3, the only remaining occurrences must be:

| File | Location | Classification | Rationale |
|------|----------|----------------|-----------|
| `docs/tui-mvp-architecture.md` | Section 9 | MCP-internal note | Documented as adapter-only; not a public TUI identifier |
| `docs/tui-control-protocol.md` | Section 2, 3.1, 3.3 | MCP-internal note | Marked `[MCP-internal adapter only]`; not required by TUI |
| `crates/atm-daemon/src/plugins/worker_adapter/hook_watcher.rs` | `HookEvent.thread_id` field | Codex adapter | Parses `thread-id` from Codex hook relay events; not exposed in public API |

## Non-Approved Usage (Must Be Zero)

- Any `thread_id` in payload examples without MCP-internal annotation
- Any `thread_id` in public control API structs (non-adapter code)
- Any `thread_id` in `crates/atm/src` (CLI) or `crates/atm-core/src` (library)
