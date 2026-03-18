use crate::{OtelConfig, health, otlp_adapter};
use sc_observability_types::MetricRecord;

/// Export metric records without allowing exporter failures to affect callers.
pub fn export_metric_records_best_effort(records: &[MetricRecord], config: &OtelConfig) {
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
    if let Err(err) = otlp_adapter::export_metrics(config, records) {
        health::note_export_failure(crate::OtelExporterKind::Collector, &err);
    } else {
        health::note_export_success(crate::OtelExporterKind::Collector);
    }
}

#[cfg(test)]
mod tests {
    use super::{MetricKind, MetricRecord};

    #[test]
    fn metric_record_round_trip_with_partial_correlation() {
        let record = MetricRecord {
            timestamp: "2026-03-18T06:00:00Z".to_string(),
            team: Some("atm-dev".to_string()),
            agent: None,
            runtime: Some("codex".to_string()),
            session_id: None,
            name: "atm_messages_total".to_string(),
            kind: MetricKind::Counter,
            value: 7.0,
            unit: Some("count".to_string()),
            source_binary: "atm".to_string(),
            attributes: serde_json::Map::from_iter([(
                "scope".to_string(),
                serde_json::Value::String("mail".to_string()),
            )]),
        };

        let json = serde_json::to_value(&record).expect("serialize metric record");
        let round_trip: MetricRecord =
            serde_json::from_value(json).expect("deserialize metric record");
        assert_eq!(round_trip, record);
    }
}
