#!/usr/bin/env bash
# Deprecated helper preserved for backward compatibility.
# Codex hook wiring is now installed via `atm init <team>`.
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"
ATM_TOML="$REPO_ROOT/.atm.toml"

TEAM="${ATM_TEAM:-}"
if [[ -z "$TEAM" && -f "$ATM_TOML" ]]; then
  TEAM="$(python3 - <<'PY' "$ATM_TOML"
import sys
from pathlib import Path
path = Path(sys.argv[1])
try:
    import tomllib
except ImportError:
    import tomli as tomllib  # type: ignore
cfg = tomllib.loads(path.read_text())
print((cfg.get('core', {}) or {}).get('default_team', ''))
PY
)"
fi

if [[ -z "$TEAM" ]]; then
  echo "Error: Could not resolve team name. Set ATM_TEAM or create .atm.toml with [core].default_team." >&2
  exit 1
fi

echo "[DEPRECATED] setup-codex-hooks.sh now delegates to 'atm init'."
exec atm init "$TEAM"
