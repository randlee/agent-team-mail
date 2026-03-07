# Phase AD Planning: Cross-Platform Script Standardization

## Goal

Eliminate product/runtime dependence on shell scripts (`bash`/`pwsh`) and
standardize ATM runtime scripting on Python for cross-platform behavior.

## Requirements Lock (Phase AD Scope)

1. Product/runtime script paths are Python-only.
2. Shell scripts are dev/CI-only exceptions unless explicitly approved.
3. `atm init` auto-installs runtime hook/config wiring for all detected runtimes:
   Claude Code, Codex CLI, Gemini CLI.
4. Runtime install behavior is per-runtime idempotent and reports status per runtime.
5. Hook/script behavior is covered by pytest and included in required CI checks.

## Violation Inventory (Input to AD Sprints)

### Must Remediate

1. `.claude/settings.json` bash wrapper commands (`bash -c`) in hook wiring paths.
2. `scripts/atm-hook-relay.sh` (Codex relay) shell runtime dependency.
3. `scripts/spawn-teammate.sh` shell launcher dependency.
4. `scripts/launch-worker.sh` shell launcher dependency.

### Review / Absorb

1. `scripts/setup-codex-hooks.sh` should be absorbed into `atm init` runtime install behavior.
2. `.github/workflows/*.yml` shell steps are CI-only and treated as dev exceptions.

## AD.1 Candidate Sprint (Potential Sprint #1)

### Objective

Lock and implement Python-only runtime policy with first-pass runtime auto-install
through `atm init`.

### Deliverables

1. Requirements updates merged:
   - Python-only product script policy.
   - `atm init` runtime auto-install behavior for Claude/Codex/Gemini.
   - CI pytest coverage requirement for runtime scripts/hooks.
2. Design-level conversion plan for the four known runtime shell violations.
3. `atm init` runtime detection contract:
   - runtime present/absent behavior
   - per-runtime status reporting
   - idempotency contract
4. Test plan additions for:
   - runtime detection matrix
   - per-runtime idempotent re-run behavior
   - pytest hook/script coverage in CI lane

### Acceptance Criteria

1. Requirements explicitly prohibit shell as a product runtime dependency.
2. `atm init` runtime auto-install contract is fully specified for Claude, Codex, Gemini.
3. AD violation list is complete and mapped to follow-on implementation sprints.
4. Test plan identifies deterministic coverage for runtime detection/install semantics.
5. CI requirement explicitly includes pytest hook/script coverage for affected behavior.

## Follow-On Sprint Candidates (After AD.1)

1. AD.2: Gemini hook install via `atm init` runtime detection + auto-install.
2. AD.3: Convert `scripts/atm-hook-relay.sh` to Python and wire into runtime flow.
3. AD.4: Convert `scripts/spawn-teammate.sh` and `scripts/launch-worker.sh` to Python.
4. AD.5: Fold `setup-codex-hooks.sh` behavior into `atm init` runtime install path.
5. AD.6: Remove remaining product-facing bash wrappers from installed hook paths.

## AD.2 Candidate Sprint (Gemini Hook Install)

### Objective

Make Gemini runtime hook install first-class in `atm init` with deterministic
cross-platform behavior and CI test coverage.

### Deliverables

1. Define Gemini runtime detection contract in `atm init`:
   - detection criteria for installed Gemini CLI
   - installed/not-installed per-runtime status output
   - fail-open behavior when Gemini is absent
2. Define Gemini hook/config install contract:
   - configuration file target(s)
   - idempotent create/update semantics
   - no duplicate hook/config entries on re-run
3. Define Gemini verification/test matrix:
   - first install in clean environment
   - re-run idempotency
   - mixed-runtime matrix (Claude only, Gemini only, Claude+Gemini, none)
4. Add pytest + CI test requirements for Gemini install behavior in existing
   hook/script test lanes.

### Acceptance Criteria

1. `atm init` behavior for Gemini installed vs not-installed is explicitly specified.
2. Gemini install path is idempotent and reports per-runtime result clearly.
3. Gemini install tests are defined in pytest-backed CI coverage.
4. AD sprint dependencies remain incremental (AD.2 can proceed after AD.1 lock).
