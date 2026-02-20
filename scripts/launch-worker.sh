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
TEAM="${ATM_TEAM:-default-team}"
MODE="${LAUNCH_MODE:-session}"  # "session" (default) or "pane" (new pane in current window)

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

# Build environment string for tmux
ENV_VARS="ATM_IDENTITY=$AGENT_NAME ATM_TEAM=$TEAM"
if [[ -n "${ATM_HOME:-}" ]]; then
    ENV_VARS="$ENV_VARS ATM_HOME=$ATM_HOME"
fi

if [[ "$MODE" == "pane" ]]; then
    # Launch as a new pane in the current tmux window
    if [[ -z "${TMUX:-}" ]]; then
        echo "Error: LAUNCH_MODE=pane requires an active tmux session." >&2
        exit 1
    fi
    PANE_ID=$(tmux split-window -h -P -F '#{pane_id}' "env $ENV_VARS $WORKER_CMD; echo ''; echo 'Worker exited. Press Enter to close.'; read")
    tmux select-layout even-horizontal
    PANE_INFO="$PANE_ID (current window)"

    # Update tmuxPaneId in team config.json so Claude Code can inject messages
    if atm teams add-member "$TEAM" "$AGENT_NAME" --pane-id "$PANE_ID" 2>/dev/null; then
        echo "  Config:    tmuxPaneId=$PANE_ID registered via atm teams add-member"
    else
        echo "  Warning:   failed to register tmuxPaneId (member may not exist in team)" >&2
    fi

    echo "Launched worker '$AGENT_NAME' in new pane."
    echo ""
    echo "  Pane:      $PANE_INFO"
    echo "  Identity:  ATM_IDENTITY=$AGENT_NAME"
    echo "  Team:      ATM_TEAM=$TEAM"
    echo "  Command:   $WORKER_CMD"
    echo ""
    echo "  Send keys: tmux send-keys -t $PANE_ID 'your message' Enter"
else
    # Default: launch as a new tmux session
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

    tmux new-session -d -s "$AGENT_NAME" "env $ENV_VARS $WORKER_CMD; echo ''; echo 'Worker exited. Press Enter to close.'; read"

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
fi
