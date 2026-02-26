#!/usr/bin/env bash
set -euo pipefail

tmp_out="$(mktemp)"
cleanup() {
  rm -f "$tmp_out"
}
trap cleanup EXIT

echo "Running MCP Inspector CLI smoke check against reference MCP server..."
if command -v timeout >/dev/null 2>&1; then
  timeout 60 npx -y @modelcontextprotocol/inspector --cli \
    npx -y @modelcontextprotocol/server-everything \
    --method tools/list >"$tmp_out"
else
  python3 - <<'PY' >"$tmp_out"
import subprocess
subprocess.run(
    [
        "npx",
        "-y",
        "@modelcontextprotocol/inspector",
        "--cli",
        "npx",
        "-y",
        "@modelcontextprotocol/server-everything",
        "--method",
        "tools/list",
    ],
    check=True,
    timeout=60,
    text=True,
)
PY
fi

echo "Validating inspector response payload..."
if ! grep -q '"name": "echo"' "$tmp_out"; then
  echo "Expected echo tool not found in MCP Inspector output"
  echo "--- inspector output ---"
  cat "$tmp_out"
  exit 1
fi

echo "MCP Inspector smoke check passed."
