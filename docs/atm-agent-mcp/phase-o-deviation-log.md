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
