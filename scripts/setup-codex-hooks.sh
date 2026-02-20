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
#   --team from .atm.toml [core].default_team

set -euo pipefail

# Parse arguments
AGENT="arch-ctm"
TEAM=""

# Handle flag-based arguments
while [[ $# -gt 0 ]]; do
    case "$1" in
        --agent)
            if [[ $# -lt 2 ]]; then
                echo "Error: --agent requires a value" >&2
                exit 1
            fi
            AGENT="$2"
            shift 2
            ;;
        --team)
            if [[ $# -lt 2 ]]; then
                echo "Error: --team requires a value" >&2
                exit 1
            fi
            TEAM="$2"
            shift 2
            ;;
        *)
            echo "Error: unknown argument: $1" >&2
            echo "Usage: $0 [--agent <name>] [--team <team>]" >&2
            exit 1
            ;;
    esac
done

# Determine paths
CODEX_CONFIG="$HOME/.codex/config.toml"
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
RELAY_SCRIPT="$SCRIPT_DIR/atm-hook-relay.sh"
REPO_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"
ATM_TOML="$REPO_ROOT/.atm.toml"

if [[ ! -f "$ATM_TOML" ]]; then
    echo "Error: .atm.toml not found at $ATM_TOML" >&2
    echo "Set up [core].default_team in repo .atm.toml before configuring hooks." >&2
    exit 1
fi

REQUIRED_TEAM="$(
python3 - "$ATM_TOML" <<'PY'
import sys
from pathlib import Path

path = Path(sys.argv[1])
with path.open("rb") as f:
    import tomllib
    config = tomllib.load(f)

team = config.get("core", {}).get("default_team")
if not isinstance(team, str) or not team.strip():
    raise SystemExit(2)
print(team.strip())
PY
)" || {
    echo "Error: failed to read [core].default_team from $ATM_TOML" >&2
    echo "Ensure .atm.toml contains: [core] default_team = \"<team-name>\"" >&2
    exit 1
}

if [[ -z "$TEAM" ]]; then
    TEAM="$REQUIRED_TEAM"
elif [[ "$TEAM" != "$REQUIRED_TEAM" ]]; then
    echo "Error: --team mismatch with repo .atm.toml" >&2
    echo "  .atm.toml [core].default_team = $REQUIRED_TEAM" >&2
    echo "  provided --team               = $TEAM" >&2
    echo "Refusing to write a mismatched team to ~/.codex/config.toml." >&2
    exit 1
fi

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
