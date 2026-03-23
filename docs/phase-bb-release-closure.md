# Phase BB Release Closure

Phase BB ships the daemon in a constrained release posture:

- single shared daemon only for normal product use
- no ambiguous daemon-backed fallback path for authoritative state surfaces
- file-based mail commands remain file-based and do not depend on the daemon

Path ownership in this release is split intentionally:

- config-root-owned paths resolve from the config root (`ATM_CONFIG_HOME` / OS config home)
- runtime-owned paths resolve from the active runtime home (`ATM_HOME`)

Applied to shipped surfaces:

- daemon plugin config/inbox resolution uses plugin context config-root data
- daemon plugin runtime state paths use plugin context runtime-home data
- `atm-tui` config reads use config-root resolution
- `atm-tui` log, spool, watch-stream, and replay checkpoint paths use runtime-home resolution

Deferred beyond Phase BB:

- daemon redesign / replacement
- plugin extraction into separate repos or services
- broader daemon scope reduction beyond release-risk fixes
