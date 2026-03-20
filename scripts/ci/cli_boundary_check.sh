#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
ATM_MANIFEST="$ROOT/crates/atm/Cargo.toml"

declare -a matches=()

while IFS= read -r match; do
  [[ -z "$match" ]] && continue
  matches+=("$match")
done < <(grep -nH -E '^[[:space:]]*agent-team-mail-(daemon|ci-monitor)(\.workspace|[[:space:]]*=)' "$ATM_MANIFEST" || true)

while IFS= read -r match; do
  [[ -z "$match" ]] && continue
  matches+=("$match")
done < <(grep -RInE 'agent_team_mail_daemon::plugins::|agent_team_mail_ci_monitor::' "$ROOT/crates/atm/src" --include='*.rs' || true)

fail=0
for match in "${matches[@]-}"; do
  [[ -z "$match" ]] && continue
  rel="${match#"$ROOT"/}"
  file="${rel%%:*}"
  rest="${rel#*:}"
  line="${rest%%:*}"
  echo "::error file=$file,line=$line::CLI-BOUNDARY violation: $rel"
  fail=1
done

if [[ "$fail" -ne 0 ]]; then
  exit 1
fi

echo "CLI-BOUNDARY check passed."
