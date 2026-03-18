# Phase AX Planning: Team Onboarding + TUI/Doctor Stability

**Status**: Complete

## Goal

Close the last dogfood blockers before broader OTel rollout:

- keep dev-install and shared daemon ownership stable
- harden inbox/TUI behavior and Windows path handling
- preserve team membership state during normal CLI use
- fix OTLP field shaping so Grafana-compatible log ingestion gets the right
  service name, correlation attributes, and severity values

## Sprint Map

| Sprint | Focus | Status |
|---|---|---|
| AX.1 | Dev-install daemon ownership + config member preservation (`#835`, `#793`) | COMPLETE |
| AX.2 | Inbox/TUI bug fixes (`#724`, `#725`, `#772`, `#783`) | COMPLETE |
| AX.3 | Test cleanup and carry-forward reliability debt | COMPLETE |
| AX.4 | OTLP field shaping (`#862`, `#863`) | COMPLETE at `4e5214d3` |

## AX.4 OTLP Field Shaping

**Issues**:
- `#862` `service_name=unknown_service`
- `#863` missing correlation fields + `detected_level=unknown`

**Deliverables**:
- map `source_binary` to OTLP `resource.service.name`
- export `team`, `agent`, `runtime`, and `session_id` as OTLP log attributes
  when present
- map ATM log levels to OTLP `severityNumber` / `severityText`

**Outcome**:
- `sc-observability-otlp` now shapes resource/service identity and required
  correlation attributes correctly for Grafana-compatible OTLP log ingestion
- severity values are no longer exported as unknown for canonical ATM levels
