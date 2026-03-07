#!/usr/bin/env bash
# atm spawn interactive mode prototype
# Run: ./spawn-demo.sh [agent-type]
# Demonstrates the review-panel UX before Rust implementation.

set -euo pipefail

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

# ─── detect running members (stub — real impl queries daemon) ─────────────────
running_members() {
  # In real impl: atm members --json | jq -r '.[] | select(.state != "offline") | .name'
  echo "team-lead"
}

# ─── discover tmux panes in current window ────────────────────────────────────
discover_panes() {
  if command -v tmux &>/dev/null && [[ -n "${TMUX:-}" ]]; then
    tmux list-panes -F "  pane #{pane_index}: '#{pane_title}' cmd=#{pane_current_command} pid=#{pane_pid}"
  else
    echo "  (not inside a tmux session)"
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
AGENT_TYPE="${1:-codex}"
TEAM="atm-dev"
MEMBER="arch-ctm"
MODEL="codex-5.3-high"
ATYPE="general-purpose"
PANE_MODE="new-pane"
WORKTREE=""
declare -A ERRORS=()

# default model based on agent type arg
case "$AGENT_TYPE" in
  codex)   MODEL="codex-5.3-high" ;;
  sonnet)  MODEL="claude-sonnet-4-6"; ATYPE="general-purpose" ;;
  haiku)   MODEL="claude-haiku-4-5";  ATYPE="general-purpose" ;;
  opus)    MODEL="claude-opus-4-6";   ATYPE="general-purpose" ;;
esac

# ─── render review panel ──────────────────────────────────────────────────────
render_panel() {
  clear
  echo ""
  echo -e "${BLD}  atm spawn${RST} — interactive mode"
  echo -e "  ${DIM}Spawning agent type: ${AGENT_TYPE}${RST}"
  echo ""

  # current tmux context
  if [[ -n "${TMUX:-}" ]]; then
    local win
    win=$(tmux display-message -p "session=#{session_name} window=#{window_index} (#{window_name})" 2>/dev/null || echo "unknown")
    echo -e "  ${DIM}tmux context: ${win}${RST}"
  fi
  echo ""

  echo -e "  ${BLD}──────────────────────────────────────────────────${RST}"

  # item renderer: item_row <n> <label> <value> <error_key>
  item_row() {
    local n="$1" label="$2" value="$3" key="$4"
    local err="${ERRORS[$key]:-}"
    local val_str
    if [[ -z "$value" ]]; then
      val_str="${DIM}(none)${RST}"
    else
      val_str="${CYN}${value}${RST}"
    fi
    if [[ -n "$err" ]]; then
      printf "  ${YLW}%2d.${RST} %-14s %s  ${RED}✗ %s${RST}\n" "$n" "${label}:" "$value" "$err"
    else
      printf "  ${GRN}%2d.${RST} %-14s " "$n" "${label}:"
      echo -e "${val_str}"
    fi
  }

  item_row 1 "team"         "$TEAM"      "team"
  item_row 2 "member"       "$MEMBER"    "member"

  # warn if member already running
  local running
  running=$(running_members)
  if echo "$running" | grep -qx "$MEMBER" 2>/dev/null; then
    echo -e "             ${YLW}⚠ ${MEMBER} appears to be running already${RST}"
  fi

  item_row 3 "model"        "$MODEL"     "model"
  item_row 4 "agent-type"   "$ATYPE"     "agent_type"
  item_row 5 "pane-mode"    "$PANE_MODE" "pane_mode"
  item_row 6 "worktree"     "$WORKTREE"  "worktree"

  echo -e "  ${BLD}──────────────────────────────────────────────────${RST}"
  echo ""

  # show pane list if pane-mode involves tmux
  if [[ "$PANE_MODE" == "existing-pane" ]]; then
    echo -e "  ${DIM}Available panes in current window:${RST}"
    discover_panes
    echo ""
  fi

  # show errors + valid options
  if [[ ${#ERRORS[@]} -gt 0 ]]; then
    echo -e "  ${RED}Errors — fix highlighted items before confirming.${RST}"
    for key in "${!ERRORS[@]}"; do
      case "$key" in
        model)      echo -e "  ${DIM}valid models:      ${VALID_MODELS[*]}${RST}" ;;
        agent_type) echo -e "  ${DIM}valid agent-types: ${VALID_AGENT_TYPES[*]}${RST}" ;;
        pane_mode)  echo -e "  ${DIM}valid pane-modes:  ${VALID_PANE_MODES[*]}${RST}" ;;
        team)       echo -e "  ${DIM}valid teams:       ${VALID_TEAMS[*]}${RST}" ;;
      esac
    done
    echo ""
  fi

  echo -e "  ${BLD}↵ Enter${RST} to confirm  ·  ${BLD}Esc${RST} or ${BLD}q${RST} to cancel"
  echo -e "  Change items: ${DIM}e.g. 1=atm-dev, 3=codex-5.3-fast, 2=scrum-master,3=claude-haiku-4-5${RST}"
  echo ""
  printf "  > "
}

