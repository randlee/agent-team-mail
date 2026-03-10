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
| `/quality-management-gh` | Multi-pass QA on GitHub PRs; CI monitoring; findings/final quality reports |
| `/sprint-report` | Generate phase status table or detailed report |
| `/atm-doctor` | Run ATM health diagnostics; escalate critical findings to atm-doctor agent |
| `/named-teammate-launch` | Launch and verify named teammates (Claude/Codex/Gemini) with mailbox polling |

> Additional orchestration guides are in `.claude/skills/*/SKILL.md`. Consult
> the relevant skill before starting a new phase or delegating to a teammate.
