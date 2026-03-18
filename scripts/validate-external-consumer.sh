#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
TARGET_REPO="$ROOT"
DRY_RUN=0

usage() {
  cat <<'EOF'
Usage: scripts/validate-external-consumer.sh [--repo PATH] [--dry-run]

Checks an external consumer repo against the AW.6 observability contract:
  - sc-observability dependency present in Cargo.toml
  - no sc_observability_otlp imports outside approved entry-point files
  - no opentelemetry* imports outside the dedicated transport layer
EOF
}

while [[ $# -gt 0 ]]; do
  case "$1" in
    --repo)
      TARGET_REPO="$2"
      shift 2
      ;;
    --dry-run)
      DRY_RUN=1
      shift
      ;;
    -h|--help)
      usage
      exit 0
      ;;
    *)
      echo "unknown argument: $1" >&2
      usage >&2
      exit 2
      ;;
  esac
done

TARGET_REPO="$(cd "$TARGET_REPO" && pwd)"

if [[ "$DRY_RUN" -eq 1 ]]; then
  cat <<EOF
{
  "status": "DRY_RUN",
  "repo": "$TARGET_REPO",
  "checks": [
    "Cargo.toml includes sc-observability",
    "no sc_observability_otlp import outside approved entry-point files",
    "no opentelemetry* import outside dedicated transport layer"
  ],
  "approved_entrypoints": [
    "src/main.rs",
    "src/bin/*.rs"
  ]
}
EOF
  exit 0
fi

declare -a matches=()

approve_otlp_import_path() {
  local rel="$1"
  case "$rel" in
    src/main.rs|src/bin/*.rs|crates/*/src/main.rs|crates/*/src/bin/*.rs) return 0 ;;
    *) return 1 ;;
  esac
}

has_sc_observability_dep=0
while IFS= read -r manifest; do
  [[ -z "$manifest" ]] && continue
  if grep -qE '(^|\s)sc-observability(\s|=|\{)' "$manifest"; then
    has_sc_observability_dep=1
    break
  fi
done < <(find "$TARGET_REPO" -name Cargo.toml -type f | sort)

if [[ "$has_sc_observability_dep" -ne 1 ]]; then
  echo "::error::external consumer contract violation: no Cargo.toml declares sc-observability"
  exit 1
fi

while IFS= read -r match; do
  [[ -z "$match" ]] && continue
  file="${match%%:*}"
  rel="${file#"$TARGET_REPO"/}"
  if approve_otlp_import_path "$rel"; then
    continue
  fi
  matches+=("${rel}${match#"$file"}::external consumer contract violation: direct sc_observability_otlp import outside approved entry-point files")
done < <(
  grep -RInE '\bsc_observability_otlp\b' "$TARGET_REPO" --include='*.rs' \
    | grep -v '/tests/' || true
)

while IFS= read -r match; do
  [[ -z "$match" ]] && continue
  file="${match%%:*}"
  rel="${file#"$TARGET_REPO"/}"
  case "$rel" in
    crates/sc-observability-otlp/*|src/main.rs|src/bin/*.rs|crates/*/src/main.rs|crates/*/src/bin/*.rs) continue ;;
  esac
  matches+=("${rel}${match#"$file"}::external consumer contract violation: direct opentelemetry import outside dedicated transport/entry-point layer")
done < <(
  grep -RInE '(^|[^A-Za-z0-9])opentelemetry([_-][A-Za-z0-9_]+)?([^A-Za-z0-9]|$)' "$TARGET_REPO" --include='*.rs' --include='Cargo.toml' \
    | grep -v '/tests/' || true
)

fail=0
for entry in "${matches[@]-}"; do
  [[ -z "$entry" ]] && continue
  match="${entry%%::external consumer contract violation:*}"
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

echo "External consumer contract check passed for $TARGET_REPO."
