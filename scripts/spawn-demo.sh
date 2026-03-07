#!/usr/bin/env bash
# atm spawn — interactive mode prototype
# Validates UX before Rust implementation.
#
# Usage:
#   ./scripts/spawn-demo.sh [agent-type] [--dry-run] [--tmux-help]
#
# Examples:
#   ./scripts/spawn-demo.sh codex            # interactive spawn of a codex agent
#   ./scripts/spawn-demo.sh sonnet --dry-run # show what would happen, no spawn
#   ./scripts/spawn-demo.sh --tmux-help      # tmux command reference guide

set -euo pipefail

# ─── tmux help ────────────────────────────────────────────────────────────────
tmux_help() {
  cat <<'TMUXHELP'

╔══════════════════════════════════════════════════════════════════════════════╗
║                        TMUX COMMAND REFERENCE                              ║
║              (for testing atm spawn pane/window modes)                     ║
╚══════════════════════════════════════════════════════════════════════════════╝

━━━ SESSIONS ━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━

  tmux new -s myname          Create new session named "myname"
  tmux ls                     List all sessions
  tmux attach -t myname       Attach to session "myname"
  tmux kill-session -t name   Kill a session

━━━ WINDOWS (tabs within a session) ━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━

  Prefix + c                  Create a new window
  Prefix + n / p              Next / previous window
  Prefix + 0-9                Switch to window by index
  Prefix + ,                  Rename current window
  Prefix + &                  Kill current window
  tmux new-window             Create window (from shell)
  tmux new-window -n myname   Create window with name

  List windows:
    tmux list-windows
    tmux list-windows -F "#{window_index}: #{window_name}"

━━━ PANES (splits within a window) ━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━

  Prefix + %                  Split horizontally (left|right)
  Prefix + "                  Split vertically (top|bottom)
  Prefix + arrow              Move between panes
  Prefix + z                  Zoom/unzoom current pane
  Prefix + x                  Kill current pane
  Prefix + q                  Show pane numbers (press number to jump)

  From shell:
    tmux split-window -h      Split horizontal in current window
    tmux split-window -v      Split vertical in current window
    tmux split-window -h -t 2 Split pane 2 horizontally

━━━ LISTING PANES ━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━

  # Panes in current window
  tmux list-panes

  # Panes in current window, formatted
  tmux list-panes -F "#{pane_index}: cmd=#{pane_current_command} title='#{pane_title}' pid=#{pane_pid} tty=#{pane_tty}"

  # ALL panes across ALL windows and sessions
  tmux list-panes -a -F "#{session_name}:#{window_index}.#{pane_index} cmd=#{pane_current_command} title='#{pane_title}'"

  # Useful format variables:
  #   #{pane_index}           Index in current window (0-based)
  #   #{pane_active}          1 if focused pane
  #   #{pane_current_command} Command running in pane
  #   #{pane_title}           Pane title (set by terminal)
  #   #{pane_pid}             PID of shell in pane
  #   #{pane_tty}             TTY device (e.g. /dev/ttys011)
  #   #{session_name}         Session name
  #   #{window_index}         Window index
  #   #{window_name}          Window name

━━━ CURRENT WINDOW / PANE INFO ━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━

  # Show info about the active pane
  tmux display-message -p "session=#{session_name} window=#{window_index} pane=#{pane_index}"

  # Get just the current window index
  tmux display-message -p "#{window_index}"

  # Get current pane TTY
  tmux display-message -p "#{pane_tty}"

━━━ TARGETING PANES ━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━

  # -t target format:  [session:]window[.pane]
  tmux send-keys -t 2 'echo hello' Enter     # Send to pane index 2, current window
  tmux send-keys -t 1.2 'ls' Enter           # Window 1, pane 2
  tmux send-keys -t myses:1.2 'ls' Enter     # Session myses, window 1, pane 2
  tmux send-keys -t myses:1.2 -l 'text'      # -l = literal (no key interpretation)

  # Select (focus) a pane
  tmux select-pane -t 2

  # Run a command in a new pane and stay there
  tmux split-window -h 'bash'

  # Run a command in a new pane (pane closes when done)
  tmux split-window -h 'echo hello; read'

━━━ SENDING COMMANDS TO A PANE ━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━

  # Type text + Enter into pane 2
  tmux send-keys -t 2 'claude --agent-id arch-ctm@atm-dev' Enter

  # Send just text (no Enter), then Enter separately
  tmux send-keys -t 2 -l 'my text here'
  tmux send-keys -t 2 Enter

  # Send Ctrl+C to a pane
  tmux send-keys -t 2 C-c

━━━ WINDOWS VS PANES — KEY DISTINCTION ━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━

  Windows  = like browser tabs (switch with Prefix+n, Prefix+0-9)
  Panes    = splits within one window (visible simultaneously)

  atm spawn modes:
    new-pane       → tmux split-window -h  (creates a split in current window)
    existing-pane  → tmux send-keys -t <n> (injects into a pane already open)
    current-pane   → no tmux, runs claude in the same pane you're in
    new-window     → tmux new-window  (future mode — new tab, not a split)

━━━ QUICK WORKFLOW FOR TESTING ━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━

  1. See what you have right now:
       tmux list-panes -F "#{pane_index}: #{pane_current_command} '#{pane_title}'"

  2. Create a blank pane to spawn into:
       tmux split-window -h bash

  3. List panes again to find its index:
       tmux list-panes -F "#{pane_index}: #{pane_current_command}"

  4. Run spawn-demo selecting that pane:
       ./scripts/spawn-demo.sh codex
       > 5=existing-pane   (switch to existing-pane mode)
       > ↵                 (confirm)

  5. See what command would be sent:
       ./scripts/spawn-demo.sh codex --dry-run

━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━
  Prefix = Ctrl+b by default. Check with: tmux show-options -g prefix
━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━

TMUXHELP
  exit 0
}

