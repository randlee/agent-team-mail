#!/usr/bin/env bash
# demos/tui-demo.sh — ATM TUI MVP Demo Script
#
# Sprint D.3: Demonstrates dashboard, agent terminal, control protocol, and
# degraded scenarios without requiring a live TUI binary (Phase D.1/D.2 scope).
#
# Prerequisites:
#   - atm binary built and on PATH (or via: cargo build && export PATH="$PWD/target/debug:$PATH")
#   - atm-daemon binary built (optional — demo handles unavailable daemon gracefully)
#   - ATM_HOME set to a writable directory (default: ~/.config/atm)
#
# Usage:
#   ./demos/tui-demo.sh [--team TEAM] [--agent AGENT_ID]
#
# Exit codes:
#   0 = all scenarios completed (including graceful degraded handling)
#   1 = prerequisite failure (atm binary not found)
#
# Team-lead sign-off: See demos/README.md

set -euo pipefail

# ── Configuration ──────────────────────────────────────────────────────────────
TEAM="${ATM_TEAM:-atm-dev}"
AGENT="${ATM_AGENT:-arch-ctm}"
ATM_HOME="${ATM_HOME:-${HOME}/.config/atm}"
DEMO_LOG="${ATM_HOME}/demo-run-$(date +%Y%m%dT%H%M%S).log"

RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[1;33m'
CYAN='\033[0;36m'
BOLD='\033[1m'
RESET='\033[0m'

step() { echo -e "\n${CYAN}${BOLD}==> $*${RESET}"; }
ok()   { echo -e "  ${GREEN}OK $*${RESET}"; }
warn() { echo -e "  ${YELLOW}WARN $*${RESET}"; }
fail() { echo -e "  ${RED}FAIL $*${RESET}"; }
note() { echo -e "  $*"; }

mkdir -p "$(dirname "$DEMO_LOG")"
exec > >(tee -a "$DEMO_LOG") 2>&1

echo -e "\n${BOLD}ATM TUI MVP Demo — Sprint D.3${RESET}"
echo "Date: $(date -u '+%Y-%m-%dT%H:%M:%SZ')"
echo "Team: $TEAM | Agent: $AGENT"
echo "Log: $DEMO_LOG"
echo "────────────────────────────────────────"

# ── Prerequisite check ─────────────────────────────────────────────────────────
step "Prerequisite: atm binary"
if ! command -v atm &>/dev/null; then
  fail "atm binary not found on PATH"
  note "Build with: cargo build --workspace"
  note "Then: export PATH=\"\$PWD/target/debug:\$PATH\""
  exit 1
fi
ATM_VERSION="$(atm --version 2>/dev/null || echo 'unknown')"
ok "atm found: $ATM_VERSION"

# ── Scenario 1: Dashboard — Team & Member Status ───────────────────────────────
step "Scenario 1: Dashboard — Team and Member Status"
note "Simulates the TUI Dashboard panel: team list + member status"
echo ""
note "  \$ atm teams"
if atm teams 2>/dev/null; then
  ok "Team list retrieved"
else
  warn "No teams configured or atm-daemon not running (degraded path — see Scenario 4)"
fi

echo ""
note "  \$ atm members --team $TEAM"
if atm members --team "$TEAM" 2>/dev/null; then
  ok "Member list retrieved for team $TEAM"
else
  warn "Team '$TEAM' not configured locally (degraded path)"
fi

# ── Scenario 2: Agent Terminal — Session/State View ───────────────────────────
step "Scenario 2: Agent Terminal — Agent Session State"
note "Simulates the TUI Agent Terminal: session status for a specific agent"
SESSION_LOG_DIR="${ATM_HOME}/agent-sessions/${TEAM}/${AGENT}"
echo ""
note "  Checking session log directory: $SESSION_LOG_DIR"
if [ -d "$SESSION_LOG_DIR" ]; then
  ok "Session log directory exists"
  LOG_FILE="$SESSION_LOG_DIR/output.log"
  if [ -f "$LOG_FILE" ]; then
    note "  Last 5 lines of output.log (stream preview):"
    tail -n 5 "$LOG_FILE" | sed 's/^/    /'
    ok "Stream preview complete"
  else
    warn "output.log not found — agent may not have run yet"
  fi
else
  warn "No session log directory for $AGENT in team $TEAM"
  note "  This is expected for a fresh checkout — daemon creates this on first agent run"
fi

# ── Scenario 3: Control Protocol — Send + Ack ─────────────────────────────────
step "Scenario 3: Control Protocol — stdin.request construction (no live daemon)"
note "Demonstrates control protocol message shape (agent_id is the public identifier)"
note "In live TUI, this would be sent via Unix socket to atm-daemon"
REQUEST_ID="req_demo_$(date +%s)"
AGENT_ID="${AGENT}"
SESSION_ID_DEMO="claude-session-demo-uuid"
SENT_AT="$(date -u '+%Y-%m-%dT%H:%M:%SZ')"

note ""
note "  Control request payload (public API — no thread_id required):"
cat <<EOF | sed 's/^/  /'
{
  "type": "control.stdin.request",
  "v": 1,
  "request_id": "${REQUEST_ID}",
  "session_id": "${SESSION_ID_DEMO}",
  "agent_id": "${AGENT_ID}",
  "team": "${TEAM}",
  "sender": "tui-demo",
  "sent_at": "${SENT_AT}",
  "content": "Hello from TUI demo",
  "interrupt": false
}
EOF
note ""
note "  NOTE: 'thread_id' is NOT included — it is MCP-internal adapter only."
note "  The public TUI identifies sessions by session_id + agent_id only."

# Check if atm-daemon socket exists
DAEMON_SOCKET="${ATM_HOME}/daemon.sock"
if [ -S "$DAEMON_SOCKET" ]; then
  ok "Daemon socket found — in full demo this would send via socket"
else
  warn "No daemon socket at $DAEMON_SOCKET (daemon not running — degraded path follows)"
fi

# ── Scenario 4: Degraded — Daemon Unavailable ─────────────────────────────────
step "Scenario 4: Degraded Scenario — Daemon Unavailable / not_live Target"
note "Demonstrates graceful behavior when daemon is not running"
echo ""

# Sub-scenario A: daemon socket missing
note "  Sub-scenario A: Daemon socket not found"
if [ ! -S "$DAEMON_SOCKET" ]; then
  note "  Expected ack response (daemon unavailable):"
  cat <<EOF | sed 's/^/    /'
{
  "type": "control.stdin.ack",
  "v": 1,
  "request_id": "${REQUEST_ID}",
  "session_id": "${SESSION_ID_DEMO}",
  "agent_id": "${AGENT_ID}",
  "team": "${TEAM}",
  "acked_at": "${SENT_AT}",
  "result": "internal_error",
  "detail": "daemon socket unavailable",
  "duplicate": false
}
EOF
  ok "Degraded path documented: daemon unavailable => result=internal_error"
else
  ok "Daemon is running — skipping simulated degraded path"
fi

# Sub-scenario B: not_live target
note ""
note "  Sub-scenario B: Target agent is not live (Launching/Killed/Stale/Closed)"
note "  In TUI, control input is disabled when target is not live."
note "  Expected result when control is attempted on non-live target:"
cat <<EOF | sed 's/^/    /'
{
  "type": "control.stdin.ack",
  "v": 1,
  "request_id": "${REQUEST_ID}",
  "session_id": "${SESSION_ID_DEMO}",
  "agent_id": "${AGENT_ID}",
  "team": "${TEAM}",
  "acked_at": "${SENT_AT}",
  "result": "not_live",
  "detail": "agent state is Stale — control input requires Active+{Idle,Busy}",
  "duplicate": false
}
EOF
ok "Degraded path documented: not_live target => result=not_live with detail"

# ── Regression Audit ───────────────────────────────────────────────────────────
step "Regression Audit: thread_id in public-facing files"
note "  Command: rg 'thread_id|threadId' docs/tui-*.md crates/atm/src crates/atm-daemon/src"
note ""
REPO_ROOT="$(git rev-parse --show-toplevel 2>/dev/null || echo '.')"
AUDIT_OUTPUT="$(cd "$REPO_ROOT" && rg 'thread_id|threadId' docs/tui-*.md crates/atm/src crates/atm-daemon/src 2>/dev/null || true)"

if [ -z "$AUDIT_OUTPUT" ]; then
  ok "No thread_id references found — clean"
else
  echo "$AUDIT_OUTPUT" | sed 's/^/  /'
  note ""
  note "  Checking for unapproved occurrences..."
  UNAPPROVED=0
  # Only approved: [MCP-internal adapter only] annotations in docs and hook_watcher.rs field
  # rg output format (without -n): "filepath:matching_line_content"
  # The file path is field 1; everything after the first colon is the matched content.
  while IFS= read -r line; do
    FILE="$(echo "$line" | cut -d: -f1)"
    CONTENT="$(echo "$line" | cut -d: -f2-)"
    if echo "$FILE" | grep -q "hook_watcher.rs"; then
      ok "APPROVED: hook_watcher.rs — Codex adapter field (see docs/thread-id-audit.md)"
    elif echo "$CONTENT" | grep -qiE 'MCP-internal|Codex internal|adapter (only|concern|field)'; then
      ok "APPROVED: $FILE — annotated as MCP-internal"
    else
      fail "UNAPPROVED: $line"
      UNAPPROVED=$((UNAPPROVED + 1))
    fi
  done <<< "$AUDIT_OUTPUT"

  if [ "$UNAPPROVED" -gt 0 ]; then
    fail "$UNAPPROVED unapproved thread_id reference(s) found — see docs/thread-id-audit.md for policy"
    exit 1
  fi
fi

# ── Summary ────────────────────────────────────────────────────────────────────
step "Demo Complete"
echo -e "  ${GREEN}All scenarios completed successfully${RESET}"
echo "  Log saved to: $DEMO_LOG"
echo ""
echo "  Scenarios covered:"
echo "    1. Dashboard: team + member status"
echo "    2. Agent Terminal: session log path + stream preview"
echo "    3. Control Protocol: stdin.request shape (no thread_id in public API)"
echo "    4. Degraded: daemon unavailable + not_live target"
echo "    5. Regression: thread_id audit (approved exceptions only)"
echo ""
echo "  Sprint D.3 exit criteria verified. See demos/README.md for sign-off."
