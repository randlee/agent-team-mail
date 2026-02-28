#!/usr/bin/env bash
# Spawn a Claude Code teammate in a new tmux pane.
# Usage: spawn-teammate.sh <agent-name> <team-name> [color] [--model <model>] [--repo-root <path>]
#
# - agent-name:   name for the teammate (also used to find .claude/agents/<agent-name>.md)
# - team-name:    ATM team to join
# - color:        optional agent color (default: cyan, or from agent frontmatter)
# - --model:      optional model override (default: sonnet, or from agent frontmatter)
# - --repo-root:  optional working directory for agent (default: this repo root)
#
# Environment overrides (take precedence over args/frontmatter):
#   ATM_IDENTITY   override agent name used for ATM identity
#   ATM_TEAM       override team name
#   SPAWN_REPO_ROOT override working directory
#
# Examples:
#   ./scripts/spawn-teammate.sh quality-mgr atm-dev
#   ./scripts/spawn-teammate.sh quality-mgr atm-dev yellow
#   ./scripts/spawn-teammate.sh quality-mgr atm-dev yellow --model opus
#   ./scripts/spawn-teammate.sh quality-mgr other-team cyan --repo-root /path/to/other/repo

set -euo pipefail

AGENT_NAME="${1:?Usage: $0 <agent-name> <team-name> [color] [--model <model>] [--repo-root <path>]}"
TEAM_NAME="${2:?Usage: $0 <agent-name> <team-name> [color] [--model <model>] [--repo-root <path>]}"
shift 2

# Defaults
COLOR=""
MODEL=""
REPO_ROOT_OVERRIDE=""

# Parse remaining args
while [[ $# -gt 0 ]]; do
    case "$1" in
        --model)
            MODEL="${2:?--model requires a value}"
            shift 2
            ;;
        --model=*)
            MODEL="${1#--model=}"
            shift
            ;;
        --repo-root)
            REPO_ROOT_OVERRIDE="${2:?--repo-root requires a value}"
            shift 2
            ;;
        --repo-root=*)
            REPO_ROOT_OVERRIDE="${1#--repo-root=}"
            shift
            ;;
        -*)
            echo "Unknown option: $1" >&2
            exit 1
            ;;
        *)
            COLOR="$1"
            shift
            ;;
    esac
done

# Apply env overrides
AGENT_NAME="${ATM_IDENTITY:-$AGENT_NAME}"
TEAM_NAME="${ATM_TEAM:-$TEAM_NAME}"

# Resolve repo root: --repo-root arg > SPAWN_REPO_ROOT env > this script's repo
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
DEFAULT_REPO_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"
REPO_ROOT="${SPAWN_REPO_ROOT:-${REPO_ROOT_OVERRIDE:-$DEFAULT_REPO_ROOT}}"

# Read agent frontmatter if .claude/agents/<agent-name>.md exists in target repo
AGENT_FILE="$REPO_ROOT/.claude/agents/${AGENT_NAME}.md"
if [[ -f "$AGENT_FILE" ]]; then
    FM_MODEL="$(awk '/^---/{p=!p; next} p && /^model:/{print $2; exit}' "$AGENT_FILE")"
    FM_COLOR="$(awk '/^---/{p=!p; next} p && /^color:/{print $2; exit}' "$AGENT_FILE")"
    [[ -z "$MODEL" && -n "$FM_MODEL" ]] && MODEL="$FM_MODEL"
    [[ -z "$COLOR" && -n "$FM_COLOR" ]] && COLOR="$FM_COLOR"
fi

# Final defaults
MODEL="${MODEL:-sonnet}"
COLOR="${COLOR:-cyan}"

# Find latest claude binary
CLAUDE_BIN="$(ls -t ~/.local/share/claude/versions/[0-9]* 2>/dev/null | grep -v '\.json$' | head -1)"
if [[ -z "$CLAUDE_BIN" ]]; then
    echo "ERROR: Could not find claude binary in ~/.local/share/claude/versions/" >&2
    exit 1
fi

# Resolve parent session ID from config.json leadSessionId
PARENT_SESSION_ID="${CLAUDE_SESSION_ID:-}"
if [[ -z "$PARENT_SESSION_ID" ]]; then
    CONFIG="$HOME/.claude/teams/${TEAM_NAME}/config.json"
    if [[ -f "$CONFIG" ]]; then
        PARENT_SESSION_ID="$(python3 -c "import json; d=json.load(open('$CONFIG')); print(d.get('leadSessionId',''))")"
    fi
fi

AGENT_ID="${AGENT_NAME}@${TEAM_NAME}"

echo "Spawning '$AGENT_NAME' in team '$TEAM_NAME' (color=$COLOR, model=$MODEL)"
echo "Binary:     $CLAUDE_BIN"
echo "Repo root:  $REPO_ROOT"
echo "Session ID: ${PARENT_SESSION_ID:-<not found>}"

# Register agent in team config
atm teams add-member "$TEAM_NAME" "$AGENT_NAME" --agent-type "$AGENT_NAME"

# Build spawn command — cd to repo root, set ATM env vars, launch claude
CMD="cd '${REPO_ROOT}' && env CLAUDECODE=1 CLAUDE_CODE_EXPERIMENTAL_AGENT_TEAMS=1 ATM_IDENTITY='${AGENT_NAME}' ATM_TEAM='${TEAM_NAME}' '${CLAUDE_BIN}' --agent-id '${AGENT_ID}' --agent-name '${AGENT_NAME}' --team-name '${TEAM_NAME}' --agent-color '${COLOR}' --agent-type '${AGENT_NAME}' --model '${MODEL}' --dangerously-skip-permissions"
[[ -n "$PARENT_SESSION_ID" ]] && CMD="$CMD --parent-session-id '${PARENT_SESSION_ID}'"

# Spawn in a new pane in the current tmux window
PANE_ID="$(tmux split-window -h -P -F '#{pane_id}' "$CMD; exec zsh")"
echo "Spawned $AGENT_NAME in pane $PANE_ID"

# Update pane ID in config
atm teams add-member "$TEAM_NAME" "$AGENT_NAME" --agent-type "$AGENT_NAME" --pane-id "$PANE_ID"

# Send agent prompt from frontmatter body if agent file exists in target repo
if [[ -f "$AGENT_FILE" ]]; then
    AGENT_PROMPT="$(awk '/^---/{p++; next} p>=2{print}' "$AGENT_FILE")"
    if [[ -n "$AGENT_PROMPT" ]]; then
        sleep 3
        echo "Sending agent prompt from $AGENT_FILE..."
        atm send "$AGENT_NAME" "$AGENT_PROMPT" --team "$TEAM_NAME"
    fi
fi
