# Documentation Cleanup — 2026-03-12

Branch: `chore/docs-cleanup-and-inventory`

## Summary

Reorganized the `docs/` directory by archiving completed phase planning docs and
test plans, creating a new `reports/` directory for analysis artifacts, and
committing previously untracked utility files.

---

## Archived: Phase Planning Docs → `docs/archive/phases/`

These planning docs are for completed phases and are no longer needed at the top
level of `docs/`.

| File | Destination |
|------|-------------|
| `docs/phase-ac-planning.md` | `docs/archive/phases/` |
| `docs/phase-ad-planning.md` | `docs/archive/phases/` |
| `docs/phase-ae-planning.md` | `docs/archive/phases/` |
| `docs/phase-af-planning.md` | `docs/archive/phases/` |
| `docs/phase-ag-planning.md` | `docs/archive/phases/` |
| `docs/phase-ah-planning.md` | `docs/archive/phases/` |
| `docs/phase-ai-planning.md` | `docs/archive/phases/` |
| `docs/phase-aj-planning.md` | `docs/archive/phases/` |
| `docs/phase-ak-planning.md` | `docs/archive/phases/` |
| `docs/phase-z-summary.md` | `docs/archive/phases/` |
| `docs/phase6-review.md` | `docs/archive/phases/` |
| `docs/phase8-bridge-design.md` | `docs/archive/phases/` |

**Kept active**: `docs/phase-al-planning.md` (current phase, committed as new file)

---

## Archived: Test Plans → `docs/archive/test-plans/`

| File | Destination |
|------|-------------|
| `docs/test-plan-phase-AC.md` | `docs/archive/test-plans/` |
| `docs/test-plan-phase-AD.md` | `docs/archive/test-plans/` |
| `docs/test-plan-phase-AE.md` | `docs/archive/test-plans/` |
| `docs/test-plan-phase-AF.md` | `docs/archive/test-plans/` |
| `docs/test-plan-phase-AG.md` | `docs/archive/test-plans/` |
| `docs/test-plan-phase-AJ.md` | `docs/archive/test-plans/` |
| `docs/test-plan-phase-AK.md` | `docs/archive/test-plans/` |
| `docs/test-plan-phase-T.md` | `docs/archive/test-plans/` |
| `docs/test-plan-phase-U.md` | `docs/archive/test-plans/` |
| `docs/test-plan-phase-V.md` | `docs/archive/test-plans/` |
| `docs/test-plan-phase-W.md` | `docs/archive/test-plans/` |
| `docs/test-plan-phase-X.md` | `docs/archive/test-plans/` |

---

## New: `reports/` Directory — Current Analysis Docs

These files are actively referenced for ongoing work and belong in a dedicated
reports area rather than the general `docs/` directory.

| File | Source |
|------|--------|
| `reports/flaky-test-analysis.md` | `docs/flaky-test-analysis.md` |
| `reports/flaky-test-fix-design.md` | `docs/flaky-test-fix-design.md` |
| `reports/thread-id-audit.md` | `docs/thread-id-audit.md` |
| `reports/test-audit.md` | `docs/test-audit.md` |
| `reports/runtime-path-consistency-audit.md` | `docs/adr/runtime-path-consistency-audit.md` |

---

## New: `reports/archive/` — Completed/Historical Analysis

| File | Source |
|------|--------|
| `reports/archive/highwatermark-fix.md` | `docs/highwatermark-fix.md` |
| `reports/archive/backup-restore-hardening.md` | `docs/backup-restore-hardening.md` |
| `reports/archive/remove-member.md` | `docs/remove-member.md` |
| `reports/archive/issue-539-stale-daemon-fix-plan.md` | `docs/adr/issue-539-stale-daemon-fix-plan.md` |
| `reports/archive/issue-636-registration-gate-fix-plan.md` | `docs/adr/issue-636-registration-gate-fix-plan.md` |
| `reports/archive/issue-669-sigterm-stop-path.md` | `docs/adr/issue-669-sigterm-stop-path.md` |
| `reports/archive/issue-679-logging-spool-merge-root-cause.md` | `docs/adr/issue-679-logging-spool-merge-root-cause.md` |
| `reports/archive/issue-680-ghost-session-reconciliation.md` | `docs/adr/issue-680-ghost-session-reconciliation.md` |
| `reports/archive/issue-681-daemon-restart-stop-path.md` | `docs/adr/issue-681-daemon-restart-stop-path.md` |
| `reports/archive/issue-682-atm-read-message-state.md` | `docs/adr/issue-682-atm-read-message-state.md` |
| `reports/archive/phase-u-u4-daemon-verification.md` | `docs/adr/phase-u-u4-daemon-verification.md` |
| `reports/archive/phase-u-u5-gemini-runtime-verification.md` | `docs/adr/phase-u-u5-gemini-runtime-verification.md` |
| `reports/archive/phase-u-u6-cli-publishability-monitor-verification.md` | `docs/adr/phase-u-u6-cli-publishability-monitor-verification.md` |
| `reports/archive/phase-u-u7-availability-signaling-verification.md` | `docs/adr/phase-u-u7-availability-signaling-verification.md` |
| `reports/archive/phase-u-u8-tui-verification.md` | `docs/adr/phase-u-u8-tui-verification.md` |

---

## Committed: Previously Untracked Files

These files existed in the working tree but had never been committed to the repo.

| File | Purpose |
|------|---------|
| `docs/phase-al-planning.md` | Active phase planning doc (Phase AL) |
| `.claude/agents/arch-qa-agent.md` | Arch QA agent definition |
| `.claude/scripts/delay-run.py` | Delayed script execution utility |
| `.claude/scripts/envelope.py` | ATM message envelope helper |
| `.claude/scripts/worktree_abort.py` | Worktree abort handler |
| `.claude/scripts/worktree_cleanup.py` | Worktree cleanup automation |
| `.claude/scripts/worktree_create.py` | Worktree creation automation |
| `.claude/scripts/worktree_scan.py` | Worktree scan utility |
| `.claude/scripts/worktree_shared.py` | Shared worktree utilities |
| `.claude/scripts/worktree_update.py` | Worktree update automation |
| `.claude/skills/sprint-report/report-detailed.md.j2` | Detailed sprint report Jinja2 template |
| `scripts/rmux` | TOML-driven tmux session launcher |

---

## Gitignored: Tool Config and Build Artifacts

Added to `.gitignore`:

| Entry | Reason |
|-------|--------|
| `.sc/` | Tool configuration (not project files) |
| `.serena/` | Tool configuration (not project files) |
| `release/artifacts/` | Generated release build outputs |

---

## Recommendations for Further Cleanup

1. **`docs/adr/`** — The remaining ADR files (`003-json-mode-transport.md`, `af3-transient-registration-contract.md`) are legitimate architecture decision records and should stay. Consider creating an ADR index if more accumulate.

2. **`docs/archive/`** — Now has two subdirectories (`phases/`, `test-plans/`) plus the pre-existing `project-plan-archive-2026-02-28.md`. Consider moving that file into a `docs/archive/misc/` or leaving it at the top of `docs/archive/`.

3. **`docs/` root** — Still contains several design/spec docs (e.g., `dev-install-design.md`, `spawn-ux.md`, `team-join-ux.md`, `codex-*.md`) that could be organized into subdirectories by topic (e.g., `docs/design/`, `docs/ux/`) in a future cleanup pass.

4. **`reports/` visibility** — Consider adding a brief `reports/README.md` listing what each report covers so future readers can navigate without opening each file.
