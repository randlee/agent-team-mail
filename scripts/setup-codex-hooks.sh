#!/usr/bin/env bash
# setup-codex-hooks.sh — Configure Codex notify hook for ATM daemon
#
# This script adds a `notify` line to ~/.codex/config.toml that calls atm-hook-relay.sh
# whenever Codex completes an agent turn.
#
# Usage:
#   ./scripts/setup-codex-hooks.sh [--agent <name>] [--team <team>]
#
# Defaults:
#   --agent arch-ctm
#   --team default-team

set -euo pipefail

# Parse arguments
AGENT="${1:---agent}"
AGENT="${2:-arch-ctm}"
TEAM="${3:---team}"
TEAM="${4:-default-team}"

# Handle flag-based arguments
while [[ $# -gt 0 ]]; do
    case "$1" in
        --agent)
            AGENT="$2"
            shift 2
            ;;
        --team)
            TEAM="$2"
            shift 2
            ;;
        *)
            shift
            ;;
    esac
done

# Determine paths
CODEX_CONFIG="$HOME/.codex/config.toml"
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
RELAY_SCRIPT="$SCRIPT_DIR/atm-hook-relay.sh"

# Ensure relay script exists and is executable
if [[ ! -f "$RELAY_SCRIPT" ]]; then
    echo "Error: Relay script not found at $RELAY_SCRIPT" >&2
    exit 1
fi

chmod +x "$RELAY_SCRIPT"

# Check if config.toml already has a notify line
if [[ -f "$CODEX_CONFIG" ]] && grep -q '^notify' "$CODEX_CONFIG"; then
    echo "⚠️  Warning: ~/.codex/config.toml already contains a 'notify' configuration."
    echo ""
    echo "Current notify config:"
    grep '^notify' "$CODEX_CONFIG"
    echo ""
    echo "Refusing to overwrite. If you want to update it, edit $CODEX_CONFIG manually."
    exit 1
fi

# Ensure parent directory exists
mkdir -p "$(dirname "$CODEX_CONFIG")"

# Append notify configuration
cat >> "$CODEX_CONFIG" <<EOF

# ATM Hook Relay — notifies ATM daemon of agent turn completions
notify = ["$RELAY_SCRIPT", "--agent", "$AGENT", "--team", "$TEAM"]
EOF

echo "✅ Added notify hook to ~/.codex/config.toml"
echo ""
echo "Configuration:"
echo "  Agent:  $AGENT"
echo "  Team:   $TEAM"
echo "  Relay:  $RELAY_SCRIPT"
echo ""
echo "To verify, check:"
echo "  cat ~/.codex/config.toml"
echo ""
echo "Events will be written to:"
echo "  \${ATM_HOME:-\$HOME}/.claude/daemon/hooks/events.jsonl"
