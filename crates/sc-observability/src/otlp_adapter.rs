use crate::{OtelConfig, OtelError, OtelExporter, OtelRecord};
use sc_observability_otlp::{TransportConfig, TransportError, TransportExporter, TransportRecord};
use std::sync::Arc;

pub fn build_transport_exporters(config: &OtelConfig) -> Result<Vec<Arc<dyn OtelExporter>>, OtelError> {
    let transport_config = TransportConfig {
        endpoint: config.endpoint.clone(),
        protocol: config.protocol.clone(),
        auth_header: config.auth_header.clone(),
        ca_file: config.ca_file.clone(),
        insecure_skip_verify: config.insecure_skip_verify,
        timeout_ms: config.timeout_ms,
        debug_local_export: config.debug_local_export,
    };

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
    fn export(&self, record: &OtelRecord) -> Result<(), OtelError> {
        self.exporter
            .export(&to_transport_record(record))
            .map_err(map_transport_error)
    }
}

fn to_transport_record(record: &OtelRecord) -> TransportRecord {
    TransportRecord {
        name: record.name.clone(),
        trace_id: record.trace_id.clone(),
        span_id: record.span_id.clone(),
        attributes: record.attributes.clone(),
    }
}

fn map_transport_error(err: TransportError) -> OtelError {
    OtelError::ExportFailed(err.to_string())
}