# ─── arg parsing ──────────────────────────────────────────────────────────────
AGENT_TYPE="codex"
DRY_RUN=false
for arg in "$@"; do
  case "$arg" in
    --tmux-help) tmux_help ;;
    --dry-run)   DRY_RUN=true ;;
    --*)         echo "unknown flag: $arg  (try --tmux-help or --dry-run)" >&2; exit 1 ;;
    *)           AGENT_TYPE="$arg" ;;
  esac
done

# ─── terminal detection ────────────────────────────────────────────────────────
if [[ ! -t 0 ]]; then
  echo "error: interactive mode requires a terminal (stdin is not a tty)" >&2
  echo "hint:  use 'atm spawn codex --team atm-dev --member arch-ctm' for non-interactive spawn" >&2
  exit 1
fi

# ─── colours ──────────────────────────────────────────────────────────────────
RED='\033[0;31m'; GRN='\033[0;32m'; YLW='\033[0;33m'
CYN='\033[0;36m'; BLD='\033[1m'; DIM='\033[2m'; RST='\033[0m'

# ─── static data (will come from daemon/config in real impl) ──────────────────
VALID_TEAMS=("atm-dev" "scmux")
VALID_MEMBERS=("arch-ctm" "scrum-master" "quality-mgr" "team-lead" "rust-developer")
VALID_MODELS=("codex-5.3-high" "codex-5.3-fast" "claude-sonnet-4-6" "claude-haiku-4-5" "claude-opus-4-6")
VALID_AGENT_TYPES=("general-purpose" "scrum-master" "rust-developer" "rust-qa-agent" "quality-mgr")
VALID_PANE_MODES=("new-pane" "existing-pane" "current-pane")

# ─── detect running members (stub) ────────────────────────────────────────────
running_members() {
  command -v atm &>/dev/null && atm members 2>/dev/null | awk '/online|active/{print $1}' || echo "team-lead"
}

# ─── discover tmux panes in current window ────────────────────────────────────
discover_panes() {
  if command -v tmux &>/dev/null && [[ -n "${TMUX:-}" ]]; then
    tmux list-panes -F "    pane #{pane_index}: cmd=#{pane_current_command}  title='#{pane_title}'  pid=#{pane_pid}"
  else
    echo "    (not inside a tmux session — pane targeting unavailable)"
  fi
}

# ─── validation ───────────────────────────────────────────────────────────────
validate_team()       { printf '%s\n' "${VALID_TEAMS[@]}"       | grep -qx "$1"; }
validate_member()     { [[ "$1" =~ ^[a-z][a-z0-9_-]{0,31}$ ]]; }
validate_model()      { printf '%s\n' "${VALID_MODELS[@]}"      | grep -qx "$1"; }
validate_agent_type() { printf '%s\n' "${VALID_AGENT_TYPES[@]}" | grep -qx "$1"; }
validate_pane_mode()  { printf '%s\n' "${VALID_PANE_MODES[@]}"  | grep -qx "$1"; }
validate_worktree()   { [[ -z "$1" ]] || [[ -d "$1" ]]; }

# ─── state ────────────────────────────────────────────────────────────────────
TEAM="atm-dev"
MEMBER="arch-ctm"
WORKTREE=""
declare -A ERRORS=()

