#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"

declare -a matches=()

while IFS= read -r match; do
  [[ -z "$match" ]] && continue
  if [[ "$match" == crates/atm-daemon/src/plugins/ci_monitor/github_provider.rs:* ]]; then
    continue
  fi
  if [[ "$match" == crates/atm-daemon/src/plugins/ci_monitor/gh_command_routing.rs:* ]]; then
    continue
  fi
  matches+=("$match")
done < <(grep -RInE 'Command::new\("gh"\)' "$ROOT/crates" --exclude='*.md' | cut -d: -f1,2 | sed "s#^$ROOT/##" || true)

while IFS= read -r match; do
  [[ -z "$match" ]] && continue
  matches+=("$match")
done < <(grep -nH -E 'gh api rate_limit|gh pr list --state open|gh run list --limit' "$ROOT/scripts/dev-daemon-smoke.py" | cut -d: -f1,2 | sed "s#^$ROOT/##" || true)

while IFS= read -r match; do
  [[ -z "$match" ]] && continue
  matches+=("$match")
done < <(grep -nH -E '^[[:space:]]*agent-team-mail-ci-monitor(\.workspace|[[:space:]]*=)' "$ROOT/crates/atm-core/Cargo.toml" | cut -d: -f1,2 | sed "s#^$ROOT/##" || true)

while IFS= read -r match; do
  [[ -z "$match" ]] && continue
  if [[ "$match" == crates/atm-daemon-launch/* ]]; then
    continue
  fi
  matches+=("$match")
done < <(grep -RInE 'Command::new\("atm-daemon"\)' "$ROOT/crates" --include='*.rs' | cut -d: -f1,2 | sed "s#^$ROOT/##" || true)

fail=0
for match in "${matches[@]-}"; do
  [[ -z "$match" ]] && continue
  file="${match%%:*}"
  rest="${match#*:}"
  line="${rest%%:*}"
  echo "::error file=$file,line=$line::ARCH-BOUNDARY-001 violation: untracked GitHub-specific boundary breach at $match"
  fail=1
done

if [[ "$fail" -ne 0 ]]; then
  exit 1
fi

echo "ARCH-BOUNDARY-001 check passed (zero remaining violations)."
