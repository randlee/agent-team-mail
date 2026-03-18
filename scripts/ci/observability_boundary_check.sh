#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"

declare -a matches=()

is_allowed_sc_observability_rust_path() {
  local rel="$1"
  case "$rel" in
    crates/atm/src/main.rs) return 0 ;;
    crates/sc-compose/src/main.rs) return 0 ;;
    # All atm-daemon/src/** modules are approved daemon-local observability
    # wiring points; the boundary rule is about keeping transport ownership out
    # of non-entrypoint crates, not forcing daemon wiring into main.rs only.
    crates/atm-daemon/src/*) return 0 ;;
    # The dedicated OTLP adapter crate may depend on the canonical
    # sc-observability signal contracts while owning transport details.
    crates/sc-observability-otlp/src/*) return 0 ;;
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
    crates/sc-observability/src/otlp_adapter.rs) continue ;;
    crates/sc-observability-otlp/*) continue ;;
  esac
  matches+=("${match}::ARCH-BOUNDARY-002 violation: direct sc-observability-otlp import outside dedicated adapter layer")
done < <(
  grep -RInE '\bsc_observability_otlp\b' "$ROOT/crates" --include='*.rs' \
    | grep -v '/tests/' \
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
  grep -RInE '(^|[^A-Za-z0-9])opentelemetry([_-][A-Za-z0-9_]+)?([^A-Za-z0-9]|$)' "$ROOT/crates" --include='*.rs' --include='Cargo.toml' \
    | grep -v '/tests/' \
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
