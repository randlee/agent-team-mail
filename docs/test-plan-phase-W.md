# Phase W: Release Workflow Automation + ATM Bug Fixes

**Goal**: (1) Eliminate manual steps from the release pipeline that caused the v0.28.0 disaster. (2) Fix ATM messaging bugs surfaced during dogfooding. (3) Harden the publisher agent to eliminate sub-agent sprawl.

**Integration branch**: `integrate/phase-W` off `develop`.

**Background**: The v0.28.0 release required manual intervention at every stage — merge conflicts, Homebrew update, GitHub Release creation (due to crates.io 403), and manual publisher.md sub-agent spawning that created named teammate sprawl. This phase automates the gaps.

---

## Part 1: ATM Bug Fixes (2 sprints)

### V.1 — Fix atm send offline action prefix (#328, #329)

**Issues addressed**: `atm send` silently prepends `[PENDING ACTION - execute when online]` to all offline messages; skill doc reinforces the pattern.

**Root causes**:
- `crates/atm/src/commands/send.rs:419` — `resolve_offline_action()` returns a hardcoded default string. Silent delivery is the correct default.
- `docs/agent-teams-mail-skill.md` — includes guidance that encourages use of the `[PENDING ACTION]` prefix pattern

**Deliverables**:
1. `send.rs:419`: change `"PENDING ACTION - execute when online".to_string()` → `String::new()`. Opt-in path already exists (`--offline-action` flag / config).
2. `docs/agent-teams-mail-skill.md`: remove or rewrite section that references `[PENDING ACTION]` tag pattern
3. Tests: verify `resolve_offline_action()` returns empty string by default; verify no prefix on offline send without explicit `--offline-action`

**Acceptance criteria**:
- `atm send <offline-agent> "message"` delivers message without prefix by default
- `atm send <offline-agent> --offline-action "URGENT" "message"` still prepends the custom action
- `docs/agent-teams-mail-skill.md` does not reference `[PENDING ACTION]`

---

### V.2 — Publisher agent: eliminate sub-agent spawning (#327)

**Issue addressed**: Publisher spawns named teammates (`pr-poll`, `ci-poll`, `release-audit`, etc.) causing pane exhaustion and gate hook violations.

**Root cause**:
- `publisher.md` step 3 instructs "spawn background audit agent" — publisher.md was written before the gate hook existed
- Publisher should trigger `gh workflow run release.yml` and poll `gh run watch` directly, not spawn sub-agents
- The `sc-delay-tasks:git-pr-check-delay` skill auto-creates named teammates — publisher must not use this skill

**Deliverables**:
1. Rewrite `.claude/agents/publisher.md`:
   - Remove all instructions to spawn sub-agents or use delay-tasks skill
   - Replace CI polling with inline `gh run watch` / `gh pr checks --watch` calls
   - Replace release audit with direct `gh release view` + `cargo install --dry-run` verification
   - All work done inline in publisher's own context, no sub-agents
2. Add note to publisher.md: "DO NOT use sc-delay-tasks skill — it creates named teammates. Use gh run watch or sleep loops instead."
3. Test: verify publisher can complete a dry-run release check without spawning any agents

**Acceptance criteria**:
- Publisher completes without spawning any named or background teammates
- All CI polling done inline (gh run watch / gh pr checks --watch)
- Gate hook never fires during publisher execution

---

## Part 2: Release Workflow Automation (2 sprints)

### V.3 — Release workflow: crates.io retry + Homebrew automation (#323, #324)

**Issues addressed**: post-publish-verify fails with 403 from crates.io API; Homebrew formula must be updated manually.

**Root causes**:
- `post-publish-verify` job uses bare `curl` with no retry logic against crates.io CDN (bot protection returns 403 on CI runners)
- No GitHub Actions job exists to update the Homebrew tap formula; requires manual SSH clone + edit + push

**Deliverables**:

1. **crates.io 403 fix** (`.github/workflows/release.yml`):
   - Replace bare `curl` in `post-publish-verify` with retry loop (max 10 attempts, 30s sleep between)
   - Use `cargo install <crate> --version <ver>` as definitive verification (already retries internally)
   - Keep `curl` as secondary check with `--retry 5 --retry-delay 30` flags

2. **Homebrew automation** (`.github/workflows/release.yml`):
   - Add `update-homebrew` job after `release` job succeeds
   - Uses GitHub Actions to clone `randlee/homebrew-tap`, update both formulas (`agent-team-mail.rb` and `atm.rb`) with new version + SHA256
   - Requires `HOMEBREW_TAP_TOKEN` secret (PAT with repo write access to homebrew-tap)
   - Computes SHA256 from the release tarball URL pattern

