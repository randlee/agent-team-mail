# Highwatermark Fix

## Problem

Task restore previously preserved `.highwatermark` from backup, which could be lower or otherwise stale relative to the restored task files.

## Required Behavior

After restore, ATM scans restored `*.json` task files and computes:

- max numeric file stem => new `.highwatermark`
- no numeric file stems => `.highwatermark = 0`

Examples:

- `76.json`, `82.json` => `.highwatermark = 82`
- only `.lock` and prior `.highwatermark` => `.highwatermark = 0`

## Scope

This recomputation applies to:

- team task restore into `~/.claude/tasks/<team>/`
- project task restore into `~/.claude/tasks/<project>/` when `--project <name>` is used

## Reasoning

The filename set is the canonical recovered state after restore. Recomputing from filenames avoids carrying forward stale counters from the backup source.
