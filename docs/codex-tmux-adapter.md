# Codex TMUX Worker Adapter — Final Architecture

## Overview

The Worker Adapter plugin enables daemon-managed async agent teammates that can receive messages and respond without blocking the user's terminal. Agents run in isolated tmux panes with full lifecycle management, health monitoring, and automatic restart capabilities.

## Architecture

### Components

1. **WorkerAdapter Trait** (`trait_def.rs`)
   - Abstract interface for worker backends
   - Methods: `spawn()`, `send_message()`, `shutdown()`
   - Returns `WorkerHandle` with agent ID, pane ID, and log file path

2. **Codex TMUX Backend** (`codex_tmux.rs`)
   - Production backend using tmux for process isolation
   - Creates dedicated tmux panes for each agent
   - Uses `tmux send-keys -l` (literal mode) to prevent command injection
   - Manages log files for response capture

3. **Mock Backend** (`mock_backend.rs`)
   - Test-only backend for CI/CD without tmux
   - Records all operations for verification
   - Supports error injection for testing failure scenarios
   - Works on all platforms including Windows

4. **Message Router** (`router.rs`)
   - Concurrency control with three policies: queue, reject, concurrent
   - Per-agent message queuing
   - Prevents duplicate work and race conditions

5. **Response Capture** (`capture.rs`)
   - Log file tailing with marker-based correlation
   - Captures worker responses for inbox delivery
   - Configurable timeout and polling interval

6. **Activity Tracker** (`activity.rs`)
   - Monitors agent heartbeats via roster updates
   - Marks inactive agents as offline
   - Configurable inactivity timeout (default: 5 minutes)

7. **Lifecycle Manager** (`lifecycle.rs`)
   - Worker health checks via tmux pane validation
   - Automatic restart with exponential backoff
   - Graceful shutdown with timeout
   - Log file rotation (10 MB threshold)

8. **Plugin** (`plugin.rs`)
   - Integrates all components
   - Periodic health checks (default: 30 seconds)
   - Auto-starts configured workers on daemon init
   - Graceful shutdown of all workers on daemon exit

### Data Flow

```
┌──────────────┐
│ Inbox Event  │
│  (new msg)   │
└──────┬───────┘
       │
       ▼
┌─────────────────┐
│ Message Router  │  ← Concurrency control
│  - queue/reject │
│  - concurrent   │
└──────┬──────────┘
       │
       ▼
┌─────────────────┐
│ WorkerAdapter   │
│   spawn() if    │
│   not running   │
└──────┬──────────┘
       │
       ▼
┌─────────────────┐
│ send_message()  │  ← Format with prompt template
└──────┬──────────┘
       │
       ▼
┌─────────────────┐
│ TMUX Pane       │
│  (Codex agent)  │
│                 │
│  Output → Log   │
└──────┬──────────┘
       │
       ▼
┌─────────────────┐
│ Response        │
│  Capture        │  ← Tail log file
└──────┬──────────┘
       │
       ▼
┌─────────────────┐
│ Write response  │
│ to sender inbox │
└─────────────────┘
```

### Health Monitoring Flow

```
┌─────────────────────┐
│ Health Check Timer  │  ← Every 30 seconds (configurable)
│ (periodic interval) │
└──────────┬──────────┘
           │
           ▼
┌──────────────────────┐
│ For each worker:     │
│   Check tmux pane    │
│   exists & running   │
└──────────┬───────────┘
           │
           ├─ Healthy ──► Update health timestamp
           │
           └─ Crashed ──► Restart with backoff
                          (max 3 attempts, 5s backoff)
```

## Configuration

### Machine-Level Config (`~/.config/atm/daemon.toml`)

```toml
[workers]
enabled = true
backend = "codex-tmux"
tmux_session = "atm-workers"
log_dir = "~/.config/atm/worker-logs"

# Lifecycle settings
inactivity_timeout_ms = 300000        # 5 minutes
health_check_interval_secs = 30       # 30 seconds
max_restart_attempts = 3              # Max restarts before giving up
restart_backoff_secs = 5              # Delay between restart attempts
shutdown_timeout_secs = 10            # Graceful shutdown timeout

# Per-agent configuration
[workers.agents."arch-ctm@atm-planning"]
enabled = true
prompt_template = "{message}"
concurrency_policy = "queue"          # "queue" | "reject" | "concurrent"

[workers.agents."dev-agent@my-team"]
enabled = true
concurrency_policy = "reject"
```

### Repo-Level Config (`./.atm/config.toml`)

Repo-level config can override agent-specific settings:

```toml
[workers.agents."arch-ctm@atm-planning"]
enabled = false  # Disable in this repo
```

## Configuration Validation

The plugin validates all configuration at init time:

### Backend Validation
- Only `"codex-tmux"` is supported currently
- Future backends: SSH, Docker, WebSocket

### TMUX Session Validation
- Session name cannot be empty
- Cannot contain `:` or `.` (tmux restrictions)

### Agent Name Validation
- Cannot be empty
- Cannot contain newlines

### Concurrency Policy Validation
- Must be one of: `"queue"`, `"reject"`, `"concurrent"`

Invalid configuration causes `PluginError::Config` at init time.

## Safety & Security

### Command Injection Prevention

**CRITICAL**: All `tmux send-keys` calls use `-l` (literal mode) to prevent shell interpretation:

