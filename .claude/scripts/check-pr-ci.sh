#!/bin/bash
# Check if all CI checks are complete on a PR
# Returns 0 (all complete) or number of incomplete checks

PR_NUM=$1
if [ -z "$PR_NUM" ]; then
  echo "Usage: $0 <pr-number>" >&2
  exit 1
fi

# Get status check rollup and count incomplete checks
gh pr view "$PR_NUM" --json statusCheckRollup --jq '[.statusCheckRollup[] | select(.status != "COMPLETED")] | length'
