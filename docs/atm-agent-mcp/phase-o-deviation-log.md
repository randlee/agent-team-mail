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
