# Daemon Status Schema Migration

Phase BB changes the daemon runtime metadata written under `status.json` and
related runtime files to support the single-daemon model. The affected
artifacts are:
- `status.json`
- `runtime.json`
- `daemon.lock.meta.json`

## Compatibility Note

- `RuntimeOwnerMetadata.runtime_kind` and `RuntimeMetadata.runtime_kind` now use
  the collapsed `RuntimeKind` enum:
  - `shared`
  - `isolated`
- Legacy serialized values `release` and `dev` are still accepted as aliases
  for `shared` during deserialization.
- Post-BB.3 daemons continue to read pre-BB.3 `status.json` files by
  deserializing legacy runtime-kind values into the collapsed enum; older
  files remain readable until they are naturally rewritten by the new daemon.

## Migration Rule

- New writers must emit only `shared` or `isolated`.
- Readers must continue to accept legacy `release` and `dev` values until all
  existing runtime metadata has been rotated out.
- Fields that may be absent in older serialized forms MUST use
  `#[serde(default)]` (or an equivalent explicit defaulting strategy) so
  backward-compatible reads succeed without hand-authored repair.
- No new code should branch on separate release/dev daemon classes; build
  profile remains independent from runtime ownership.
