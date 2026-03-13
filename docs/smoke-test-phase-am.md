# Phase AM Smoke Test Report

- Version: `0.44.9`
- Develop SHA: `88877bf`
- Date: `2026-03-13`
- Build source: `/tmp/agent-team-mail-smoke-88877bf`
- Isolated runtime: `/tmp/atm-smoke-test`
- Install milestone: `0.44.9-dev.1`

## Results

| Area | Test | Result | Notes |
|---|---|---|---|
| Build/install | `cargo build --release --workspace` | PASS | Built cleanly from `88877bf` in a temp worktree. |
| Build/install | `python3 scripts/dev-install` | PASS | Installed to `/tmp/atm-smoke-test/bin`; post-install verification all passed. |
| Core messaging | `atm send` + `atm read` roundtrip | PASS | `smoke-lead -> smoke-peer` delivered and read successfully in isolated `smoke-am` team. |
| Core messaging | `atm inbox` summary | PASS | `atm inbox --team smoke-am` reflected pending counts and latest timestamps correctly. |
| Core messaging | `atm broadcast` | PASS | Broadcast delivered to all 3 members; verified on `smoke-third`. |
| Daemon | `atm-daemon` start | PASS | Started cleanly during isolated `dev-install`; daemon reported `0.44.9`. |
| Daemon | `atm-daemon` status / health check | FAIL | `atm doctor --json` reported multiple warnings in isolated runtime: hook audit missing scripts/config under `ATM_HOME`, `ACTIVE_FLAG_STALE`, and `degraded_spooling`. |
| Daemon | `atm-daemon` stop / clean shutdown | FAIL | `atm daemon stop` timed out after SIGTERM and reported manual `kill -9` guidance. Recovery required stale-PID cleanup path in `atm daemon restart`. |
| CI monitor | `atm gh status --json` against isolated `smoke-am` team | FAIL | Reported `disabled_config_error` because repo config targets `atm-dev`, not the isolated `smoke-am` team. |
| CI monitor | `atm gh status --json` against isolated `atm-dev` team | PASS | After creating isolated `atm-dev`, `gh_monitor` reported `lifecycle_state=running`, `availability_state=healthy`. |
| CI monitor | `atm gh pr report 727 --json` | PASS | Returned full one-shot JSON snapshot for PR `#727`, including checks/review/merge state. |
| Team config writes | Team create path (`create_or_update`) | PASS | `atm init smoke-am --local` created team config cleanly in isolated runtime. |
| Team config writes | `atm teams create` exact command | FAIL | Command is not present in the CLI (`unrecognized subcommand 'create'`). Smoke checklist should be updated to use the actual create path. |
| Team config writes | `atm teams add-member` (`update`) | PASS | `smoke-peer` and `smoke-third` were added successfully. |
| Team config writes | No `config.json` corruption under normal use | PASS | `/tmp/atm-smoke-test/.claude/teams/smoke-am/config.json` remained valid JSON after create/add-member/send/broadcast. |
| `atm gh` commands | `atm gh status` structured output | PASS | JSON output was structured and actionable in both failure and healthy cases. |
| Regression check | No regressions vs `0.44.9` baseline | FAIL | Clean isolated stop-path still fails; isolated doctor hook audit/path expectations are not compatible with `atm init --local`; logging still reports `degraded_spooling`. |

## Summary

Overall result: **FAIL**

The Phase AM build/install path is usable: release build, isolated `dev-install`,
core messaging, broadcast, inbox summary, and one-shot GH reporting all worked.
However, the smoke test did not clear because the isolated daemon/runtime health
surfaces are still noisy and `atm daemon stop` did not complete cleanly.

## Issues Found

1. `atm daemon stop` still times out after SIGTERM in isolated runtime.
   Recovery only succeeded because `atm daemon restart` cleaned up stale runtime
   files once the original PID was gone.

2. Isolated `atm doctor --json` is not clean under `ATM_HOME=/tmp/atm-smoke-test`.
   Hook audit expects scripts/config under `${ATM_HOME}/.claude/...`, but
   `atm init --local` installs Claude hooks into the repo-local `.claude` tree
   instead. This produces many `HOOK_SCRIPT_MISSING` / `HOOK_CONFIG_MISSING`
   warnings even though local hook installation succeeded.

3. Isolated logging still reports `degraded_spooling` immediately after startup.

4. The smoke checklist says `atm teams create`, but that command is not present
   in this CLI build. The equivalent create path exercised here was
   `atm init <team> --local`.

## Recommended Action

**Hold before Phase AN kickoff** until the daemon stop-path and isolated hook
audit expectations are clarified or fixed.

At minimum:
- fix or document the isolated `atm daemon stop` timeout behavior
- reconcile `atm doctor` hook-audit expectations with `atm init --local`
  under redirected `ATM_HOME`
- update the smoke checklist to use the actual team-create path
