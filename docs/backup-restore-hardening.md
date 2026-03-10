# Backup And Restore Hardening

## Scope

This fix branch hardens team backup and restore around three concrete failure modes:

1. Claude Code project task lists under `~/.claude/tasks/<project>/` were not captured.
2. Restored task directories could keep a stale `.highwatermark`.
3. Restore preserved runtime-only member state that should be cleared.

## Decisions

- `atm teams backup --project <name>` copies `~/.claude/tasks/<project>/` into `tasks-cc/`.
- `atm teams restore --project <name>` restores `tasks-cc/` back into the same project task scope.
- Restore recomputes `.highwatermark` from the highest numeric `<task-id>.json` file in each restored task directory.
- If no numeric task ids are present after restore, `.highwatermark` is set to `0`.
- Restored members are reintroduced as inactive and detached from runtime session state:
  `is_active=false`, `last_active=None`, `session_id=None`, `tmux_pane_id=None`.

## Backup Naming

Backup directories use `YYYYMMDDTHHMMSSfffffffffZ`.

Reason:
- preserves lexicographic time ordering
- avoids same-second collisions during automatic handoff backups

## Validation

- unit tests cover project-task backup, project-task restore, missing-project backup, and highwatermark recomputation
- `resume --project <name>` is covered so the automatic handoff path preserves `tasks-cc/`