# ─── apply edits ──────────────────────────────────────────────────────────────
apply_edits() {
  local input="$1"
  ERRORS=()

  # parse comma-separated n=value pairs
  IFS=',' read -ra parts <<< "$input"
  for part in "${parts[@]}"; do
    part="${part// /}"  # trim spaces
    if [[ ! "$part" =~ ^([0-9]+)=(.*)$ ]]; then
      ERRORS["parse"]="'${part}' is not valid — use n=value format (e.g. 3=codex-5.3-fast)"
      continue
    fi
    local n="${BASH_REMATCH[1]}"
    local val="${BASH_REMATCH[2]}"

    case "$n" in
      1) TEAM="$val";      validate_team "$val"       || ERRORS["team"]="unknown team '$val'" ;;
      2) MEMBER="$val";    validate_member "$val"     || ERRORS["member"]="invalid member name '$val'" ;;
      3) MODEL="$val";     validate_model "$val"      || ERRORS["model"]="unknown model '$val'" ;;
      4) ATYPE="$val";     validate_agent_type "$val" || ERRORS["agent_type"]="unknown agent-type '$val'" ;;
      5) PANE_MODE="$val"; validate_pane_mode "$val"  || ERRORS["pane_mode"]="unknown pane-mode '$val'" ;;
      6) WORKTREE="$val";  validate_worktree "$val"   || ERRORS["worktree"]="path does not exist '$val'" ;;
      *) ERRORS["item_${n}"]="item ${n} does not exist (valid: 1-6)" ;;
    esac
  done
}

# ─── dry-run output ───────────────────────────────────────────────────────────
dry_run_output() {
  echo ""
  echo -e "${BLD}  [dry-run] What would happen:${RST}"
  echo ""
  case "$PANE_MODE" in
    new-pane)
      echo "  1. tmux split-window -h  (create new pane in current window)"
      echo "  2. In new pane, run:"
      ;;
    existing-pane)
      echo "  1. Target existing pane (user selects from list)"
      echo "  2. Send to that pane:"
      ;;
    current-pane)
      echo "  1. In current pane, run:"
      ;;
  esac

  local launch_cmd="claude --agent-id ${MEMBER}@${TEAM} --agent-name ${MEMBER} --team-name ${TEAM}"
  [[ -n "$WORKTREE" ]] && launch_cmd="cd ${WORKTREE} && ${launch_cmd}"

  echo ""
  echo -e "     ${CYN}${launch_cmd}${RST}"
  echo ""
  echo "  3. Register ${MEMBER} in team ${TEAM} config"
  echo "     model: ${MODEL}, agent-type: ${ATYPE}"
  echo ""
  echo -e "  ${DIM}No changes made (dry-run).${RST}"
  echo ""
}

# ─── confirm execution ────────────────────────────────────────────────────────
execute_spawn() {
  echo ""
  echo -e "  ${GRN}✓ Confirmed.${RST} Spawning ${BLD}${MEMBER}${RST} on team ${BLD}${TEAM}${RST}..."
  echo ""
  echo -e "  ${DIM}[prototype — no actual spawn performed]${RST}"
  echo ""
  # Real impl would:
  # 1. Write member to team config
  # 2. tmux split-window / target existing pane
  # 3. Send claude launch command to pane
  # 4. Optionally wait for agent to register (poll atm members)
}

# ─── main loop ────────────────────────────────────────────────────────────────
# check for --dry-run flag
DRY_RUN=false
for arg in "$@"; do
  [[ "$arg" == "--dry-run" ]] && DRY_RUN=true
done

while true; do
  render_panel

  # read one line; handle Esc
  IFS= read -r line

  # Esc sequence or 'q' = cancel
  if [[ "$line" == $'\e' || "$line" == "q" || "$line" == "Q" ]]; then
    echo ""
    echo -e "  ${DIM}Cancelled.${RST}"
    echo ""
    exit 0
  fi

  # empty line = confirm (if no errors)
  if [[ -z "$line" ]]; then
    if [[ ${#ERRORS[@]} -gt 0 ]]; then
      # errors still present — stay in loop, flash message
      ERRORS["_confirm"]="fix errors above before confirming"
      continue
    fi
    if $DRY_RUN; then
      dry_run_output
    else
      execute_spawn
    fi
    exit 0
  fi

  # otherwise parse edit instructions
  apply_edits "$line"
done
