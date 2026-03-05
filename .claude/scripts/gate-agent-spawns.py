#!/usr/bin/env python3
"""PreToolUse hook that enforces safe agent spawning patterns for orchestrators.

## Why This Exists

The scrum-master agent acts as an ORCHESTRATOR — it coordinates sprints by
spawning dev and QA sub-agents to do the actual work. Without this gate,
orchestrators can accidentally spawn agents incorrectly, leading to:

1. Resource exhaustion: Each named teammate = tmux pane. 3 scrum-masters each
   spawning 2 named teammates = 9 panes. With background agents = 3 panes.

2. Lifecycle issues: Background agents without names can't compact and die at
   context limit. Orchestrators need full teammate status to survive long sprints.

This gate enforces three rules:
- Rule 1: Agents declaring `metadata.spawn_policy: named_teammate_required`
  MUST be named teammates
- Rule 2: Only the team LEAD can create named teammates (not orchestrators themselves)
- Rule 3: team_name must match .atm.toml [core].default_team when provided

## Mode Compatibility

Works with both in-process and tmux teammates because it uses PreToolUse hooks
in settings.json (fires for ALL Task calls) and session_id differentiation
(present in both modes). See Reddit post for mode differences:
https://www.reddit.com/r/ClaudeCode/comments/1qzypcs/playing_around_with_the_new_agent_teams_experiment/

NOTE: Agent-teams is pre-release as of 2/11/2026. Verified on Claude Code v2.1.39+.

Exit codes: 0 = Allow, 2 = Block
"""

import json
import os
import re
import sys
import tempfile
from pathlib import Path

from atm_hook_lib import read_atm_toml

SPAWN_POLICY_NAMED_REQUIRED = "named_teammate_required"
SPAWN_POLICY_LEADERS_ONLY = "leaders-only"
SPAWN_POLICY_ANY_MEMBER = "any-member"
SPAWN_UNAUTHORIZED = "SPAWN_UNAUTHORIZED"

DEBUG_LOG = Path(tempfile.gettempdir()) / "gate-agent-spawns-debug.jsonl"
SESSION_ID_FILE = Path(tempfile.gettempdir()) / "atm-session-id"


def load_team_config(team_name: str) -> dict | None:
    """Load ~/.claude/teams/<team>/config.json when available."""
    if not team_name or not team_name.strip():
        return None

    config_path = Path.home() / ".claude" / "teams" / team_name / "config.json"
    if not config_path.exists():
        return None

    try:
        config = json.loads(config_path.read_text())
        if isinstance(config, dict):
            return config
        return None
    except Exception:
        return None


def load_spawn_policy_from_toml() -> tuple[str | None, str, list[str]]:
    """Return (required_team, spawn_policy, co_leaders) from .atm.toml.

    Defaults when keys are absent:
    - spawn_policy: leaders-only
    - co_leaders: []
    """
    config = read_atm_toml() or {}
    core = config.get("core", {}) if isinstance(config, dict) else {}
    required_team = core.get("default_team") if isinstance(core, dict) else None

    spawn_policy = SPAWN_POLICY_LEADERS_ONLY
    co_leaders: list[str] = []
    if isinstance(required_team, str) and required_team.strip():
        team_cfg = config.get("team", {}) if isinstance(config, dict) else {}
        team_entry = team_cfg.get(required_team, {}) if isinstance(team_cfg, dict) else {}
        if isinstance(team_entry, dict):
            raw_policy = str(team_entry.get("spawn_policy", SPAWN_POLICY_LEADERS_ONLY)).strip()
            if raw_policy in {SPAWN_POLICY_LEADERS_ONLY, SPAWN_POLICY_ANY_MEMBER}:
                spawn_policy = raw_policy
            raw_co_leaders = team_entry.get("co_leaders", [])
            if isinstance(raw_co_leaders, list):
                co_leaders = [
                    str(item).strip()
                    for item in raw_co_leaders
                    if isinstance(item, str) and str(item).strip()
                ]
    return required_team, spawn_policy, co_leaders


def resolve_caller_identity(
    session_id: str, team_name: str | None, env_identity: str | None
) -> str | None:
    """Resolve caller identity for spawn policy checks.

    Priority:
    1. ATM_IDENTITY env override
    2. Team config lookup by session_id (leadSessionId or member.sessionId)
    """
    if isinstance(env_identity, str) and env_identity.strip():
        return env_identity.strip()

    team = (team_name or "").strip()
    if not team or not session_id:
        return None
    config = load_team_config(team)
    if not config:
        return None

    if config.get("leadSessionId") == session_id:
        return "team-lead"

    members = config.get("members", [])
    if isinstance(members, list):
        for member in members:
            if not isinstance(member, dict):
                continue
            if member.get("sessionId") == session_id:
                name = member.get("name")
                if isinstance(name, str) and name.strip():
                    return name.strip()
    return None


def _extract_frontmatter(text: str) -> str | None:
    """Return YAML frontmatter body between leading --- markers, if present."""
    # Require frontmatter at the start of the file.
    if not text.startswith("---\n"):
        return None
    end = text.find("\n---", 4)
    if end == -1:
        return None
    return text[4:end]


def _agent_file_for(subagent_type: str) -> Path | None:
    if not subagent_type or not str(subagent_type).strip():
        return None
    project_dir = os.environ.get("CLAUDE_PROJECT_DIR")
    base = Path(project_dir) if project_dir else Path(".")
    return base / ".claude" / "agents" / f"{subagent_type}.md"


