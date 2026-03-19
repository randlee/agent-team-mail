#!/usr/bin/env bash
set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
cd "$repo_root"

python3 scripts/release_artifacts.py validate-publish-order \
  --manifest release/publish-artifacts.toml \
  --workspace-toml Cargo.toml
