---
name: quality-management-gh
version: 1.0.0
description: Reusable QA orchestration skill for GitHub PRs. Use for multi-pass QA, CI monitoring with `atm gh monitor`, one-shot PR reporting with `atm gh pr report`, and template-driven findings/final quality reports.
---

# Quality Management (GitHub)

This skill defines a reusable quality-management workflow for teams that run QA across one or more passes before merge.

## Scope

Use this skill when you need to:
- run QA in multiple passes (`IN-FLIGHT`, `FAIL`, `PASS`),
- monitor CI progression for a PR,
- publish structured findings to PR + ATM,
- publish a final QA closeout report on PASS.

This skill is intentionally generic. Team-specific teammate names, branch policy, and background-agent ownership stay in the team's `quality-mgr` agent prompt.

## Required QA Status Contract

Every QA update (ATM and PR) must include:
- sprint/task identifier
- branch, commit, PR number
- verdict (`PASS | FAIL | IN-FLIGHT`)
- finding counts by severity (`blocking`, `important`, `minor`)
- blocking IDs + concise summaries
- next required action + owner
- merge readiness (`ready | not ready`) + reason

Use fenced JSON for machine-readable status payloads:

```json
{
  "sprint": "AI.4",
  "task": "issue-582",
  "branch": "feature/issue-582-gh-monitor-report-semantics",
  "commit": "abc1234",
  "pr": 586,
  "verdict": "FAIL",
  "findings": {
    "blocking": 1,
    "important": 2,
    "minor": 1
  },
  "blocking_ids": ["QA-001"],
  "next_action": "Fix CI rollup neutral handling",
  "owner": "arch-ctm",
  "merge_readiness": "not ready",
  "merge_reason": "Blocking findings remain"
}
```

## QA Lifecycle (Multi-Pass)

1. Initial pass: usually `FAIL` with findings.
2. Fix passes: `IN-FLIGHT` or `FAIL` while fixes are in progress.
3. Final pass: `PASS` with final quality report and merge recommendation.

Do not treat QA as single-shot.

## CI Monitoring

Use daemon-backed monitoring for CI progression:

1. Ensure plugin configured:
- `atm gh`
- `atm gh status`

2. Start/attach CI monitor for a PR:
- `atm gh monitor pr <PR> --start-timeout 120`

3. Inspect lifecycle/availability during QA:
- `atm gh monitor status`
- `atm gh status pr <PR>`

If monitoring cannot start, include the failure in QA status and proceed with one-shot PR report data.

## One-Shot PR Report Generation

Use:
- `atm gh pr report <PR> --json`

Use report JSON to populate findings/final template fields (checks summary, review decision, merge readiness signals).

## Findings Report to PR

Template:
- `.claude/skills/quality-management-gh/findings-report.md.j2`

Recommended flow:
1. Gather findings from QA agents.
2. Render markdown from template with required variables.
3. Post to PR as comment/update.

Suggested command (stream render directly into PR comment):
- `sc-compose render .claude/skills/quality-management-gh/findings-report.md.j2 --var-file <vars.json> | gh pr comment <PR> --body-file -`

`<vars.json>` must be a flat JSON map (`string -> string`) for `sc-compose`:

```json
{
  "generated_at": "2026-03-09T20:40:00Z",
  "qa_pass": "pass-2",
  "sprint_id": "AI.4",
  "task_id": "issue-582",
  "branch": "feature/issue-582-gh-monitor-report-semantics",
  "commit": "b6d7d23",
  "pr_number": "586",
  "verdict": "FAIL",
  "findings_blocking": "1",
  "findings_important": "2",
  "findings_minor": "1",
  "blocking_ids": "QA-001",
  "blocking_findings_md": "- [QA-001] CI rollup omitted neutral checks (crates/atm/src/commands/gh.rs:1259)",
  "detailed_findings_md": "- [QA-001] Blocking: CI rollup omitted neutral checks\\n- [QA-002] Important: Test fixture missing neutral sample",
  "merge_readiness": "not ready",
  "merge_reason": "Blocking findings remain",
  "next_action": "Patch rollup logic and rerun QA",
  "action_owner": "arch-ctm"
}
```

Use findings template for `FAIL` and `IN-FLIGHT` updates.

## Final Quality Report to PR

Template:
- `.claude/skills/quality-management-gh/quality-report.md.j2`

Recommended flow:
1. Confirm final QA pass and summarize validation scope.
2. Render markdown from template with required variables.
3. Post as final closeout comment/review.

Suggested command (stream render directly into PR comment):
- `sc-compose render .claude/skills/quality-management-gh/quality-report.md.j2 --var-file <vars.json> | gh pr comment <PR> --body-file -`

Use final template only for `PASS` closeout.

## PR Update Conventions

- First QA pass posts detailed findings with `FAIL`.
- Fix-pass updates revise status and open findings.
- Final pass posts `PASS` closeout with residual risk + readiness.

Never overwrite history silently; each update should be clearly timestamped and tied to a pass number.
Rendered reports must include a fenced JSON block (` ```json ... ``` `) for machine parsing.

## ATM Coordination Protocol

For each task:
1. immediate acknowledgement,
2. execute QA work,
3. send completion/status summary,
4. receiver acknowledgement.

No silent processing.