3. Tests: CI job dry-run validation

**Acceptance criteria**:
- `post-publish-verify` succeeds on crates.io CDN even with transient 403s (retries)
- After release: Homebrew tap updated automatically within the same workflow run
- No manual Homebrew steps required

---

### V.4 — Release workflow: pre-publish audit + cross-channel verification (#325, #326)

**Issues addressed**: No pre-publish audit gate (bad packages could publish); no consolidated completion report.

**Root causes**:
- Release workflow publishes immediately without validating crate packaging (`cargo package --locked`) first
- No waiver gate: if a known crate is intentionally excluded, the workflow has no mechanism to require explicit approval
- After release completes, no single report confirms all channels (GitHub Release, crates.io ×5, Homebrew) are live

**Deliverables**:

1. **Pre-publish audit job** (`.github/workflows/release.yml`):
   - Add `pre-publish-audit` job before `publish-crates`
   - Runs `cargo package -p <crate> --locked` for each crate — fails fast on any packaging error
   - Validates `release-inventory.json` version matches `Cargo.toml` workspace version
   - Checks that all expected crates are present in inventory (no silent omissions)

2. **Waiver gate enforcement**:
   - `release-inventory.json` can mark a crate with `"publish": false` + `"waiver_reason": "..."`
   - Pre-publish audit job reads inventory and requires waiver_reason for any skipped crate
   - Without waiver_reason, skipped crates fail the job

3. **Consolidated completion report** (`.github/workflows/release.yml`):
   - Add `release-summary` job that runs after all publish/verify jobs
   - Queries: GitHub Release exists, all 5 crates at correct version on crates.io, Homebrew formula updated
   - Writes summary to GitHub Actions job summary (visible in PR/workflow UI)
   - Posts comment on release PR (if applicable) with ✅/❌ per channel

4. Tests: workflow validation with mock/dry-run

**Acceptance criteria**:
- `cargo package` errors block publishing (no partial releases)
- Waiver-free skipped crates fail the workflow
- After release: single summary shows all channel statuses in workflow UI

---

## Part 3: Deferred / Backlog

| Item | Source | Priority | Notes |
|------|--------|----------|-------|
| Flaky daemon integration tests (`#[serial]` missing) | Moved to Phase X | Medium | Phase X `X.2` / issue #337 tracks this explicitly |
| `acquire_lock` max_retries 3→8 in daemon_client.rs | Observed flakiness | Medium | Races during fast CI test runs |
| Doctor regression: `LEAD_SESSION_RECOVERY_REQUIRED` shown when lead is functional | Post-Phase-U finding | High | After daemon restart, doctor shows team-lead offline even when messaging works. Phase U U.2 hardening may not cover this path. Needs investigation. |
| `atm teams add-member` does not create inbox file | Moved to Phase X | High | Phase X `X.1` / issue #338 tracks atomic add-member inbox creation |
| Phase V release hygiene: cherry-pick gate hook fix to develop if not already present | Cleanup | High | Commit 0863bd5 must be on develop |

---

## Sprint Summary

| Sprint | Description | Issues | Type | Depends On |
|--------|-------------|--------|------|------------|
| W.1 | ATM send offline action + skill doc fix | #328, #329 | Fix | — |
| W.2 | Publisher agent rewrite (no sub-agents) | #327 | Fix/Docs | — |
| W.3 | Release workflow: crates.io retry + Homebrew automation | #323, #324 | CI/CD | — |
| W.4 | Release workflow: pre-publish audit + completion report | #325, #326 | CI/CD | W.3 |

**Parallel tracks**: V.1 and V.2 can run in parallel with each other and with V.3. V.4 depends on V.3 (extends the same workflow file).

**Version**: Phase W targets v0.29.0.

---

## Key Lessons from Phase U / v0.28.0 Release

1. **Named teammate sprawl**: Publisher spawning sub-agents = pane exhaustion. Gate hook now blocks this, but publisher.md must be rewritten to not try.
2. **Homebrew is not automated**: Every release requires manual SSH+edit+push to homebrew-tap. W.3 closes this.
3. **crates.io 403s on CI**: CDN bot protection causes transient failures. Retry logic is the fix.
4. **Merge conflicts**: `Cargo.lock` stale entries, hardcoded version pins in `atm-tui/Cargo.toml`. Pre-publish audit catches version mismatches before they block releases.
5. **State-model separation must be explicit**: mailbox delivery can still work while daemon liveness is stale. Treat this as a state reconciliation bug, not expected behavior; `isActive` must remain activity-only and daemon session state must remain liveness truth.
