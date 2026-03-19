use crate::{
    MetricKind, MetricRecord, OtelConfig, OtelError, OtelExporter, OtelExporterKind, OtelRecord,
    TraceRecord, TraceStatus,
};
use sc_observability_otlp::{
    MetricKind as TransportMetricKind, MetricTransportRecord, TraceStatus as TransportTraceStatus,
    TraceTransportRecord, TransportConfig, TransportError, TransportExporter,
    TransportExporterKind, TransportRecord,
};
use std::sync::Arc;

pub fn build_transport_exporters(
    config: &OtelConfig,
) -> Result<Vec<Arc<dyn OtelExporter>>, OtelError> {
    let transport_config = config.clone();

    let exporters = sc_observability_otlp::build_exporters(&transport_config)
        .map_err(|err| OtelError::ExportFailed(err.to_string()))?;
    Ok(exporters
        .into_iter()
        .map(|exporter| Arc::new(BridgeExporter { exporter }) as Arc<dyn OtelExporter>)
        .collect())
}

struct BridgeExporter {
    exporter: Arc<dyn TransportExporter>,
}

impl OtelExporter for BridgeExporter {
    fn kind(&self) -> OtelExporterKind {
        match self.exporter.kind() {
            TransportExporterKind::Collector => OtelExporterKind::Collector,
            TransportExporterKind::DebugLocal => OtelExporterKind::DebugLocal,
        }
    }

    fn export(&self, record: &OtelRecord) -> Result<(), OtelError> {
        self.exporter
            .export(&to_transport_record(record))
            .map_err(map_transport_error)
    }
}

fn to_transport_record(record: &OtelRecord) -> TransportRecord {
    TransportRecord {
        name: record.name.clone(),
        source_binary: record.source_binary.clone(),
        level: record.level.clone(),
        trace_id: record.trace_id.clone(),
        span_id: record.span_id.clone(),
        attributes: record.attributes.clone(),
    }
}

pub fn export_traces(config: &OtelConfig, records: &[TraceRecord]) -> Result<(), OtelError> {
    let transport_config = build_transport_config(config);
    let records = records
        .iter()
        .cloned()
        .map(to_trace_transport_record)
        .collect::<Vec<_>>();
    sc_observability_otlp::export_traces(&transport_config, &records).map_err(map_transport_error)
}

pub fn export_metrics(config: &OtelConfig, records: &[MetricRecord]) -> Result<(), OtelError> {
    let transport_config = build_transport_config(config);
    let records = records
        .iter()
        .cloned()
        .map(to_metric_transport_record)
        .collect::<Vec<_>>();
    sc_observability_otlp::export_metrics(&transport_config, &records).map_err(map_transport_error)
}

fn build_transport_config(config: &OtelConfig) -> TransportConfig {
    config.clone()
}

fn to_trace_transport_record(record: TraceRecord) -> TraceTransportRecord {
    TraceTransportRecord {
        timestamp: record.timestamp,
        team: record.team,
        agent: record.agent,
        runtime: record.runtime,
        session_id: record.session_id,
        trace_id: record.trace_id,
        span_id: record.span_id,
        parent_span_id: record.parent_span_id,
        name: record.name,
        status: match record.status {
            TraceStatus::Ok => TransportTraceStatus::Ok,
            TraceStatus::Error => TransportTraceStatus::Error,
            TraceStatus::Unset => TransportTraceStatus::Unset,
        },
        duration_ms: record.duration_ms,
        source_binary: record.source_binary,
        attributes: record.attributes,
    }
}

fn to_metric_transport_record(record: MetricRecord) -> MetricTransportRecord {
    MetricTransportRecord {
        timestamp: record.timestamp,
        team: record.team,
        agent: record.agent,
        runtime: record.runtime,
        session_id: record.session_id,
        name: record.name,
        kind: match record.kind {
            MetricKind::Counter => TransportMetricKind::Counter,
            MetricKind::Gauge => TransportMetricKind::Gauge,
            MetricKind::Histogram => TransportMetricKind::Histogram,
        },
        value: record.value,
        unit: record.unit,
        source_binary: record.source_binary,
        attributes: record.attributes,
    }
}

fn map_transport_error(err: TransportError) -> OtelError {
    OtelError::ExportFailed(err.to_string())
}
