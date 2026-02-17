#!/usr/bin/env bash
# launch-arch-ctm.sh — Launch the arch-ctm Codex agent in a tmux session
#
# Convenience wrapper around launch-worker.sh for the arch-ctm agent.
#
# Usage:
#   ./scripts/launch-arch-ctm.sh

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"

exec "$SCRIPT_DIR/launch-worker.sh" arch-ctm "codex --yolo"
