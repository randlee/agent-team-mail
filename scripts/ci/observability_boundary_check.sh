#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"

declare -a matches=()

is_allowed_sc_observability_rust_path() {
  local rel="$1"
  case "$rel" in
    crates/sc-compose/src/main.rs) return 0 ;;
    crates/atm-daemon/src/daemon/observability.rs) return 0 ;;
    *) return 1 ;;
  esac
}

while IFS= read -r match; do
  [[ -z "$match" ]] && continue
  file="${match%%:*}"
  if is_allowed_sc_observability_rust_path "$file"; then
    continue
  fi
  matches+=("${match}::ARCH-BOUNDARY-002 violation: direct sc-observability import outside approved entry-point/facade wiring")
done < <(
  grep -RInE '\bsc_observability\b' "$ROOT/crates" --include='*.rs' \
    | grep -v '/tests/' \
    | sed "s#^$ROOT/##" || true
)

while IFS= read -r match; do
  [[ -z "$match" ]] && continue
  file="${match%%:*}"
  case "$file" in
    crates/sc-observability-otlp/*) continue ;;
  esac
  matches+=("${match}::ARCH-BOUNDARY-002 violation: direct sc-observability-otlp import outside dedicated adapter layer")
done < <(
  grep -RInE '\bsc_observability_otlp\b' "$ROOT/crates" --include='*.rs' \
    | sed "s#^$ROOT/##" || true
)

while IFS= read -r match; do
  [[ -z "$match" ]] && continue
  file="${match%%:*}"
  case "$file" in
    crates/sc-observability-otlp/*) continue ;;
  esac
  matches+=("${match}::ARCH-BOUNDARY-002 violation: direct opentelemetry import outside dedicated adapter crate")
done < <(
  grep -RInE '\bopentelemetry(_sdk|_otlp)?\b' "$ROOT/crates" --include='*.rs' --include='Cargo.toml' \
    | sed "s#^$ROOT/##" || true
)

fail=0
for entry in "${matches[@]-}"; do
  [[ -z "$entry" ]] && continue
  match="${entry%%::ARCH-BOUNDARY-002 violation:*}"
  file="${match%%:*}"
  rest="${match#*:}"
  line="${rest%%:*}"
  message="${entry#*::}"
  echo "::error file=$file,line=$line::$message at $match"
  fail=1
done

if [[ "$fail" -ne 0 ]]; then
  exit 1
fi

echo "ARCH-BOUNDARY-002 check passed (observability imports within approved boundaries)."
