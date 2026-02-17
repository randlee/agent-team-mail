# agent-team-mail (`atm`)

Mail-like messaging for Claude agent teams.

`atm` is a Rust CLI and library for sending and receiving messages between Claude agents in a team. It provides atomic file I/O operations over the `~/.claude/teams/` directory structure with conflict detection, guaranteed delivery, and schema versioning.

## Features

- **Simple messaging**: Send messages to agents, broadcast to teams, read your inbox
- **Atomic operations**: Safe concurrent access with platform-specific atomic file swaps
- **Conflict detection**: Hash-based detection and automatic merge of concurrent writes
- **Guaranteed delivery**: Outbound spool with retry logic ensures messages aren't lost
- **Schema versioning**: Forward-compatible with unknown JSON fields preserved on round-trip
- **Cross-platform**: Works on macOS, Linux, and Windows

## Installation

### Pre-built Binaries (GitHub Releases)

Download the latest release for your platform from [GitHub Releases](https://github.com/randlee/agent-team-mail/releases):

| Platform | Archive |
|----------|---------|
| Linux (x86_64) | `atm_<version>_x86_64-unknown-linux-gnu.tar.gz` |
| macOS (Intel) | `atm_<version>_x86_64-apple-darwin.tar.gz` |
| macOS (Apple Silicon) | `atm_<version>_aarch64-apple-darwin.tar.gz` |
| Windows (x86_64) | `atm_<version>_x86_64-pc-windows-msvc.zip` |

Extract and place `atm` (and optionally `atm-daemon`) somewhere in your `$PATH`.

### Homebrew (macOS/Linux)

```bash
brew tap randlee/tap
brew install agent-team-mail
```

### crates.io

```bash
# Install the CLI
cargo install agent-team-mail

# Install the daemon (optional)
cargo install agent-team-mail-daemon
```

### Build from Source

```bash
git clone https://github.com/randlee/agent-team-mail.git
cd agent-team-mail
cargo install --path crates/atm
# Optionally install the daemon:
cargo install --path crates/atm-daemon
```

The `atm` (and `atm-daemon`) binaries will be available in your `$PATH`.

## Quick Start

### Send a message

```bash
# Send to an agent on the default team
atm send agent-name "Hello from the terminal"

# Send to an agent on a specific team
atm send agent-name@team-name "Cross-team message"

# Send with explicit summary
atm send agent-name "Important update" --summary "Deploy notification"
```

### Read your inbox

```bash
# Read unread messages (marks them as read)
atm read

# Read all messages without marking as read
atm read --all --no-mark

# Read someone else's inbox
atm read other-agent
```

### Broadcast to a team

```bash
# Broadcast to all members of the default team
atm broadcast "Team-wide announcement"

# Broadcast to a specific team
atm broadcast --team backend-ci "CI pipeline updated"
```

### Check team status

```bash
# Show overview of default team
atm status

# List all teams
atm teams

# List members of a team
atm members backend-ci
```

## Commands

| Command | Description |
|---------|-------------|
| `send` | Send a message to a specific agent |
| `broadcast` | Send a message to all agents in a team |
| `read` | Read messages from an inbox |
| `inbox` | Show inbox summary (message counts) |
| `teams` | List all teams on this machine |
| `members` | List agents in a team |
| `status` | Show team status overview |
| `config` | Show effective configuration |

Run `atm <command> --help` for detailed usage of each command.

## Configuration

`atm` resolves configuration from multiple sources (highest priority first):

1. **Command-line flags** (`--team`, `--identity`)
2. **Environment variables** (`ATM_TEAM`, `ATM_IDENTITY`, `ATM_NO_COLOR`)
3. **Repo-local config** (`.atm.toml` in current directory or git root)
4. **Global config** (`~/.config/atm/config.toml`)
5. **Defaults**

### Example `.atm.toml`

```toml
[core]
default_team = "backend-ci-team"
identity = "human"

[display]
format = "text"  # text | json
color = true
timestamps = "relative"  # relative | absolute | iso8601
```

### Environment Variables

- `ATM_TEAM` — Default team name
- `ATM_IDENTITY` — Sender identity for messages
- `ATM_CONFIG` — Path to config file override
- `ATM_NO_COLOR` — Disable colored output
- `ATM_HOME` — Override home directory (mainly for testing)

## Architecture

The project is organized as a Cargo workspace with three crates:

```
agent-team-mail/
├── crates/
│   ├── atm-core/    # Shared library (schema types, atomic I/O, config)
│   ├── atm/         # CLI binary (commands, output formatting)
│   └── atm-daemon/  # Daemon with plugin system (post-MVP)
```

### `atm-core`

Shared library providing:
- **Schema types** with versioning (`TeamConfig`, `InboxMessage`, `TaskItem`, `SettingsJson`)
- **Atomic file I/O** with platform-specific swaps (`renamex_np` on macOS, `renameat2` on Linux)
- **Conflict detection** via content hashing (BLAKE3)
- **File locking** (`flock`) for coordination between `atm` processes
- **Outbound spool** for guaranteed delivery with retry logic
- **Config resolution** from multiple sources

### `atm`

CLI binary with subcommands for:
- Messaging (`send`, `broadcast`, `read`, `inbox`)
- Discovery (`teams`, `members`, `status`)
- Configuration (`config`)

### `atm-daemon` (post-MVP)

Always-on daemon with plugin system for:
- **Issues plugin**: Bridge GitHub/Azure DevOps issues to agent inboxes
- **CI Monitor plugin**: Watch CI workflows and notify agents of failures
- **Bridge plugin**: Enable cross-machine agent teams
- **Human Chat plugin**: Connect chat apps (Slack, Discord) to agent teams

## Development

See [`docs/project-plan.md`](docs/project-plan.md) for the development workflow and sprint plan.

### Running Tests

```bash
# Run all tests
cargo test --workspace

# Run clippy
cargo clippy --workspace -- -D warnings

# Generate documentation
cargo doc --no-deps --workspace --open
```

### Cross-Platform Testing

The CI runs tests on macOS, Linux, and Windows. See [`docs/cross-platform-guidelines.md`](docs/cross-platform-guidelines.md) for platform-specific patterns (especially Windows home directory handling).

## Documentation

- [`docs/requirements.md`](docs/requirements.md) — System requirements and architecture
- [`docs/project-plan.md`](docs/project-plan.md) — Sprint plan and development workflow
- [`docs/agent-team-api.md`](docs/agent-team-api.md) — Claude agent team file-based API reference
- [`docs/cross-platform-guidelines.md`](docs/cross-platform-guidelines.md) — Windows CI compliance patterns

## License

Licensed under either of:

- Apache License, Version 2.0 ([LICENSE-APACHE](LICENSE-APACHE) or http://www.apache.org/licenses/LICENSE-2.0)
- MIT License ([LICENSE-MIT](LICENSE-MIT) or http://opensource.org/licenses/MIT)

at your option.

## Contributing

Contributions are welcome! See [`docs/project-plan.md`](docs/project-plan.md) for the development workflow.

Unless you explicitly state otherwise, any contribution intentionally submitted for inclusion in the work by you, as defined in the Apache-2.0 license, shall be dual licensed as above, without any additional terms or conditions.
