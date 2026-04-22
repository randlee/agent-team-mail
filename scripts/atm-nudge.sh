#!/bin/bash
set -euo pipefail

to="$(echo "$ATM_POST_SEND" | jq -r '.to' 2>/dev/null)"
task_id="$(echo "$ATM_POST_SEND" | jq -r '.task_id // empty' 2>/dev/null)"
msg="You have unread ATM messages. Run: atm read --team atm-dev${task_id:+ (task: $task_id)}"

case "$to" in
    arch-ctm@*)
        pane="atm-dev:1.2"
        ;;
    team-lead@*)
        pane="atm-dev:1.1"
        ;;
    *)
        exit 0
        ;;
esac

tmux send-keys -t "$pane" -l "$msg"
sleep 0.2
tmux send-keys -t "$pane" Enter
sleep 0.5
tmux send-keys -t "$pane" Enter
