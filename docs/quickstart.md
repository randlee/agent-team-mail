# ATM Quickstart

This guide covers the minimum setup to start using `atm` and the required
upgrade behavior for daemon version safety.

## Install

Homebrew:

```bash
brew tap randlee/tap
brew install agent-team-mail
```

Cargo:

```bash
cargo install agent-team-mail --locked
```

## First Run

Initialize hooks and team defaults in the current project:

```bash
atm init <team-name>
```

Then verify messaging:

```bash
atm send <agent> "ping" --team <team-name>
atm read --team <team-name> --timeout 60
```

## Upgrading

Homebrew upgrade:

```bash
brew upgrade randlee/tap/agent-team-mail
```

The formula post-install step terminates any stale `atm-daemon` process
automatically (`pkill -x atm-daemon || true`), so the next `atm` command starts
the upgraded daemon binary.

Cargo/manual binary upgrade:

```bash
pkill -x atm-daemon || true
```

Run the command after installing new binaries so a stale daemon process does
not keep serving the old version. The daemon auto-starts on the next daemon-
backed `atm` invocation.

Note: there is currently no dedicated `atm daemon stop` command; use the
`pkill` approach above for explicit manual restart during upgrades.