case "$AGENT_TYPE" in
  codex)  MODEL="codex-5.3-high";    ATYPE="general-purpose" ;;
  sonnet) MODEL="claude-sonnet-4-6"; ATYPE="general-purpose" ;;
  haiku)  MODEL="claude-haiku-4-5";  ATYPE="general-purpose" ;;
  opus)   MODEL="claude-opus-4-6";   ATYPE="general-purpose" ;;
  *)      MODEL="codex-5.3-high";    ATYPE="general-purpose" ;;
esac
PANE_MODE="new-pane"

# ─── render review panel ──────────────────────────────────────────────────────
render_panel() {
  clear
  echo ""
  printf "  ${BLD}atm spawn${RST}  —  interactive mode"
  printf "   ${DIM}(agent: %s)${RST}\n" "$AGENT_TYPE"

  # tmux context line
  if command -v tmux &>/dev/null && [[ -n "${TMUX:-}" ]]; then
    local ctx
    ctx=$(tmux display-message -p "session=#{session_name}  window=#{window_index} (#{window_name})  active-pane=#{pane_index}" 2>/dev/null || echo "unknown")
    printf "  ${DIM}tmux: %s${RST}\n" "$ctx"
  else
    printf "  ${DIM}tmux: not in a tmux session${RST}\n"
  fi
  echo ""
  echo -e "  ${BLD}─────────────────────────────────────────────────────${RST}"

  _row() {
    local n="$1" label="$2" value="$3" key="$4"
    local err="${ERRORS[$key]:-}"
    if [[ -n "$err" ]]; then
      printf "  ${YLW}%2d.${RST}  %-14s ${YLW}%-24s${RST}  ${RED}✗ %s${RST}\n" \
        "$n" "${label}:" "$value" "$err"
    elif [[ -z "$value" ]]; then
      printf "  ${GRN}%2d.${RST}  %-14s ${DIM}(none)${RST}\n" "$n" "${label}:"
    else
      printf "  ${GRN}%2d.${RST}  %-14s ${CYN}%s${RST}\n" "$n" "${label}:" "$value"
    fi
  }

  _row 1 "team"        "$TEAM"      "team"
  _row 2 "member"      "$MEMBER"    "member"

  # warn if member already running
  local running
  running=$(running_members 2>/dev/null || echo "")
  if printf '%s\n' $running | grep -qx "$MEMBER" 2>/dev/null; then
    printf "               ${YLW}⚠  %s appears to already be running${RST}\n" "$MEMBER"
  fi

  _row 3 "model"       "$MODEL"     "model"
  _row 4 "agent-type"  "$ATYPE"     "agent_type"
  _row 5 "pane-mode"   "$PANE_MODE" "pane_mode"
  _row 6 "worktree"    "$WORKTREE"  "worktree"

  echo -e "  ${BLD}─────────────────────────────────────────────────────${RST}"

  # show pane list when relevant
  if [[ "$PANE_MODE" == "existing-pane" ]]; then
    echo ""
    echo -e "  ${DIM}Panes in current window:${RST}"
    discover_panes
  fi

  # validation errors + valid options
  if [[ ${#ERRORS[@]} -gt 0 ]]; then
    echo ""
    for key in "${!ERRORS[@]}"; do
      [[ "$key" == "_confirm" ]] && continue
      case "$key" in
        model)      printf "  ${DIM}valid models:       %s${RST}\n" "${VALID_MODELS[*]}" ;;
        agent_type) printf "  ${DIM}valid agent-types:  %s${RST}\n" "${VALID_AGENT_TYPES[*]}" ;;
        pane_mode)  printf "  ${DIM}valid pane-modes:   %s${RST}\n" "${VALID_PANE_MODES[*]}" ;;
        team)       printf "  ${DIM}valid teams:        %s${RST}\n" "${VALID_TEAMS[*]}" ;;
        member)     printf "  ${DIM}member: lowercase letters, numbers, hyphens, max 32 chars${RST}\n" ;;
        worktree)   printf "  ${DIM}worktree: must be an existing directory path${RST}\n" ;;
        parse)      printf "  ${RED}parse error: %s${RST}\n" "${ERRORS[parse]}" ;;
      esac
    done
    if [[ -n "${ERRORS[_confirm]:-}" ]]; then
      printf "\n  ${RED}↑ Fix errors before confirming.${RST}\n"
    fi
  fi

  echo ""
  echo -e "  ${BLD}↵ Enter${RST} to confirm  ·  ${BLD}q / Esc${RST} to cancel  ·  ${BLD}--dry-run${RST} flag for preview"
  echo -e "  ${DIM}Edit:  n=value  or  n=value,m=value2   e.g.  2=scrum-master,3=codex-5.3-fast${RST}"
  echo -e "  ${DIM}Help:  run with --tmux-help for tmux command reference${RST}"
  echo ""
  printf "  > "
}

