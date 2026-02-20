#!/usr/bin/env bash
# atm-hook-relay.sh — Fire-and-forget hook relay for Codex AfterAgent events
#
# This script is called by Codex's `notify` config when an agent turn completes.
# It appends the event to a JSONL file for the ATM daemon to consume.
#
# Usage:
#   atm-hook-relay.sh [--agent <name>] [--team <team>] <json-payload>
#
# The json-payload is passed by Codex as the last argument and contains:
#   {"type":"agent-turn-complete","thread-id":"...","turn-id":"...","cwd":"...","input-messages":[...],"last-assistant-message":"..."}
#
# The script enriches the payload with:
#   - agent: from --agent flag or ATM_IDENTITY env var
#   - team: from --team flag or ATM_TEAM env var
#   - received_at: ISO 8601 timestamp
#
# Output: Appends one JSON line to ${ATM_HOME:-$HOME}/.claude/daemon/hooks/events.jsonl

set -euo pipefail

# Parse command-line arguments
AGENT="${ATM_IDENTITY:-}"
TEAM="${ATM_TEAM:-}"
JSON_PAYLOAD=""

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
            # Last argument is the JSON payload
            JSON_PAYLOAD="$1"
            shift
            ;;
    esac
done

# Validate that we have a payload
if [[ -z "$JSON_PAYLOAD" ]]; then
    echo "Error: No JSON payload provided" >&2
    exit 1
fi

# Determine output file location
ATM_HOME="${ATM_HOME:-$HOME}"
EVENTS_FILE="$ATM_HOME/.claude/daemon/hooks/events.jsonl"

# Ensure parent directory exists
mkdir -p "$(dirname "$EVENTS_FILE")"

# Generate ISO 8601 timestamp
RECEIVED_AT=$(date -u +"%Y-%m-%dT%H:%M:%SZ")

# Build enriched event JSON
# We parse the input payload and add our fields, then output as a single line
ENRICHED_EVENT=$(cat <<EOF | tr -d '\n'
{
  "agent": "$AGENT",
  "team": "$TEAM",
  "received_at": "$RECEIVED_AT",
  "payload": $JSON_PAYLOAD
}
EOF
)

# Append to events file (atomic append operation)
echo "$ENRICHED_EVENT" >> "$EVENTS_FILE"
