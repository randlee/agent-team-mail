#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
SELF_REL="scripts/ci/gh_boundary_check.sh"

declare -a matches=()

# This gate catches the concrete raw-gh execution patterns we expect to regress:
# direct std::process::Command::new("gh") and the AT.6-confirmed
# tokio::process::Command::new("gh") variant in Rust crates, script-level raw
# gh invocations under scripts/, and the forbidden atm-core -> atm-ci-monitor dep.
# It does not attempt full static analysis for variable-based launches,
# shell indirection outside scripts/, or non-Rust helper ecosystems.
while IFS= read -r match; do
  [[ -z "$match" ]] && continue
  if [[ "$match" == crates/atm-daemon/src/plugins/ci_monitor/github_provider.rs:* ]]; then
    continue
  fi
  if [[ "$match" == crates/atm-daemon/src/plugins/ci_monitor/gh_command_routing.rs:* ]]; then
    continue
  fi
  matches+=("$match")
done < <(grep -RInE '((std|tokio)::process::)?Command::new\("gh"\)' "$ROOT/crates" --exclude='*.md' | cut -d: -f1,2 | sed "s#^$ROOT/##" || true)

while IFS= read -r match; do
  [[ -z "$match" ]] && continue
  if [[ "$match" == "$SELF_REL:"* ]]; then
    continue
  fi
  matches+=("$match")
done < <(find "$ROOT/scripts" -type f \( -name '*.sh' -o -name '*.py' \) -print0 | xargs -0 grep -nHE '(^|[^[:alnum:]_])gh[[:space:]]+(api|auth|issue|pr|run|version)\b|["'"'"']gh["'"'"']' | cut -d: -f1,2 | sed "s#^$ROOT/##" || true)

while IFS= read -r match; do
  [[ -z "$match" ]] && continue
  matches+=("$match")
done < <(grep -nH -E '^[[:space:]]*agent-team-mail-ci-monitor(\.workspace|[[:space:]]*=)' "$ROOT/crates/atm-core/Cargo.toml" | cut -d: -f1,2 | sed "s#^$ROOT/##" || true)

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
