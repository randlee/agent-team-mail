---
name: team-lead
description: >
  Session initialization for the team-lead identity. Confirms identity and
  detects whether a full team restore is needed. Only run when
  ATM_IDENTITY=team-lead.
---

# Team Lead Skill

**Trigger**: Run at the start of every session where `ATM_IDENTITY=team-lead`.

---

## Step 0 — Confirm Identity

```bash
echo "ATM_IDENTITY=$ATM_IDENTITY"
```

Stop if `ATM_IDENTITY` is not `team-lead`.

> **TODO**: Verify no other active session is already running as `team-lead`
> for this team before proceeding.

---

## Step 1 — Detect Whether Restore Is Needed

Get the current session ID from the `SessionStart` hook output at the top of
context (format: `SESSION_ID=<uuid>`). Compare with `leadSessionId` in the
team config:

```bash
python3 -c "import json; print(json.load(open('/Users/randlee/.claude/teams/atm-dev/config.json'))['leadSessionId'])"
```

- **Match** → team is already initialized for this session. Proceed directly
  to reading `docs/project-plan.md` and outputting project status.
- **Mismatch or config missing** → follow the full restore procedure in
  `.claude/skills/team-lead/backup-and-restore-team.md`.

---

## Team Lead Responsibilities

After initialization, the team-lead uses these skills to coordinate the team:

| Skill | Trigger |
|-------|---------|
| `/phase-orchestration` | Orchestrate a multi-sprint phase (sprint waves, scrum-master lifecycle, integration branch, arch-ctm reviews) |
| `/codex-orchestration` | Run phases where arch-ctm (Codex) is sole dev, with pipelined QA via quality-mgr |
| `/quality-management-gh` | Multi-pass QA on GitHub PRs; CI monitoring; findings/final quality reports. **Simple fixes/small features only** — team-lead runs `atm-qa-agent` + `rust-qa-agent` directly in parallel. For multi-sprint phases use `/phase-orchestration` or `/codex-orchestration` instead. |
| `/sprint-report` | Generate phase status table or detailed report |
| `/atm-doctor` | Run ATM health diagnostics; escalate critical findings to atm-doctor agent |
| `/named-teammate-launch` | Launch and verify named teammates (Claude/Codex/Gemini) with mailbox polling |

> Additional orchestration guides are in `.claude/skills/*/SKILL.md`. Consult
> the relevant skill before starting a new phase or delegating to a teammate.

### Phased Development — MANDATORY

> ⚠️ **For any multi-sprint phased development, `/codex-orchestration` or
> `/phase-orchestration` MUST be used as directed by the user. Using ad-hoc
> coordination instead of these skills leads to process drift, missed
> communications, and inconsistent QA gates.**

**After every session start or context compaction**, if a phase is in progress:

1. Identify which **one** skill governs the active phase — either
   `/codex-orchestration` or `/phase-orchestration`. **Read only that one.**
2. If unsure which applies, **ask the user immediately** and read the correct
   skill before taking any coordination action.
3. Resume execution from the last documented state — do not rely on memory
   alone.

> Do not read both skills. Do not guess. If unsure — ask first, read immediately.
>
> Skipping this re-read is the primary cause of process drift between sessions.

---

## Task Assignment Protocol

When assigning work to any teammate:

1. **Create or update the task list** — `TaskCreate` or `TaskUpdate` with assignee and description before sending the first message.
2. **Include in the assignment message**:
   - The task and its scope (link to worktree, relevant issues, design docs)
   - Applicable development guidelines (`docs/cross-platform-guidelines.md`, Rust guidelines, etc.)
   - Expected deliverables and acceptance criteria
3. **Render all task messages via `sc-compose`** from a Jinja2 template (see `/codex-orchestration` skill). Never hand-write prose for task assignments.

### Template Rendering with sc-compose

All dev and QA assignments MUST be rendered via `sc-compose` before sending:

```bash
# Dev assignment
sc-compose render .claude/skills/codex-orchestration/dev-template.xml.j2 \
  --var-file vars.json

# QA assignment
sc-compose render .claude/skills/codex-orchestration/qa-template.xml.j2 \
  --var-file vars.json
```

Example `vars.json` for a dev assignment:

```json
{
  "task_id": "SC-EXAMPLE-1",
  "sprint": "S10-EXAMPLE",
  "assignee": "arch-ctm",
  "description": "Short description of the task.",
  "worktree_path": "/Users/randlee/Documents/github/schook-worktrees/feature-example",
  "branch": "feature/example",
  "pr_target": "integrate/phase-x",
  "deliverables": "- Deliverable one\n- Deliverable two",
  "acceptance_criteria": "- cargo test --workspace PASS\n- cargo clippy -- -D warnings PASS",
  "references": "- docs/requirements.md\n- docs/architecture.md"
}
```

Rendered output is sent directly as the task message body. Required fields are
defined in the frontmatter of each template. Scalar strings (not JSON arrays)
must be used for list fields — see the `/codex-orchestration` skill for the
full render contract and tested examples.

### Communication Rules

- **No ACK = work is not being done.** If a teammate does not acknowledge within a reasonable
  window, assume the message was not received and follow up (nudge via tmux for Codex agents).
- **Codex agents (arch-ctm, arch-ctask)** do not receive message injection — they only see
  new messages when they check mail after their current task completes. Do not assume they
  received a message until they ACK.

---

## PR and CI Protocol

- **Create the PR as soon as dev completes work and begins self-testing** — before QA starts,
  so CI runs in parallel with the QA review.
- **Immediately after PR creation**, run:
  ```bash
  atm gh monitor pr <NUMBER>
  ```
  to receive CI notifications automatically. Do not wait for the user to ask.
