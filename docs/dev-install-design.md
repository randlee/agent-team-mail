# Dev Install Design

Status: proposed

## Summary

We want a team-wide development runtime for ATM that avoids mixed binary
versions, isolates daemon/runtime state from the published Homebrew install, and
lets us atomically switch the whole team to a new dogfood build before release.

The design is:

- keep published/Homebrew binaries as the `stable` channel
- run day-to-day team work from a dedicated `dev` channel
- install each `dev` build into an immutable versioned prefix
- switch the whole team by repointing one `current` symlink
- always launch `dev` sessions with a dedicated `ATM_HOME`
- always force daemon autostart to use the matching `atm-daemon` via
  `ATM_DAEMON_BIN`

This gives us a controlled pre-release environment now, and a clean foundation
for future "other computer" simulation by running a second isolated environment
such as `release-sim`.

## Goals

- Prevent mixed `atm` / `atm-daemon` / `atm-agent-mcp` / `atm-tui` versions.
- Let the whole team switch to a new dogfood build in one operation.
- Keep `dev` runtime state isolated from the published install.
- Make rollback to the prior dogfood build trivial.
- Support future multi-environment testing such as `dev` talking to a simulated
  `release` environment.

## Non-Goals

- Replacing Homebrew as the release distribution channel.
- Installing library crates directly. This design is for binary crates only.
- Supporting arbitrary per-user PATH overrides inside the `dev` channel.

## Required Invariants

The `dev` channel is only valid if all of these are true:

1. `PATH` resolves ATM binaries from the active `dev` prefix first.
2. `ATM_DAEMON_BIN` points to the matching `atm-daemon` in that same prefix.
3. `ATM_HOME` points to the `dev` runtime home, not the normal user runtime.
4. Team sessions are launched through the `dev` environment wrapper every time.
5. Switching the active version is followed by a daemon restart.

If any of these drift, mixed-version behavior is possible again.

## Binary Set

The `dev` install should manage these binaries:

- `atm`
- `atm-daemon`
- `atm-agent-mcp`
- `atm-tui`
- `sc-compose`

Do not install test-only binaries such as `echo-mcp-server`.

## Filesystem Layout

Recommended root:

```text
~/.local/atm-dev/
  installs/
    2026-03-10-6ed6420/
      bin/
        atm
        atm-daemon
        atm-agent-mcp
        atm-tui
        sc-compose
      manifest.json
      source.txt
  current -> installs/2026-03-10-6ed6420
  previous -> installs/2026-03-09-abc1234
  env.sh
  use
```

Recommended runtime home:

```text
~/.local/share/atm-dev/home/
```

Rationale:

- `installs/<version>` is immutable once created
- `current` is the only pointer the team should execute from
- `previous` makes rollback cheap
- `ATM_HOME` lives outside the install prefix so runtime data survives version
  switches

## Environment Contract

Every `dev` shell/session should export:

```bash
export ATM_DEV_ROOT="$HOME/.local/atm-dev"
export ATM_HOME="$HOME/.local/share/atm-dev/home"
export PATH="$ATM_DEV_ROOT/current/bin:$PATH"
export ATM_DAEMON_BIN="$ATM_DEV_ROOT/current/bin/atm-daemon"
```

Optional convenience variable:

```bash
export ATM_CHANNEL=dev
```

The important part is that both CLI execution and daemon autostart resolve from
the same `current/bin`.

## Team Operation Model

Normal flow:

1. Homebrew stays installed for `stable`.
2. Team work runs from the `dev` environment, not from Homebrew.
3. When a sprint is ready for dogfood, we build a new immutable `dev` install.
4. We atomically switch `current` to that install.
5. We restart the `dev` daemon.
6. Team sessions resume under the same `ATM_HOME` and the new binaries.

After publish:

1. Update Homebrew.
2. Optionally validate `stable`.
3. Keep the team on `dev` until we intentionally promote or reset the channel.

## `dev-install` Script Contract

The `dev-install` script should:

1. Resolve the repo root.
2. Compute an install label.
   Default: `<YYYY-MM-DD>-<short-sha>`