```rust
Command::new("tmux")
    .arg("send-keys")
    .arg("-t")
    .arg(&pane_id)
    .arg("-l")              // LITERAL MODE - mandatory
    .arg(user_message)      // Safe: not interpreted as commands
    .output()
```

**Without `-l`**: User message `"rm -rf /" && echo "pwned"` would execute as shell commands.

**With `-l`**: Message is sent as literal text to the agent.

### Process Isolation

- Each agent runs in a separate tmux pane
- No shared state between workers
- Pane crashes don't affect other workers or daemon
- Log files are per-agent

### Graceful Degradation

- Plugin disables if tmux not available (e.g., Windows)
- Workers restart automatically on crash (up to 3 attempts)
- Failed health checks trigger restart, not panic
- Daemon continues if individual worker fails

## Testing Strategy

### Unit Tests
- Config parsing and validation
- Mock backend operations
- Message routing logic
- Activity tracking

### Integration Tests
- Full daemon → mock backend → response cycle
- Config validation (reject invalid configs)
- Error injection (spawn, send, shutdown failures)
- Mock response writing and capture

### Platform-Specific Tests
- Real tmux tests only on macOS/Linux
- Skip gracefully on Windows CI
- Mock backend used for cross-platform CI

### CI Compliance
- Uses `ATM_HOME` env var for all tests
- Never uses `HOME` or `USERPROFILE` directly
- Runs `cargo clippy -- -D warnings` (rust-1.93 strict lints)
- All tests pass on Ubuntu, macOS, Windows

## Error Scenarios

### Worker Crash During Message Processing

1. Health check detects crashed pane
2. Lifecycle manager marks worker as `Crashed`
3. Automatic restart initiated with backoff
4. If max attempts exceeded, agent marked `Failed`
5. User can manually restart via daemon command

### Log File Missing

1. Response capture fails with `std::io::Error`
2. Router marks agent as finished (no deadlock)
3. Error logged, no response delivered
4. Worker remains running, next message retries

### TMUX Not Available

1. `spawn()` returns `PluginError::Runtime`
2. Plugin logs warning
3. Daemon continues without worker adapter
4. On Windows CI: tests skip gracefully

### Invalid Configuration

1. `from_toml()` validation fails
2. `PluginError::Config` returned at init
3. Daemon logs error, plugin disabled
4. Other plugins continue normally

## Operational Notes

### Attaching to Workers

View running workers:
```bash
tmux list-windows -t atm-workers
```

Attach to a specific agent:
```bash
tmux attach -t atm-workers:<agent-window-name>
```

Detach without killing:
```
Ctrl+B, D
```

### Log Files

Worker logs are in `~/.config/atm/worker-logs/`:
```
arch-ctm_atm-planning.log
dev-agent_my-team.log
```

View live logs:
```bash
tail -f ~/.config/atm/worker-logs/arch-ctm_atm-planning.log
```

### Manual Restart

Kill a worker pane:
```bash
tmux kill-pane -t atm-workers:<window-name>
```

Daemon will auto-restart on next health check (within 30 seconds).

### Disable a Worker

Edit `daemon.toml`:
```toml
[workers.agents."arch-ctm@atm-planning"]
enabled = false
```

Restart daemon or send `SIGHUP` to reload config.

## Performance Characteristics

### Startup
- Auto-starts all enabled workers on daemon init
- Parallel spawn operations (async)
- Typical startup: < 1 second per worker

### Message Processing
- Queue policy: sequential, no concurrency
- Reject policy: instant rejection if busy
- Concurrent policy: parallel processing (use with caution)

### Health Checks
- Minimal overhead: `tmux list-panes` per agent
- Default interval: 30 seconds
- CPU usage: negligible

### Memory
- Daemon: ~10 MB baseline
- Per worker: ~5 MB (tmux pane overhead)
- Log files: rotated at 10 MB threshold

## Future Enhancements

### Additional Backends
- **SSH Backend**: Workers on remote machines
- **Docker Backend**: Containerized workers
- **WebSocket Backend**: Browser-based workers

### Advanced Features
- **Message priority queues**: Urgent vs normal messages
- **Worker pools**: Multiple workers per agent for load balancing
- **Cross-machine routing**: Bridge to workers on other hosts
- **Response streaming**: Incremental output delivery

### Monitoring & Observability
- **Prometheus metrics**: Worker health, message rates, latency
- **Grafana dashboards**: Real-time worker status
- **Alerting**: Notify on repeated crashes or high queue depth

## References

- [Worker Adapter Plugin Source](../crates/atm-daemon/src/plugins/worker_adapter/)
- [Integration Tests](../crates/atm-daemon/tests/worker_adapter_tests.rs)
- [Project Requirements](./requirements.md)
- [Project Plan - Phase 7](./project-plan.md#9-phase-7-worker-adapter-plugin-codex-tmux-backend)

## Version History

- **0.1.0** (Sprint 7.1): Initial trait and Codex backend
- **0.2.0** (Sprint 7.2): Message routing and response capture
- **0.3.0** (Sprint 7.3): Lifecycle management and health monitoring
- **0.4.0** (Sprint 7.4): Integration tests and config validation

---

**Status**: Complete (Sprint 7.4)
**Maintainer**: ARCH-ATM
**Last Updated**: 2026-02-14
