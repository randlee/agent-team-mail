# Publishing Procedures

This document is the source of truth for release publishing workflow in `agent-team-mail`.

## Scope

Applies when promoting integrated code to release and shipping packages.

## Release Targets

Publishing is considered complete only when all configured targets are handled:
- GitHub release (tag + release notes/artifacts as applicable)
- crates.io (`atm` package publication flow)
- Homebrew update (tap/formula update and verification)

## Standard Release Flow

1. Sync `develop` to latest remote state.
2. Create release branch from `develop`:
   - `release/vX.Y.Z`
3. Apply release updates on release branch:
   - Version bump (minor by default unless explicitly overridden)
   - Release notes/changelog updates required by repository conventions
4. Run validation gates on release branch:
   - Build, test, clippy, and any required release checks
5. Publish from release branch only (never directly from `develop` or `main`).
   - Complete crates.io publish steps.
   - Complete Homebrew tap/formula update steps.
   - Complete GitHub release/tagging steps.
6. Open PRs:
   - `release/vX.Y.Z -> main`
   - `release/vX.Y.Z -> develop` (back-merge to preserve release commits on develop)
7. Wait for CI and approvals, then merge per team-lead/user policy.

## Required Outputs

Release orchestration must report:
- release branch name
- version change
- validation results
- publish status
- PR URLs for `main` and `develop`
- blockers and required follow-up actions
