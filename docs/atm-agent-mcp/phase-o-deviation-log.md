# Phase O Deviation Log

Date: 2026-02-24  
Owner: `arch-ctm`

## O.2

### DEV-O2-001: ATM Mail 3-line clamp rendering
- Requirement: FR-23.8 expects `sender@team <short-message>` with a 3-line clamp.
- Current behavior: attach renderer clamps to three source lines but joins them with ` / ` for compact single-row output.
- Rationale: keep attached stream dense and scan-friendly while preserving exact line-boundary truncation semantics.
- Mitigation: source text is clamped from true line boundaries, and overflow indicator (`...`) is preserved.
- Follow-up: revisit multiline expansion in O.3b if operator feedback shows readability regression.
- approved_by: team-lead
- approved_date: 2026-02-24

### DEV-O2-002: O.2 split trigger assessment
- Assessment: split trigger did **not** fire.
- Reason: required class coverage + fixture expansion landed in O.2 scope without additional architecture churn.
- Residual deferred work: none for O.2 gate; O.3 continues control-path parity hardening.

## O.3

### DEV-O3-001: stream error_source granularity
- Requirement context: control-path/fault surfacing parity in Phase O.
- Current behavior: `error_source` is always emitted as `proxy` (no `child` vs `upstream_mcp` distinction).
- approved_by: team-lead
- approved_date: 2026-02-24
- rationale: full fault source classification deferred to production hardening phase.

### DEV-O3-002: Ctrl-C maps to process exit, not interrupt control
- Requirement context: attached mode control UX.
- Current behavior: terminal `Ctrl-C` follows default process signal behavior and exits attached mode; it does not emit `control.interrupt.request`.
- approved_by: team-lead
- approved_date: 2026-02-24
- rationale: explicit `:interrupt` command is the canonical interrupt path in Phase O; terminal-signal interception is deferred to hardening.

## O-R.4

### DEV-OR4-001: Markdown rendering is semantic-first, not full ANSI syntax highlighting
- Requirement context: FR-23.20 markdown parity hardening in attached/watch output.
- Current behavior: renderer recognizes headings, bullets, and code fences with syntax labels (`[code-block:<lang>]`) and improved wrapping, but does not implement full token-level syntax highlighting.
- approved_by: team-lead
- approved_date: 2026-02-25
- rationale: preserves high-signal readability while avoiding a heavy lexer/highlighter dependency in Phase O-R.4; full token highlighting can be evaluated in post-parity UX hardening.

## O-R.5

### DEV-OR5-001: attach path error_source classification deferred
- Requirement context: FR-23.20 error classification parity.
- Current behavior: O-R.5 implements `error_source` classification (`proxy`/`child`/`upstream`) in the TUI watch path; attach CLI continues generic `stream.error` formatting.
- approved_by: team-lead
- approved_date: 2026-02-25
- rationale: O-R.5 implementation scope targeted the TUI watch pipeline first; attach-path parity is deferred follow-up.
- closed_date: 2026-02-25
- closed_by: arch-ctm
- implementation_ref: https://github.com/randlee/agent-team-mail/pull/242
- validation_status: verified

### DEV-OR5-002: attach path fatal reconnect hint deferred
- Requirement context: FR-23.20 fatal-path operator guidance.
- Current behavior: TUI watch rendering appends fatal reconnect guidance; attach CLI does not yet emit the same hint text.
- approved_by: team-lead
- approved_date: 2026-02-25
- rationale: bounded-scope hardening in O-R.5 prioritized live TUI operator surface.
- closed_date: 2026-02-25
- closed_by: arch-ctm
- implementation_ref: https://github.com/randlee/agent-team-mail/pull/242
- validation_status: verified

### DEV-OR5-003: attach replay remains fixed-window clip
- Requirement context: FR-23.22 turn-boundary-aware replay behavior.
- Current behavior: TUI replay now applies turn-boundary shaping + truncation warning; attach replay remains fixed 50-frame clipping.
- approved_by: team-lead
- approved_date: 2026-02-25
- rationale: replay hardening landed in TUI watch path; attach replay parity deferred.

### DEV-OR5-004: attach replay checkpoint persistence deferred
- Requirement context: FR-23.22 session-scoped replay checkpoint continuity.
- Current behavior: checkpoint persistence is implemented in TUI watch path only.
- approved_by: team-lead
- approved_date: 2026-02-25
- rationale: checkpointing added where re-attach continuity is user-visible in the TUI; attach checkpointing deferred.

### DEV-OR5-005: attach unsupported-event summary flush deferred
- Requirement context: FR-23.23 unsupported-event telemetry summary.
- Current behavior: unsupported-event summary/warning flush is implemented in TUI flow; attach path does not flush per-session summary on detach/exit.
- approved_by: team-lead
- approved_date: 2026-02-25
- rationale: telemetry closure delivered for TUI watch path in O-R.5; attach telemetry flush deferred.

### DEV-OR5-006: attach stdin payload sanitization deferred
- Requirement context: FR-23.23 stdin input sanitization.
- Current behavior: `parse_attach_input()` trims whitespace only before forwarding payload to `send_stdin_control()`. No null-byte stripping, control-character filtering, or ANSI-sequence rejection.
- approved_by: team-lead
- approved_date: 2026-02-25
- rationale: TUI-first scope for O-R.5; attach stdin sanitization deferred to input-hardening phase.

### DEV-OR5-007: attach help text missing Ctrl-C/SIGINT documentation
- Requirement context: FR-23.25, GAP-014 help text completeness.
- Current behavior: `print_input_contract()` omits Ctrl-C/SIGINT behavior description.
- approved_by: team-lead
- approved_date: 2026-02-25
- rationale: Ctrl-C follows default process signal behavior (DEV-O3-002); explicit documentation deferred to help-text hardening pass.

## Phase P Progress Note (2026-02-25)

O-R.5 approved deviations are being closed in Phase P by sprint:

- P.1: DEV-OR5-001, DEV-OR5-002 (closed)
- P.2: DEV-OR5-003, DEV-OR5-004 (planned)
- P.3: DEV-OR5-005 (planned)
- P.4: DEV-OR5-006 (planned)
- P.5: DEV-OR5-007 (planned)
