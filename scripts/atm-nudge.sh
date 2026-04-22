#!/bin/bash
set -euo pipefail

recipient="${1:?recipient required}"
msg="You have unread ATM messages. Run: atm read --team atm-dev"

case "$recipient" in
    arch-ctm)
        pane="atm-dev:1.2"
        ;;
    team-lead)
        pane="atm-dev:1.1"
        ;;
    *)
        exit 0
        ;;
esac

tmux set-buffer -- "$msg"
tmux paste-buffer -t "$pane"
tmux send-keys -t "$pane" Enter