3. Build the binary set from source.
4. Create a new immutable install prefix under `installs/<label>`.
5. Copy the built binaries into `<prefix>/bin/`.
6. Write a manifest describing what was built.
7. Move the old `current` to `previous` if present.
8. Atomically repoint `current` to the new prefix.
9. Restart the `dev` daemon using the `dev` environment.
10. Run post-install verification commands.
11. Print the active version and verification summary.

The script must fail fast before switching `current` if the build or copy step
fails.

## Build Strategy

Preferred approach:

```bash
cargo build --release \
  -p agent-team-mail \
  -p agent-team-mail-daemon \
  -p agent-team-mail-mcp \
  -p agent-team-mail-tui \
  -p sc-compose
```

Then copy:

```bash
target/release/atm
target/release/atm-daemon
target/release/atm-agent-mcp
target/release/atm-tui
target/release/sc-compose
```

Reasons to prefer `cargo build --release` plus copy over `cargo install --path`:

- one workspace build graph
- easier to inspect outputs before install
- easier to attach repo metadata to the install
- fewer surprises from cargo-install metadata/state

## Manifest Requirements

Each install should include `manifest.json` with at least:

- install label
- git commit SHA
- git branch
- build timestamp
- dirty/not-dirty flag
- Rust toolchain version
- binary list with filenames and checksums

`source.txt` should record the source repo path used to build the install.

## Atomic Switching

Switching active versions must be atomic at the symlink level.

Required behavior:

1. Build into a new prefix.
2. Verify binaries exist.
3. Update `previous`.
4. Replace `current` in one operation.
5. Restart daemon only after `current` points at the new prefix.

Do not mutate binaries in place inside `current/bin`.

## Daemon Restart Rules

After a successful install switch:

```bash
ATM_HOME="$HOME/.local/share/atm-dev/home" \
ATM_DAEMON_BIN="$HOME/.local/atm-dev/current/bin/atm-daemon" \
PATH="$HOME/.local/atm-dev/current/bin:$PATH" \
atm daemon restart
```

This restart should be part of `dev-install`; it should not be a manual
follow-up step.

## Post-Install Verification

Minimum verification after switching:

```bash
atm daemon status
atm status
atm doctor
atm --version
atm-daemon --version
atm-agent-mcp --version
atm-tui --version
sc-compose --version
```

If any verification step fails, the script should report failure clearly and
offer rollback guidance.

## Rollback

Rollback should be simple:

1. repoint `current` to `previous`
2. restart daemon in the same `ATM_HOME`
3. verify health again

That implies `previous` should be maintained automatically by `dev-install`.

## Suggested Companion Scripts

`dev-install`
- build a new versioned install
- switch `current`
- restart daemon
- verify

`dev-use <label>`
- repoint `current` to an existing install
- restart daemon
- verify

`dev-shell`
- print or source the canonical `dev` environment exports

`dev-status`
- show current install label, manifest details, daemon PID, and `ATM_HOME`

## Future: Release Simulation / "Other Computer"

When we begin testing cross-computer messaging, we should add a second isolated
environment, for example:

- `dev`
- `release-sim`

Each environment should have its own:

- install root
- `current` symlink
- `ATM_HOME`
- daemon runtime
- logical host identity

Example:

```text
~/.local/atm-envs/dev/
~/.local/atm-envs/release-sim/
```

This lets us simulate:

- new dogfood binaries talking to older published binaries
- bridge/sync behavior between isolated environments
- staged rollout compatibility problems before using real second machines

If network transport is added later, `release-sim` can bind a different local
port while still remaining on the same workstation.

## Open Implementation Choices

These are still design choices, not final requirements:

- script location: `scripts/dev-install` vs `tools/dev-install`
- shell implementation: POSIX shell vs Python
- manifest format: JSON only vs JSON + human-readable text
- retention policy for old installs
- whether `dev-install` should refuse dirty worktrees by default

Recommended defaults:

- use a script under `scripts/`
- refuse dirty worktrees unless `--allow-dirty` is passed
- keep the last 5 installs by default
- write JSON manifest plus a short text summary

## Recommended Next Step

Implement two scripts first:

1. `scripts/dev-install`
2. `scripts/dev-use`

That is enough to validate the model before adding extra helpers.
