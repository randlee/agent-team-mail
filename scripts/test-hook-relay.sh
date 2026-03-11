#!/usr/bin/env bash
# test-hook-relay.sh — Test the atm-hook-relay.sh script
#
# This script creates a temporary directory, sets ATM_HOME, calls the relay
# script with a sample payload, and verifies the output.

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
RELAY_SCRIPT="$SCRIPT_DIR/atm-hook-relay.sh"

# Ensure relay script exists
if [[ ! -f "$RELAY_SCRIPT" ]]; then
    echo "❌ FAIL: Relay script not found at $RELAY_SCRIPT"
    exit 1
fi

# Create temporary directory
TEMP_DIR=$(mktemp -d)
trap "rm -rf '$TEMP_DIR'" EXIT

# Set ATM_HOME to temp directory
export ATM_HOME="$TEMP_DIR"
EVENTS_FILE="$ATM_HOME/.atm/daemon/hooks/events.jsonl"

# Sample AfterAgent payload from Codex
SAMPLE_PAYLOAD='{
  "type": "agent-turn-complete",
  "thread-id": "thread-123",
  "turn-id": "turn-456",
  "cwd": "/home/user/project",
  "input-messages": ["Test input"],
  "last-assistant-message": "Test response"
}'

# Compact JSON (remove newlines for single-line payload)
SAMPLE_PAYLOAD=$(echo "$SAMPLE_PAYLOAD" | tr -d '\n' | tr -s ' ')

echo "Testing atm-hook-relay.sh..."
echo ""
echo "Temp directory: $TEMP_DIR"
echo "Events file: $EVENTS_FILE"
echo ""

# Call the relay script with test parameters
"$RELAY_SCRIPT" --agent "test-agent" --team "test-team" "$SAMPLE_PAYLOAD"

# Verify the events file was created
if [[ ! -f "$EVENTS_FILE" ]]; then
    echo "❌ FAIL: Events file was not created"
    exit 1
fi

echo "✅ Events file created"

# Read the events file
EVENT_LINE=$(cat "$EVENTS_FILE")

echo ""
echo "Event written:"
echo "$EVENT_LINE" | jq '.'
echo ""

# Verify expected fields exist using jq
if ! echo "$EVENT_LINE" | jq -e '.agent == "test-agent"' >/dev/null; then
    echo "❌ FAIL: agent field mismatch"
    exit 1
fi

if ! echo "$EVENT_LINE" | jq -e '.team == "test-team"' >/dev/null; then
    echo "❌ FAIL: team field mismatch"
    exit 1
fi

if ! echo "$EVENT_LINE" | jq -e '.received_at' >/dev/null; then
    echo "❌ FAIL: received_at field missing"
    exit 1
fi

if ! echo "$EVENT_LINE" | jq -e '.type == "agent-turn-complete"' >/dev/null; then
    echo "❌ FAIL: event type mismatch"
    exit 1
fi

if ! echo "$EVENT_LINE" | jq -e '."thread-id" == "thread-123"' >/dev/null; then
    echo "❌ FAIL: thread-id mismatch"
    exit 1
fi

if ! echo "$EVENT_LINE" | jq -e '.state == "idle"' >/dev/null; then
    echo "❌ FAIL: state field missing/mismatch"
    exit 1
fi

if ! echo "$EVENT_LINE" | jq -e '.timestamp' >/dev/null; then
    echo "❌ FAIL: timestamp field missing"
    exit 1
fi

if ! echo "$EVENT_LINE" | jq -e '.idempotency_key' >/dev/null; then
    echo "❌ FAIL: idempotency_key field missing"
    exit 1
fi

echo "✅ All fields validated"
echo ""
echo "🎉 PASS: atm-hook-relay.sh working correctly"
