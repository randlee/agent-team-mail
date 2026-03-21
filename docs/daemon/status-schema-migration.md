# Daemon Status Schema Migration

Phase BB changes the daemon runtime metadata written under `status.json` and
related runtime files to support the single-daemon model.

## Compatibility Note

- `RuntimeOwnerMetadata.runtime_kind` and `RuntimeMetadata.runtime_kind` now use
  the collapsed `RuntimeKind` enum:
  - `shared`
  - `isolated`
- Legacy serialized values `release` and `dev` are still accepted as aliases
  for `shared` during deserialization.

## Migration Rule

- New writers must emit only `shared` or `isolated`.
- Readers must continue to accept legacy `release` and `dev` values until all
  existing runtime metadata has been rotated out.
- No new code should branch on separate release/dev daemon classes; build
  profile remains independent from runtime ownership.
