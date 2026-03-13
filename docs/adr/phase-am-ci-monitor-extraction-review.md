# Phase AM CI Monitor Extraction Review

## Status

Traceable review artifact for the Phase AM completion review that informed
Phase AN planning.

## Summary

Phase AM successfully thinned `socket.rs` and moved major CI-monitor handling
out of the daemon monolith, but it did not finish the extraction boundary.
The subsystem is easier to reason about now, yet still not ready to become a
standalone crate without another cleanup phase.

## Key Findings

1. `types.rs` is close to extractable.
   Shared CI-monitor domain types are mostly clean and near a reusable crate
   boundary.

2. `provider.rs` and `registry.rs` still leak daemon/plugin concerns.
   Their public surfaces still depend on daemon-oriented error and plumbing
   shapes that should not cross a future crate boundary.

3. `service.rs` is not yet a pure CI-monitor service layer.
   It still speaks daemon-client request/response types, which makes it a poor
   crate boundary.

4. `plugin.rs` is the main extraction blocker.
   It mixes plugin lifecycle, config/repo resolution, provider loading,
   polling/task wiring, alert routing, and health/state persistence.

5. `gh_monitor_router.rs` is a daemon transport adapter.
   It is better separated from `socket.rs`, but it is not reusable CI-monitor
   core.

6. `mod.rs` still exposes test-oriented helpers too broadly.
   That makes the production API look cleaner in tests than it really is.

## Extraction Readiness

- `types.rs`: high
- `provider.rs` / `registry.rs`: moderate
- `service.rs`: low to moderate
- `plugin.rs`: low
- `gh_monitor_router.rs`: low as a crate candidate, acceptable as a daemon adapter

## Recommended Follow-up

Phase AN should proceed in this order:

1. cleanup gates for init error propagation and test-surface narrowing
2. domain types boundary cleanup
3. service split away from daemon-only wiring
4. trait injection for provider/registry dependencies
5. transport adapter hardening
6. crate extraction only after the earlier boundaries are stable
