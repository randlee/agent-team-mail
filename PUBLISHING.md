# Publishing Guide

This repo uses a single source of truth for release artifacts:

- Manifest: `release/publish-artifacts.toml`
- Loader/generator: `scripts/release_artifacts.py`

Do not hardcode crate lists, publish order, or release binary lists in docs or
workflows. Update the manifest instead.

## Distribution Channels

- GitHub Releases: <https://github.com/randlee/agent-team-mail/releases>
- crates.io artifacts from manifest (`publish = true`)
- Homebrew tap formulas: `Formula/agent-team-mail.rb`, `Formula/atm.rb` in
  <https://github.com/randlee/homebrew-tap>

## Workflows

- Preflight: `.github/workflows/release-preflight.yml`
- Release: `.github/workflows/release.yml`

Both workflows are manual dispatch workflows.

## Standard Flow

1. Ensure `develop` contains the release version bump and is ready for merge.
2. Run preflight workflow with:
   - `version=<X.Y.Z or vX.Y.Z>`
   - `run_by_agent=publisher`
3. Preflight fails if any publishable artifact in
   `release/publish-artifacts.toml` is already published at that version.
4. Merge `develop` to `main` once CI and preflight are green.
5. Run release workflow with `version=<X.Y.Z or vX.Y.Z>`.
6. Release workflow gates, tags, builds archives, publishes crates from the
   manifest in manifest order, verifies publish outcomes, creates GitHub
   release, and updates Homebrew formulas.

## Manifest-Driven Behavior

`release/publish-artifacts.toml` defines:

- Crate artifact identity and package name
- Crate Cargo.toml path
- Required/publish flags
- Publish order
- Preflight check mode (`full` or `locked`)
- Post-publish propagation wait seconds
- Whether post-publish `cargo install` verification is required
- Release binary list for archive packaging

Mandatory order rule:
- `sc-observability` must publish before `agent-team-mail-core` because core and
  downstream tools consume the shared observability contract.
- Enforce this only via manifest order; do not hardcode order in workflow YAML.

## Local Validation Commands

```bash
# Show publish plan (package|wait_seconds)
python3 scripts/release_artifacts.py list-publish-plan \
  --manifest release/publish-artifacts.toml

# Show release binaries that will be archived
python3 scripts/release_artifacts.py list-release-binaries \
  --manifest release/publish-artifacts.toml

# Generate inventory JSON from manifest
python3 scripts/release_artifacts.py emit-inventory \
  --manifest release/publish-artifacts.toml \
  --version 0.41.0 \
  --tag v0.41.0 \
  --commit "$(git rev-parse HEAD)" \
  --source-ref refs/heads/develop \
  --output release/release-inventory.json

# Preflight guard: fail if version already exists on crates.io
python3 scripts/release_artifacts.py check-version-unpublished \
  --manifest release/publish-artifacts.toml \
  --version 0.41.0
```

## Updating Release Artifacts

When adding/removing/reordering release crates or binaries:

1. Update `release/publish-artifacts.toml`.
2. Run:
   - `python3 scripts/release_artifacts.py list-artifacts --manifest release/publish-artifacts.toml`
   - `python3 scripts/release_artifacts.py list-release-binaries --manifest release/publish-artifacts.toml`
3. Run CI/Preflight to validate the change.

No workflow edits are required for normal artifact-list changes when the
manifest is kept current.
