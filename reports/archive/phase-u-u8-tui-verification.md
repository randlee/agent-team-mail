---
title: "U.8 TUI Verification Evidence"
sprint: U.8
phase: U
type: verification
issues: ["#184", "#185", "#187"]
source-sprint-pr: "#299"
source-branch: feature/pT-s6-tui-coverage
verified-against: v0.27.0
date: 2026-03-01
status: PASS
---

# U.8 TUI Verification Evidence

## Summary

Sprint U.8 verifies the Phase T Sprint T.6 deliverables (PR #299, branch
`feature/pT-s6-tui-coverage`) against the acceptance criteria in
`docs/test-plan-phase-U.md` section U.8, covering GitHub issues #184, #185,
and #187.

All five acceptance criteria pass. The T.6 worktree delivers 157 TUI unit
tests; all pass with zero failures. Issues #184, #185, and #187 are ready to
be closed.

---

## Verification Environment

| Field | Value |
|-------|-------|
| Worktree | `/Users/randlee/Documents/github/agent-team-mail-worktrees/feature/pT-s6-tui-coverage` |
| Test command | `cargo test -p agent-team-mail-tui` |
| Result | 157 passed, 0 failed, 0 ignored |
| Runtime | 0.02 s |

---

## Acceptance Criteria Results

### AC-1: TUI left/right panel state derives from same source (Issue #184)

**Status**: PASS

**Evidence**: Both the left Dashboard panel and the right Agent Terminal panel
render from a single shared `App` struct. The `App::members` field (type
`Vec<MemberRow>`) is populated once per poll cycle and read by both
`draw_dashboard` and `draw_agent_terminal` without duplication or independent
re-computation.

Key code path (`crates/atm-tui/src/ui.rs`):

```rust
pub fn draw(frame: &mut Frame, app: &App) {
    // ...
    draw_header(frame, outer[0], app);
    draw_body(frame, outer[1], app);       // passes same &app to both panels
    // ...
}

fn draw_body(frame: &mut Frame, area: Rect, app: &App) {
    // ...
    draw_dashboard(frame, columns[0], app);          // left panel
    draw_agent_terminal(frame, columns[1], app);     // right panel — same app
}
```

The right panel stream badge derives its LIVE/REPLAY/WAITING state from
`app.daemon_turn_state`, and agent selection from `app.selected_index` into
`app.members` — the same slice the left panel uses. There is no separate state
store that could diverge.

**Test**: `ui::tests::test_panel_state_parity_uses_shared_snapshot`

```
test ui::tests::test_panel_state_parity_uses_shared_snapshot ... ok
```

The test sets `app.members`, `app.selected_index`, `app.streaming_agent`, and
`app.daemon_turn_state` once, then asserts both "arch-ctm", "busy", and
"[LIVE]" appear in the rendered frame — confirming both panels reflect the same
snapshot.

---

### AC-2: Message list view shows inbox messages (Issue #185)

**Status**: PASS

**Evidence**: `crates/atm-tui/src/dashboard.rs` implements
`read_inbox_messages` which reads the agent's inbox JSON file under a
lock-protected path and returns messages newest-first. The `draw_dashboard`
function in `ui.rs` renders the full list as a `ratatui::widgets::List` when
`app.inbox_detail_open` is false, with unread marker (`●`) and from/summary
columns.

**Tests** (all pass):

```
test dashboard::tests::test_read_inbox_messages_returns_recent_first ... ok
test dashboard::tests::test_read_inbox_preview_returns_recent_messages ... ok
test dashboard::tests::test_inbox_count_with_messages ... ok
test dashboard::tests::test_inbox_count_empty ... ok
```

`test_read_inbox_messages_returns_recent_first` writes a 3-message inbox file,
calls `read_inbox_messages` with `max_items=2`, and asserts the two most recent
messages are returned in reverse-chronological order (newest first):

```rust
assert_eq!(messages[0].from, "c");   // timestamp 00:00:02
assert_eq!(messages[1].from, "b");   // timestamp 00:00:01
```

---

### AC-3: Message detail view renders full content (Issue #185)

**Status**: PASS

**Evidence**: When `app.inbox_detail_open` is true, `draw_dashboard` switches
the inbox sub-panel to render the selected message's full content via
`app.selected_message()`:

```rust
} else if app.inbox_detail_open {
    if let Some(msg) = app.selected_message() {
        let detail = vec![
            Line::from(Span::styled(
                format!("From: {}  [{status}]", msg.from), ...)),
            Line::from(Span::styled(
                format!("At: {}", msg.timestamp), ...)),
            Line::from(Span::raw("")),
            Line::from(Span::raw(msg.text.clone())),  // full message body
        ];
        frame.render_widget(
            Paragraph::new(detail).block(inbox_block).wrap(Wrap { trim: false }),
            left_rows[1],
        );
    }
}
```

The `Wrap { trim: false }` ensures full multi-line content is not clipped.
`app.selected_message()` delegates to `app.inbox_messages.get(app.selected_message_index)`,
which is the same slice populated from the lock-protected inbox read.

---

### AC-4: Mark-read persists to inbox file with lock-protected write (Issue #185)

**Status**: PASS

**Evidence**: `dashboard::mark_inbox_message_read` acquires a file lock on
`{agent}.lock` before reading or writing the inbox JSON:

```rust
let _lock = acquire_lock(&lock_path, 5).map_err(|e| format!("lock failed: {e}"))?;
let content = std::fs::read(&inbox_path)...;
let mut messages: Vec<InboxMessage> = serde_json::from_slice(&content)...;
// mutate
if changed {
    let payload = serde_json::to_vec_pretty(&messages)...;
    std::fs::write(&inbox_path, payload)...;
}
```

The lock guard (`_lock`) is held across the read-mutate-write cycle and dropped
at function return, preventing concurrent partial writes.

**Test**: `dashboard::tests::test_mark_inbox_message_read_updates_file`

```
test dashboard::tests::test_mark_inbox_message_read_updates_file ... ok
```

The test writes a 2-message inbox, calls `mark_inbox_message_read` targeting
message `m2`, asserts `changed == true`, then re-reads the file via
`read_inbox_messages` and verifies `m2.read == true`.

---

### AC-5: TUI header shows version number (Issue #187)

**Status**: PASS

**Evidence**: `draw_header` in `crates/atm-tui/src/ui.rs` constructs the
header line using `env!("CARGO_PKG_VERSION")`:

```rust
fn draw_header(frame: &mut Frame, area: Rect, app: &App) {
    let text = Line::from(vec![
        Span::styled(" ATM TUI  ", ...),
        Span::raw(format!(
            " v{}  Team: {}",
            env!("CARGO_PKG_VERSION"),
            app.team
        )),
    ]);
    frame.render_widget(Paragraph::new(text), area);
}
```

The version string is embedded at compile time from the crate manifest — it
cannot be empty or missing.

**Test**: `ui::tests::test_header_includes_non_empty_version_token`

```
test ui::tests::test_header_includes_non_empty_version_token ... ok
```

The test renders a full 100x30 frame into a `TestBackend` buffer and asserts:

```rust
assert!(rendered.contains("ATM TUI"));
assert!(rendered.contains(&format!("v{}", env!("CARGO_PKG_VERSION"))));
```

---

## Full Test Run Output

```
running 157 tests
... (all pass)

test dashboard::tests::test_inbox_count_empty ... ok
test dashboard::tests::test_inbox_count_with_messages ... ok
test dashboard::tests::test_mark_inbox_message_read_updates_file ... ok
test dashboard::tests::test_read_inbox_messages_returns_recent_first ... ok
test dashboard::tests::test_read_inbox_preview_returns_recent_messages ... ok
test dashboard::tests::test_read_team_members_from_config ... ok
test dashboard::tests::test_session_log_path ... ok
test ui::tests::test_header_includes_non_empty_version_token ... ok
test ui::tests::test_panel_state_parity_uses_shared_snapshot ... ok

test result: ok. 157 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.02s
```

---

## Issue Disposition

| Issue | Description | Verdict | Action |
|-------|-------------|---------|--------|
| #184 | TUI right panel contradicts left panel | FIXED in T.6 | Close |
| #185 | No message viewing in TUI | FIXED in T.6 | Close |
| #187 | TUI header missing version number | FIXED in T.6 | Close |

All three issues are resolved by the T.6 sprint implementation and confirmed by
the 157-test suite. Recommend closing #184, #185, and #187 upon merge of this
sprint PR.

---

## Files Examined

| File | Purpose |
|------|---------|
| `crates/atm-tui/src/ui.rs` | Header render, panel draw functions, test assertions |
| `crates/atm-tui/src/dashboard.rs` | Inbox read/write helpers, mark-read with lock, tests |
| `crates/atm-tui/src/app.rs` | Shared `App` state struct — single source of truth for both panels |
