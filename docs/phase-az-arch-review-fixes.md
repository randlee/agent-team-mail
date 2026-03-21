## AZ Arch Review Fixes

### Finding 1: shared-dev smoke must assert ATM canonical log advancement

The current `scripts/otel-dev-install-smoke.py` logic correctly resolves the
shared dev `ATM_HOME`, but it weakens the smoke in shared-dev mode by skipping
the `atm` canonical log and `.otel.jsonl` mirror existence/count-advance checks.
I will keep the shared-dev path resolution and canonical path discovery, but
remove the `if not live_shared_dev` / `if not outage_shared_dev` guards around
the ATM assertions so both live and outage flows always prove the `atm` sink
advanced at the resolved canonical location.

### Finding 2: replace the ignored concurrent-start test with a deterministic guard test

The ignored `test_ensure_daemon_running_serializes_concurrent_start` currently
depends on a real spawned fake daemon and timing-sensitive socket convergence.
I will factor the startup lock acquisition in `ensure_daemon_running_unix()`
into a small helper that returns the held process/file-lock guards. The new
test will exercise that helper directly with two threads and channels,
asserting that the second thread cannot acquire the startup guards until the
first releases them. That preserves the actual serialization contract without
needing a fake daemon process or scheduler-sensitive timing.
