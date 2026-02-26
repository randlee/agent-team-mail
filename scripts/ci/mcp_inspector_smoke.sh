#!/usr/bin/env bash
set -euo pipefail

tmp_out="$(mktemp)"
cleanup() {
  rm -f "$tmp_out"
}
trap cleanup EXIT

echo "Running MCP Inspector CLI smoke check against reference MCP server..."
npx -y @modelcontextprotocol/inspector --cli \
  npx -y @modelcontextprotocol/server-everything \
  --method tools/list >"$tmp_out"

echo "Validating inspector response payload..."
if ! grep -q '"name": "echo"' "$tmp_out"; then
  echo "Expected echo tool not found in MCP Inspector output"
  echo "--- inspector output ---"
  cat "$tmp_out"
  exit 1
fi

echo "MCP Inspector smoke check passed."
