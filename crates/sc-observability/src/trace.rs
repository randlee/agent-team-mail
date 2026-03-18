use crate::{OtelConfig, health, otlp_adapter};

/// Neutral trace signal contract for producer-side observability code.
///
/// Correlation fields are intentionally optional and fail-open in AW.1 so
/// producers can adopt trace emission incrementally without blocking callers.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, PartialEq)]
pub struct TraceRecord {
    pub timestamp: String,
    pub team: Option<String>,
    pub agent: Option<String>,
    pub runtime: Option<String>,
    pub session_id: Option<String>,
    pub trace_id: String,
    pub span_id: String,
    pub parent_span_id: Option<String>,
    pub name: String,
    pub status: TraceStatus,
    pub duration_ms: u64,
    pub source_binary: String,
    pub attributes: serde_json::Map<String, serde_json::Value>,
}

#[derive(Debug, Clone, Copy, serde::Serialize, serde::Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum TraceStatus {
    Ok,
    Error,
    Unset,
}

/// Export trace records without allowing exporter failures to affect callers.
///
/// AW.3 uses this to emit native trace spans from CLI and daemon code while
/// keeping all failures fail-open.
pub fn export_trace_records_best_effort(records: &[TraceRecord], config: &OtelConfig) {
    if !config.enabled || records.is_empty() {
        return;
    }
    if config
        .endpoint
        .as_deref()
        .is_none_or(|value| value.trim().is_empty())
    {
        return;
    }
    if let Err(err) = otlp_adapter::export_traces(config, records) {
        health::note_export_failure(crate::OtelExporterKind::Collector, &err);
    } else {
        health::note_export_success(crate::OtelExporterKind::Collector);
    }
}

#[cfg(test)]
mod tests {
    use super::{TraceRecord, TraceStatus};

    #[test]
    fn trace_record_round_trip_allows_missing_correlation_fields() {
        let record = TraceRecord {
            timestamp: "2026-03-18T06:00:00Z".to_string(),
            team: None,
            agent: None,
            runtime: None,
            session_id: None,
            trace_id: "trace-123".to_string(),
            span_id: "span-456".to_string(),
            parent_span_id: Some("span-000".to_string()),
            name: "atm.send".to_string(),
            status: TraceStatus::Ok,
            duration_ms: 42,
            source_binary: "atm".to_string(),
            attributes: serde_json::Map::from_iter([(
                "target".to_string(),
                serde_json::Value::String("team-lead@atm-dev".to_string()),
            )]),
        };

        let json = serde_json::to_value(&record).expect("serialize trace record");
        let round_trip: TraceRecord =
            serde_json::from_value(json).expect("deserialize trace record");
        assert_eq!(round_trip, record);
    }
}
