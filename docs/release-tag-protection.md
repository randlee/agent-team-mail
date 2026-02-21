# Release Tag Protection Policy

This policy prevents accidental or premature `v*` tags.

## Goal
Ensure release tags are created only after `develop` is merged into `main` and release gates pass.

## Required GitHub Ruleset
Create a **tag ruleset** for pattern:
- `v*`

Recommended settings:
1. Restrict tag creation to trusted actors only.
2. Deny deletion of release tags by default.
3. Deny force-update of existing release tags.

Recommended actor model:
- Allow: repository admins/maintainers and release automation actor.
- Block: all other contributors and bots.

## Operational Contract
- Human release flow uses `workflow_dispatch` in `.github/workflows/release.yml`.
- Workflow runs `scripts/release_gate.sh` before creating any tag.
- Workflow creates tag from `origin/main` (not local branch HEAD).

## Audit Checks
Before and after each release, validate:
```bash
git fetch origin --prune --tags
git log --oneline origin/main..origin/develop
git rev-parse vX.Y.Z
git rev-parse origin/main
```
Expected:
- `origin/main..origin/develop` is empty.
- `vX.Y.Z` points to intended release commit on `origin/main`.

## Incident Response
If an incorrect tag is pushed:
1. Stop publication immediately.
2. Record impacted channels (GitHub release, crates.io, Homebrew).
3. Notify `team-lead` with commit and tag mismatch details.
4. Execute correction plan approved by `team-lead`.