def _frontmatter_requires_named_teammate(frontmatter: str) -> bool:
    """Best-effort parse for metadata spawn policy without YAML deps.

    Supported keys:
    - metadata.spawn_policy: named_teammate_required
    - metadata.atm.spawn_policy: named_teammate_required
    """
    direct = re.search(
        r"(?m)^metadata:\n(?:^[ \t].*\n)*?^[ \t]+spawn_policy:\s*([^\n#]+)",
        frontmatter,
    )
    if direct and direct.group(1).strip().strip("'\"") == SPAWN_POLICY_NAMED_REQUIRED:
        return True

    nested = re.search(
        r"(?m)^metadata:\n(?:^[ \t].*\n)*?^[ \t]+atm:\n(?:^[ \t]{4,}.*\n)*?^[ \t]{4,}spawn_policy:\s*([^\n#]+)",
        frontmatter,
    )
    if nested and nested.group(1).strip().strip("'\"") == SPAWN_POLICY_NAMED_REQUIRED:
        return True

    return False


def requires_named_teammate(subagent_type: str) -> bool:
    """Determine policy from agent prompt metadata."""
    agent_path = _agent_file_for(subagent_type)
    if agent_path and agent_path.exists():
        try:
            body = agent_path.read_text(encoding="utf-8")
            frontmatter = _extract_frontmatter(body)
            if frontmatter is not None:
                return _frontmatter_requires_named_teammate(frontmatter)
        except Exception:
            # Fail open on parse/read errors.
            pass
    return False


def main() -> int:
    try:
        data = json.load(sys.stdin)
    except Exception:
        # Can't parse input - allow by default (fail open)
        return 0

    # Log for debugging (include process_id for diagnostics)
    try:
        log_entry = {**data, "process_id": os.getpid()}
        with DEBUG_LOG.open("a") as f:
            f.write(json.dumps(log_entry) + "\n")
    except Exception:
        pass

    tool_input = data.get("tool_input", {})
    subagent_type = tool_input.get("subagent_type", "")
    teammate_name = tool_input.get("name", "")  # If present, spawns named teammate
    team_name = tool_input.get("team_name", "")  # If present, spawns into team
    session_id = data.get("session_id", "")

    # Persist session ID as an audit/debug breadcrumb only.
    # IMPORTANT: this file is NOT read by `atm teams resume` in the
    # production path; session resolution uses CLAUDE_SESSION_ID/--session-id.
    # Written every time session_id is non-empty so local diagnostics can
    # inspect the latest observed hook payload.
    if session_id:
        try:
            SESSION_ID_FILE.write_text(session_id)
        except Exception:
            pass

    required_team, spawn_policy, co_leaders = load_spawn_policy_from_toml()

    # Rule 1: Agents with named-teammate policy must be spawned with teammate_name
    # WHY: They need full lifecycle (compaction, proper shutdown) to coordinate
    # long-running sprints. Background agents die at context limit.
    if requires_named_teammate(subagent_type) and not teammate_name:
        sys.stderr.write(
            f"BLOCKED: '{subagent_type}' requires named teammate spawn policy.\n"
            f"\n"
            f"Correct:\n"
            f'  Task(subagent_type="{subagent_type}", name="sm-sprint-X", team_name="<team>", ...)\n'
            f"\n"
            f"Wrong:\n"
            f'  Task(subagent_type="{subagent_type}", run_in_background=true)  # no name = dies at context limit\n'
        )
        return 2

    # Rule 3: Any explicit team_name must match .atm.toml default_team.
    # WHY: Wrong team_name can create/target the wrong team and hide ATM messages.
    if team_name and str(team_name).strip() and required_team and team_name != required_team:
        sys.stderr.write(
            f"BLOCKED: team_name must match .atm.toml core.default_team.\n"
            f"\n"
            f"Required team_name: {required_team!r}\n"
            f"Got team_name:      {team_name!r}\n"
            f"\n"
            f"Use:\n"
            f'  Task(..., team_name="{required_team}", ...)\n'
        )
        return 2

    # Rule 2: Enforce leaders-only spawn policy for spawn-capable Task calls.
    # Spawn-capable means a teammate name or team_name is provided.
    if (team_name and str(team_name).strip()) or (teammate_name and str(teammate_name).strip()):
        if spawn_policy == SPAWN_POLICY_ANY_MEMBER:
            return 0

        auth_team = required_team or team_name
        caller_identity = resolve_caller_identity(
            session_id=session_id,
            team_name=auth_team,
            env_identity=os.environ.get("ATM_IDENTITY"),
        )
        allowed = {"team-lead", *co_leaders}
        if caller_identity in allowed:
            return 0

        sys.stderr.write(
            f"{SPAWN_UNAUTHORIZED}: leaders-only spawn policy violation.\n"
            f"\n"
            f"Policy team: {auth_team or '<unknown>'}\n"
            f"Allowed identities: {', '.join(sorted(allowed))}\n"
            f"Resolved caller: {caller_identity or '<unknown>'}\n"
            f"\n"
            f"Action: run spawn as team-lead or add caller to [team.\"{auth_team}\"].co_leaders in .atm.toml.\n"
        )
        return 2

    # Allow: All checks passed
    # WHY: Either spawning non-orchestrator, or spawning background agent (no team_name)
    return 0


if __name__ == "__main__":
    sys.exit(main())
