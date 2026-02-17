#!/usr/bin/env bash
# launch-worker.sh — Launch an ATM worker agent in a tmux session
#
# Usage:
#   ./scripts/launch-worker.sh <agent-name> [command]
#
# Examples:
#   ./scripts/launch-worker.sh arch-ctm              # codex --yolo
#   ./scripts/launch-worker.sh dev-agent "codex --yolo --model o3"
#   ./scripts/launch-worker.sh qa-agent "claude"

set -euo pipefail

AGENT_NAME="${1:-}"
WORKER_CMD="${2:-codex --yolo}"
TEAM="${ATM_TEAM:-atm-dev}"

if [[ -z "$AGENT_NAME" ]]; then
    echo "Usage: $0 <agent-name> [command]"
    echo ""
    echo "  agent-name   ATM_IDENTITY for the worker (used as tmux session name)"
    echo "  command      Command to run (default: codex --yolo)"
    echo ""
    echo "Examples:"
    echo "  $0 arch-ctm"
    echo "  $0 dev-agent \"codex --yolo --model o3\""
    exit 1
fi

# Check dependencies
if ! command -v tmux &>/dev/null; then
    echo "Error: tmux is not installed or not in PATH" >&2
    exit 1
fi

# Extract the base command name for the PATH check (first word)
BASE_CMD="${WORKER_CMD%% *}"
if ! command -v "$BASE_CMD" &>/dev/null; then
    echo "Error: '$BASE_CMD' is not installed or not in PATH" >&2
    exit 1
fi

# Check if Codex notify hook is configured (only warn for codex commands)
if [[ "$WORKER_CMD" == *"codex"* ]]; then
    CODEX_CONFIG="$HOME/.codex/config.toml"
    if [[ ! -f "$CODEX_CONFIG" ]] || ! grep -q '^notify' "$CODEX_CONFIG" 2>/dev/null; then
        echo "⚠️  Codex notify hook not configured."
        echo "    Run: ./scripts/setup-codex-hooks.sh --agent $AGENT_NAME --team $TEAM"
        echo ""
    fi
fi

# Check if session already exists
if tmux has-session -t "$AGENT_NAME" 2>/dev/null; then
    echo "tmux session '$AGENT_NAME' already exists."
    echo ""
    echo "  Attach:  tmux attach -t $AGENT_NAME"
    echo "  Kill:    tmux kill-session -t $AGENT_NAME"
    echo ""
    read -rp "Attach to existing session? [Y/n] " answer
    case "${answer:-Y}" in
        [Nn]*) echo "Aborted."; exit 0 ;;
        *)     exec tmux attach -t "$AGENT_NAME" ;;
    esac
fi

# Build environment string for tmux
ENV_VARS="ATM_IDENTITY=$AGENT_NAME ATM_TEAM=$TEAM"
if [[ -n "${ATM_HOME:-}" ]]; then
    ENV_VARS="$ENV_VARS ATM_HOME=$ATM_HOME"
fi

# Create tmux session with environment variables and launch the command
tmux new-session -d -s "$AGENT_NAME" "env $ENV_VARS $WORKER_CMD; echo ''; echo 'Worker exited. Press Enter to close.'; read"

# Get pane info for daemon discovery
PANE_INFO=$(tmux list-panes -t "$AGENT_NAME" -F '#{session_name}:#{window_index}.#{pane_index} (pid #{pane_pid})')

echo "Launched worker '$AGENT_NAME' in tmux session."
echo ""
echo "  Session:   $AGENT_NAME"
echo "  Identity:  ATM_IDENTITY=$AGENT_NAME"
echo "  Team:      ATM_TEAM=$TEAM"
echo "  Command:   $WORKER_CMD"
echo "  Pane:      $PANE_INFO"
echo ""
echo "  Attach:    tmux attach -t $AGENT_NAME"
echo "  Send keys: tmux send-keys -t $AGENT_NAME 'your message' Enter"
