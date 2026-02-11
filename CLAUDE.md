# Claude Instructions for agent-team-mail

## ⚠️ CRITICAL: Branch Management Rules

**NEVER switch the main repository branch on disk from `develop`.**

- Main repo at `/Users/randlee/Documents/github/agent-team-mail/` MUST remain on `develop` at all times
- **ALWAYS use `sc-git-worktree` skill** to create worktrees for all development work
- **ALWAYS create worktrees FROM `develop` branch** (not from `main`)
- Do NOT use `git checkout` or `git switch` in the main repository
- All sprint work happens in worktrees at `../agent-team-mail-worktrees/<branch-name>`
- **All PRs target `develop` branch** (integration branch, not `main`)

**Why**: Switching branches in the main repo breaks worktree references and destabilizes the development environment.

**Worktree Creation Pattern**:
```bash
# ✅ CORRECT: Create worktree from develop
/sc-git-worktree --create feature/1-2a-work-bead develop

# ❌ WRONG: Creating from main
/sc-git-worktree --create feature/1-2a-work-bead main
```

---

## Project Overview

**agent-team-mail** is a CLI for interfacing with Claude agents via email with plugin-based expansion:
- Routes email requests to appropriate Claude agents
- Supports multiple agent types for specialized task handling
- Extensible plugin architecture for adding new agent integrations
- Multi-agent coordination for complex workflows
- Email-driven development and management interface

**Goal**: Build a flexible, extensible agent-mail CLI system with plugin-based expansion.

---

## Implementation Plan

**Current Plan**: [`pm/2026-02-08-implementation-plan.md`](./pm/2026-02-08-implementation-plan.md)

- 6 phases, 26 sprints
- Aggressive parallelization with worktree isolation
- CI/CD integration (Python tests in Phase 1, Go tests in Phase 4)
- Target: MVP capable of building post-MVP features

**Current Status**: Phase 1 in progress - 3 of 26 sprints complete
- ✅ Sprint 1.1: Core Schema Validation Script (PR #4 merged)
- ✅ Sprint 1.2a: Example Work Bead (PR #8 pending review)
- ✅ Sprint 1.2b: Example Merge Bead (PR #9 pending review)
- **Next**: Sprint 1.3 - Integration & Documentation

---

## Key Documentation

**For reference only - read when information is needed:**

- [`docs/README.md`](./docs/README.md) - System overview and quick reference
- [`docs/schema.md`](./docs/schema.md) - Extended bead schema with validation rules
- [`docs/numbering.md`](./docs/numbering.md) - Phase/sprint numbering scheme
- [`docs/architecture.md`](./docs/architecture.md) - System architecture and data flow
- [`docs/corner-cases.md`](./docs/corner-cases.md) - Failure scenarios and mitigations
- [`docs/DESIGN-SUMMARY.md`](./docs/DESIGN-SUMMARY.md) - High-level implementation roadmap

---

## Workflow

### Sprint Execution Pattern (Dev-QA Loop)

Every sprint follows this pattern:

1. **Create worktree** using `sc-git-worktree` skill
2. **Dev work** by assigned dev agent(s)
3. **QA validation** by assigned QA agent(s)
4. **Retry loop** if QA fails (max attempts configurable)
5. **Commit/Push/PR** to `develop` branch
6. **Agent-teams review** documenting what worked/didn't

### Integration Branch

- All PRs target `develop` branch
- Merge to `main` after review/approval
- Post-merge CI runs as safety net

---

## Agent Model Selection

- **Haiku** - Exploration, test execution, simple validation
- **Sonnet** - Implementation work, documentation writing
- **Opus** - Critical planning, architecture decisions, complex review

---

## Environment

**Task List**: `agent-team-mail` (configured in `.env`)
**Agent Teams**: Enabled (experimental feature)

---

## Initialization Process
- Read Implementation Plan
- Quick verification plan is up to date (i.e. commits/worktrees exist for next step, plan is not up to date)
- Output concise project summary and status to user
- Output a detailed plan for the next sprint using background parallel agents on dedicated sc-git-worktree(s)
- Be prepared to begin the next sprint upon the users approval