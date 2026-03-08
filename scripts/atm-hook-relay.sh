#!/usr/bin/env bash
# Backward-compatible wrapper. Product/runtime logic lives in Python.
set -euo pipefail
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
exec python3 "$SCRIPT_DIR/atm-hook-relay.py" "$@"