# ─── apply edits ──────────────────────────────────────────────────────────────
apply_edits() {
  local input="$1"
  ERRORS=()

  IFS=',' read -ra parts <<< "$input"
  for part in "${parts[@]}"; do
    part="${part// /}"
    if [[ ! "$part" =~ ^([0-9]+)=(.*)$ ]]; then
      ERRORS["parse"]="'${part}' is not valid — use n=value (e.g. 3=codex-5.3-fast)"
      continue
    fi
    local n="${BASH_REMATCH[1]}" val="${BASH_REMATCH[2]}"
    case "$n" in
      1) TEAM="$val";      validate_team "$val"       || ERRORS["team"]="unknown team '$val'" ;;
      2) MEMBER="$val";    validate_member "$val"     || ERRORS["member"]="invalid name '$val'" ;;
      3) MODEL="$val";     validate_model "$val"      || ERRORS["model"]="unknown model '$val'" ;;
      4) ATYPE="$val";     validate_agent_type "$val" || ERRORS["agent_type"]="unknown agent-type '$val'" ;;
      5) PANE_MODE="$val"; validate_pane_mode "$val"  || ERRORS["pane_mode"]="unknown pane-mode '$val'" ;;
      6) WORKTREE="$val";  validate_worktree "$val"   || ERRORS["worktree"]="path not found '$val'" ;;
      *) ERRORS["item_${n}"]="item $n does not exist (valid: 1-6)" ;;
    esac
  done
}

# ─── dry-run output ───────────────────────────────────────────────────────────
dry_run_output() {
  echo ""
  echo -e "  ${BLD}[dry-run] What would happen:${RST}"
  echo ""

  case "$PANE_MODE" in
    new-pane)
      echo -e "  ${DIM}1. Create new pane in current window:${RST}"
      echo -e "       tmux split-window -h"
      echo -e "  ${DIM}2. Run in new pane:${RST}"
      ;;
    existing-pane)
      echo -e "  ${DIM}1. Target an existing pane (selected by index):${RST}"
      echo -e "       tmux list-panes -F '#{pane_index}: #{pane_current_command}'"
      echo -e "  ${DIM}2. Send to selected pane:${RST}"
      ;;
    current-pane)
      echo -e "  ${DIM}1. Run in current pane:${RST}"
      ;;
  esac

  local launch_cmd="claude --agent-id ${MEMBER}@${TEAM} --agent-name ${MEMBER} --team-name ${TEAM}"
  [[ -n "$WORKTREE" ]] && launch_cmd="cd ${WORKTREE} && ${launch_cmd}"

  echo ""
  echo -e "       ${CYN}${launch_cmd}${RST}"
  echo ""
  echo -e "  ${DIM}3. Register ${MEMBER} in team ${TEAM} config (model: ${MODEL}, type: ${ATYPE})${RST}"
  echo ""
  echo -e "  ${YLW}No changes made (dry-run).${RST}"
  echo ""
}

# ─── confirm + execute ────────────────────────────────────────────────────────
execute_spawn() {
  echo ""
  echo -e "  ${GRN}✓ Confirmed.${RST}"
  echo ""
  echo -e "  Spawning ${BLD}${MEMBER}${RST} on team ${BLD}${TEAM}${RST}"
  echo -e "  model: ${MODEL}  agent-type: ${ATYPE}  pane-mode: ${PANE_MODE}"
  [[ -n "$WORKTREE" ]] && echo -e "  worktree: ${WORKTREE}"
  echo ""
  echo -e "  ${DIM}[prototype — no actual spawn performed]${RST}"
  echo -e "  ${DIM}Real impl: write member to config, create/target pane, send claude launch cmd${RST}"
  echo ""
}

# ─── main loop ────────────────────────────────────────────────────────────────
while true; do
  render_panel

  IFS= read -r line

  # Esc sequence or q = cancel
  if [[ "$line" == $'\e' || "$line" == "q" || "$line" == "Q" ]]; then
    echo ""
    echo -e "  ${DIM}Cancelled.${RST}"
    echo ""
    exit 0
  fi

  # empty line = confirm
  if [[ -z "$line" ]]; then
    if [[ ${#ERRORS[@]} -gt 0 ]]; then
      ERRORS["_confirm"]="fix errors above first"
      continue
    fi
    if $DRY_RUN; then
      dry_run_output
    else
      execute_spawn
    fi
    exit 0
  fi

  apply_edits "$line"
done
