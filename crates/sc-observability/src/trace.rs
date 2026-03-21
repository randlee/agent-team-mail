use crate::{OtelConfig, health, otlp_adapter};
use sc_observability_types::TraceRecord;

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
    use super::TraceRecord;
    use crate::TraceStatus;

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
