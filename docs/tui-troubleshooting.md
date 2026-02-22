# ATM TUI Troubleshooting Guide

This guide covers the most common failure modes for `atm-tui` and how to diagnose them.

---

## 1. Daemon Not Running / Degraded State

**Symptom**: The Dashboard panel shows `(no members — daemon may be offline)` and the agent
list remains empty. The status bar may display `[FROZEN]` immediately on startup.

**Cause**: The `atm-daemon` process is not running or the Unix socket is unavailable.

**Resolution**:
1. Verify the daemon is running:
   ```bash
   atm daemon status
   ```
2. If stopped, restart it:
   ```bash
   atm daemon start
   ```
3. Check the daemon log at `~/.claude/teams/<team>/daemon.log` for startup errors.
4. Confirm the Unix socket exists:
   ```bash
   ls -la /tmp/atm-daemon.sock
   ```

---

## 2. Stream Frozen — Log File Missing or Unreadable

**Symptom**: The Agent Terminal panel shows `[FROZEN]` with a message such as
`"stream frozen: log unreadable"` or `"stream reset: log truncated (daemon restart?)"`.

**Causes and checks**:

| Cause | How to check |
|-------|-------------|
| Agent session not started | Verify the agent is `idle` or `busy` in the Dashboard |
| Log file was never created | Session log appears only after the agent produces output |
| Daemon restarted, cleared the log | A truncation event resets the stream automatically |
| Permissions changed | `ls -la ~/.claude/teams/<team>/<agent>/session.log` |

**Resolution**:
- Wait: the TUI polls every 100 ms and will recover automatically when the file becomes readable.
- If the log file is permanently missing, try switching to another agent with `↑`/`↓` and back.
- Restart the daemon if the socket is gone.

---

## 3. Interrupt Not Sending

**Symptom**: Pressing `Ctrl-I` has no visible effect, or the status bar does not change.

**Possible reasons**:

1. **Agent is not live** — the selected agent must be in `idle` or `busy` state. The control
   input field at the bottom of the Agent Terminal panel shows a `[disabled]` indicator when
   the agent is not live. States that block interrupts: `launching`, `killed`, `stale`, `closed`.

2. **`interrupt_policy = "never"`** — the interrupt is silently discarded when this policy is
   set in `~/.config/atm/tui.toml`. Change to `"confirm"` or `"always"` to re-enable.

3. **Confirmation dialog is open** — with `interrupt_policy = "confirm"` (the default), `Ctrl-I`
   opens a `"Send interrupt? [y/N]"` prompt. Press `y` or `Enter` to confirm, or `n`/`Esc` to
   cancel. No other keys are accepted while the dialog is open.

4. **Wrong panel focus** — `Ctrl-I` only works when the Agent Terminal panel is focused.
   Press `Tab` to switch focus if the Dashboard is currently active.

---

## 4. Control Ack Timeout

**Symptom**: The status bar shows `"timeout"` after a stdin or interrupt action.

**What happens**: `atm-tui` automatically retries the control request once after
`stdin_timeout_secs / 2` seconds (stdin) or `interrupt_timeout_secs / 2` seconds (interrupt).
If the retry also times out, `"timeout"` is shown and no further retries are attempted.

**Resolution**:
- Check whether the daemon is responsive: `atm daemon status`.
- Increase the per-action timeout in `~/.config/atm/tui.toml`:
  ```toml
  stdin_timeout_secs = 20
  interrupt_timeout_secs = 10
  ```
- Restart the daemon if it appears hung.

---

## 5. Garbled Display / Terminal Rendering Issues

**Symptom**: The TUI displays overlapping text, incomplete borders, or the layout looks
broken on resize.

**Common causes and mitigations**:

| Symptom | Mitigation |
|---------|-----------|
| Terminal too small | Resize to at least 80x24; `atm-tui` does not enforce a minimum size |
| Mouse events interfering | `atm-tui` captures mouse events; some terminal emulators conflict. Try disabling mouse capture by modifying the startup sequence (not yet a CLI flag) |
| Raw mode left active after crash | Run `reset` in the terminal to restore normal state |
| Nesting inside another TUI | Avoid running `atm-tui` inside `tmux` panes that themselves have full-screen apps |

If the terminal is left in raw mode after a crash, restore it:
```bash
reset
# or:
stty sane
```

---

## 6. Config File Location and ATM_HOME Override

The TUI preferences file is loaded from:

```
{home}/.config/atm/tui.toml
```

where `{home}` is resolved in priority order:

1. `ATM_HOME` environment variable (if set and non-empty)
2. Platform default home directory (`$HOME` on Linux/macOS; Windows API on Windows)

**Override example**:
```bash
ATM_HOME=/custom/home atm-tui --team atm-dev
# Loads: /custom/home/.config/atm/tui.toml
```

**Minimal config file**:
```toml
# ~/.config/atm/tui.toml
interrupt_policy = "confirm"   # "always" | "never" | "confirm"
follow_mode_default = true     # auto-scroll on startup
stdin_timeout_secs = 10        # total wait budget for stdin actions
interrupt_timeout_secs = 5     # total wait budget for interrupt actions
```

The file is optional. If it is absent the built-in defaults apply without any error.

---

## 7. Preferences Not Loading / Silently Using Defaults

**Symptom**: Changes to `tui.toml` are not reflected when `atm-tui` starts. The TUI
behaves as if the file does not exist.

**Causes**:

1. **Parse error** — if `tui.toml` contains invalid TOML syntax or an unrecognised field
   value, `atm-tui` logs a warning to stderr (visible before the TUI switches to alternate
   screen) and falls back to defaults. Check for typos:
   ```bash
   # Quickly validate TOML syntax
   python3 -c "import tomllib, sys; tomllib.load(open(sys.argv[1], 'rb'))" \
       ~/.config/atm/tui.toml
   ```

2. **Wrong path** — verify the file is at the correct location for your `ATM_HOME`:
   ```bash
   echo "${ATM_HOME:-$HOME}/.config/atm/tui.toml"
   ls -la "${ATM_HOME:-$HOME}/.config/atm/tui.toml"
   ```

3. **File not readable** — check permissions:
   ```bash
   ls -la "${ATM_HOME:-$HOME}/.config/atm/tui.toml"
   ```

4. **Wrong field value** — `interrupt_policy` must be one of the three lowercase strings:
   `"always"`, `"never"`, or `"confirm"`. Any other value causes a parse error and falls
   back to defaults.

**Valid `interrupt_policy` values**:

| Value | Behaviour |
|-------|-----------|
| `"confirm"` | (default) Show `[y/N]` prompt before sending interrupt |
| `"always"` | Send interrupt immediately on `Ctrl-I` |
| `"never"` | Silently discard `Ctrl-I` — interrupt is disabled |

---

## 8. Follow Mode

The stream pane auto-scrolls to the latest log line when follow mode is enabled.

| State | Indicator in status bar |
|-------|------------------------|
| Follow on | `F: follow:ON` |
| Follow off | `F: follow:OFF` |

- Default is controlled by `follow_mode_default` in `tui.toml` (default: `true`).
- Toggle at runtime with `F` (uppercase).
- When follow mode is off, the scroll position is preserved so you can read earlier output.
  New lines still accumulate in the buffer — switch follow back on to jump to the bottom.

---

## 9. Performance SLO Targets

These targets define the expected responsiveness of the TUI under normal operating conditions.

| Metric | SLO Target | Notes |
|--------|-----------|-------|
| **Render tick latency** | ≤ 100 ms | Main loop ticks every 100 ms; drawing happens once per tick |
| **Control ack visibility latency** | ≤ 3 s (first attempt) + 5 s (retry) | One automatic retry on timeout using the same `request_id` |
| **Stream tail update latency** | ≤ 100 ms | Log tail read on every tick; capped at 256 KiB per read |
| **Daemon refresh latency** | ≤ 2 s | Member list rate-limited to once per 2 seconds |
| **Stream buffer bound** | 1000 lines | Oldest lines dropped when limit is exceeded; prevents unbounded memory growth |

**Stress test baseline** (validated in `app::tests::test_stress_stream_append_bounded`):
- 10,000 line append in batches of 100 completes in < 200 ms
- Buffer correctly bounded to 1000 lines throughout

If the TUI appears sluggish:
- Check that the daemon is responsive (`atm daemon status`) — slow daemon queries block the 2-second refresh
- Reduce terminal emulator font rendering load (smaller window, simpler font)
- Ensure the session log file is on a fast filesystem (not a network mount)

---

## Quick Reference

| Problem | First thing to check |
|---------|---------------------|
| No agents visible | `atm daemon status` — restart if stopped |
| `[FROZEN]` in stream | Agent session started? Log file readable? |
| `Ctrl-I` no effect | Agent live? Interrupt policy? Focus in Agent Terminal? |
| Status shows `"timeout"` | Daemon responsive? Increase `interrupt_timeout_secs` in config |
| Garbled display | Run `reset` to restore terminal after crash |
| Config not loading | Parse errors on stderr? File path correct? |
