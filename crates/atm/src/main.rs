//! atm - Mail-like messaging for Claude agent teams
//!
//! A thin CLI over the `~/.claude/teams/` file-based API, providing
//! send, read, broadcast, and inbox commands with atomic file I/O.

use agent_team_mail_core::event_log::{
    EventFields, clear_event_observer_hook, emit_event_best_effort, install_event_observer_hook,
};
use agent_team_mail_core::gh_command::{
    flush_local_gh_observability_records, install_cli_teardown_hook, run_cli_teardown_hook,
};
use agent_team_mail_core::logging;
use clap::{CommandFactory, FromArgMatches};
use sc_observability::LogConfig;
use sc_observability_types::{MetricKind, MetricRecord, OtelConfig, TraceRecord, TraceStatus};
use std::sync::Arc;
use std::time::Instant;
use uuid::Uuid;

mod commands;
mod consts;
mod util;

use commands::Cli;

fn parse_cli() -> Cli {
    let version =
        agent_team_mail_core::install::effective_display_version(env!("CARGO_PKG_VERSION"));
    let version: &'static str = Box::leak(version.into_boxed_str());
    let matches = Cli::command().version(version).get_matches();
    Cli::from_arg_matches(&matches).unwrap_or_else(|err| err.exit())
}

fn env_nonempty(key: &str) -> Option<String> {
    std::env::var(key).ok().and_then(|value| {
        let trimmed = value.trim();
        if trimmed.is_empty() {
            None
        } else {
            Some(trimmed.to_string())
        }
    })
}

fn build_command_trace_record(
    command_name: &str,
    request_id: &str,
    trace_id: &str,
    span_id: &str,
    status: TraceStatus,
    duration_ms: u64,
    error: Option<&str>,
) -> TraceRecord {
    let mut attributes = serde_json::Map::new();
    attributes.insert(
        "command".to_string(),
        serde_json::Value::String(command_name.to_string()),
    );
    attributes.insert(
        "request_id".to_string(),
        serde_json::Value::String(request_id.to_string()),
    );
    attributes.insert(
        "outcome".to_string(),
        serde_json::Value::String(
            match status {
                TraceStatus::Ok => "ok",
                TraceStatus::Error => "error",
                TraceStatus::Unset => "unset",
            }
            .to_string(),
        ),
    );
    if let Some(error) = error {
        attributes.insert(
            "error".to_string(),
            serde_json::Value::String(error.to_string()),
        );
    }

    TraceRecord {
        timestamp: chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
        team: env_nonempty("ATM_TEAM"),
        agent: env_nonempty("ATM_IDENTITY"),
        runtime: env_nonempty("ATM_RUNTIME"),
        session_id: env_nonempty("CLAUDE_SESSION_ID"),
        trace_id: trace_id.to_string(),
        span_id: span_id.to_string(),
        parent_span_id: None,
        name: format!("atm.command.{command_name}"),
        status,
        duration_ms,
        source_binary: "atm".to_string(),
        attributes,
    }
}

fn build_metric_record(
    name: &str,
    kind: MetricKind,
    value: f64,
    unit: Option<&str>,
    attributes: serde_json::Map<String, serde_json::Value>,
) -> MetricRecord {
    MetricRecord {
        timestamp: chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
        team: env_nonempty("ATM_TEAM"),
        agent: env_nonempty("ATM_IDENTITY"),
        runtime: env_nonempty("ATM_RUNTIME"),
        session_id: env_nonempty("CLAUDE_SESSION_ID"),
        name: name.to_string(),
        kind,
        value,
        unit: unit.map(str::to_string),
        source_binary: "atm".to_string(),
        attributes,
    }
}

fn build_command_metric_records(
    command_name: &str,
    outcome: &str,
    duration_ms: u64,
) -> Vec<MetricRecord> {
    let mut records = Vec::new();

    let mut base_attrs = serde_json::Map::new();
    base_attrs.insert(
        "command".to_string(),
        serde_json::Value::String(command_name.to_string()),
    );
    base_attrs.insert(
        "outcome".to_string(),
        serde_json::Value::String(outcome.to_string()),
    );

    records.push(build_metric_record(
        "atm.commands_count",
        MetricKind::Counter,
        1.0,
        Some("count"),
        base_attrs.clone(),
    ));
    records.push(build_metric_record(
        "atm.command_duration_ms",
        MetricKind::Histogram,
        duration_ms as f64,
        Some("ms"),
        base_attrs.clone(),
    ));

    match command_name {
        "send" | "broadcast" | "request" => records.push(build_metric_record(
            "atm.messages_sent_count",
            MetricKind::Counter,
            1.0,
            Some("count"),
            base_attrs.clone(),
        )),
        "read" | "inbox" => records.push(build_metric_record(
            "atm.messages_read_count",
            MetricKind::Counter,
            1.0,
            Some("count"),
            base_attrs.clone(),
        )),
        _ => {}
    }

    if let Ok(home_dir) = agent_team_mail_core::home::get_home_dir() {
        let logging = crate::commands::logging_health::read_daemon_logging_health(&home_dir);
        let mut logging_attrs = base_attrs.clone();
        logging_attrs.insert(
            "logging_state".to_string(),
            serde_json::Value::String(logging.state.clone()),
        );
        records.push(build_metric_record(
            "atm.spool_file_count",
            MetricKind::Gauge,
            logging.spool_count as f64,
            Some("count"),
            logging_attrs.clone(),
        ));
        records.push(build_metric_record(
            "atm.dropped_events_total",
            MetricKind::Gauge,
            logging.dropped_counter as f64,
            Some("count"),
            logging_attrs,
        ));

        let otel = crate::commands::logging_health::read_daemon_otel_health(&home_dir);
        if let Some(code) = otel.last_error.code {
            let mut otel_attrs = base_attrs;
            otel_attrs.insert("error_code".to_string(), serde_json::Value::String(code));
            otel_attrs.insert(
                "collector_state".to_string(),
                serde_json::Value::String(otel.collector_state),
            );
            records.push(build_metric_record(
                "atm.export_failures_count",
                MetricKind::Counter,
                1.0,
                Some("count"),
                otel_attrs,
            ));
        }
    }

    records
}

fn install_cli_otel_event_hook() {
    let home_dir =
        agent_team_mail_core::home::get_home_dir().unwrap_or_else(|_| std::env::temp_dir());
    let log_path = LogConfig::from_home_for_tool(&home_dir, "atm").log_path;
    install_event_observer_hook(Arc::new(move |event| {
        sc_observability::export_otel_best_effort_from_path(&log_path, event);
    }));
}

fn export_trace_records_from_entrypoint(records: &[TraceRecord], config: &OtelConfig) {
    let _ = sc_observability_otlp::export_traces(config, records);
}

fn export_metric_records_from_entrypoint(records: &[MetricRecord], config: &OtelConfig) {
    let _ = sc_observability_otlp::export_metrics(config, records);
}

fn main() {
    // Enable daemon auto-start for daemon-backed ATM commands.
    // Respect explicit caller override (e.g., tests setting "0").
    if std::env::var_os("ATM_DAEMON_AUTOSTART").is_none() {
        // SAFETY: process-local env mutation at startup before command execution.
        unsafe { std::env::set_var("ATM_DAEMON_AUTOSTART", "1") };
    }

    let _guards = logging::init_unified(
        "atm",
        logging::UnifiedLogMode::ProducerFanIn {
            daemon_socket: agent_team_mail_core::daemon_client::daemon_socket_path()
                .unwrap_or_else(|_| std::env::temp_dir().join("atm-daemon.sock")),
            fallback_spool_dir: agent_team_mail_core::home::get_home_dir()
                .map(|home| agent_team_mail_core::logging_event::configured_spool_dir(&home))
                .unwrap_or_else(|_| std::env::temp_dir().join("atm-spool")),
        },
    )
    .unwrap_or_else(|_| logging::init_stderr_only());
    install_cli_otel_event_hook();
    if let Ok(home_dir) = agent_team_mail_core::home::get_home_dir() {
        install_cli_teardown_hook(Arc::new(move || {
            let _ = flush_local_gh_observability_records(&home_dir);
        }));
    }

    let cli = parse_cli();
    let command_name = cli.command_name().to_string();
    let request_id = Uuid::new_v4().to_string();
    let trace_id = agent_team_mail_core::event_log::trace_id_for_request("atm", &request_id);
    let start_span_id =
        agent_team_mail_core::event_log::span_id_for_action(&trace_id, "command_start");
    let started_at = Instant::now();

    emit_event_best_effort(EventFields {
        level: "info",
        source: "atm",
        action: "command_start",
        team: std::env::var("ATM_TEAM").ok(),
        session_id: std::env::var("CLAUDE_SESSION_ID").ok(),
        agent_id: std::env::var("ATM_IDENTITY").ok(),
        agent_name: std::env::var("ATM_IDENTITY").ok(),
        result: Some("starting".to_string()),
        request_id: Some(request_id.clone()),
        trace_id: Some(trace_id.clone()),
        span_id: Some(start_span_id.clone()),
        extra_fields: {
            let mut fields = serde_json::Map::new();
            fields.insert(
                "command".to_string(),
                serde_json::Value::String(command_name.clone()),
            );
            fields
        },
        ..Default::default()
    });

    let otel_config = OtelConfig::from_env();
    let exit_code = if let Err(e) = cli.execute() {
        let rendered = e.to_string();
        let duration_ms = started_at.elapsed().as_millis() as u64;
        emit_event_best_effort(EventFields {
            level: "error",
            source: "atm",
            action: "command_error",
            team: std::env::var("ATM_TEAM").ok(),
            session_id: std::env::var("CLAUDE_SESSION_ID").ok(),
            result: Some("error".to_string()),
            request_id: Some(request_id.clone()),
            trace_id: Some(trace_id.clone()),
            span_id: Some(agent_team_mail_core::event_log::span_id_for_action(
                &trace_id,
                "command_error",
            )),
            error: Some(rendered.clone()),
            extra_fields: {
                let mut fields = serde_json::Map::new();
                fields.insert(
                    "command".to_string(),
                    serde_json::Value::String(command_name.clone()),
                );
                fields
            },
            ..Default::default()
        });
        export_trace_records_from_entrypoint(
            &[build_command_trace_record(
                &command_name,
                &request_id,
                &trace_id,
                &start_span_id,
                TraceStatus::Error,
                duration_ms,
                Some(&rendered),
            )],
            &otel_config,
        );
        export_metric_records_from_entrypoint(
            &build_command_metric_records(&command_name, "error", duration_ms),
            &otel_config,
        );
        if serde_json::from_str::<serde_json::Value>(&rendered).is_ok() {
            eprintln!("{rendered}");
        } else {
            eprintln!("Error: {rendered}");
        }
        1
    } else {
        let duration_ms = started_at.elapsed().as_millis() as u64;
        emit_event_best_effort(EventFields {
            level: "info",
            source: "atm",
            action: "command_success",
            team: std::env::var("ATM_TEAM").ok(),
            session_id: std::env::var("CLAUDE_SESSION_ID").ok(),
            agent_id: std::env::var("ATM_IDENTITY").ok(),
            agent_name: std::env::var("ATM_IDENTITY").ok(),
            result: Some("ok".to_string()),
            request_id: Some(request_id.clone()),
            trace_id: Some(trace_id.clone()),
            span_id: Some(agent_team_mail_core::event_log::span_id_for_action(
                &trace_id,
                "command_success",
            )),
            extra_fields: {
                let mut fields = serde_json::Map::new();
                fields.insert(
                    "command".to_string(),
                    serde_json::Value::String(command_name.clone()),
                );
                fields
            },
            ..Default::default()
        });
        export_trace_records_from_entrypoint(
            &[build_command_trace_record(
                &command_name,
                &request_id,
                &trace_id,
                &start_span_id,
                TraceStatus::Ok,
                duration_ms,
                None,
            )],
            &otel_config,
        );
        export_metric_records_from_entrypoint(
            &build_command_metric_records(&command_name, "ok", duration_ms),
            &otel_config,
        );
        0
    };

    // Neutral CLI teardown hook for plugin-owned lifecycle cleanup.
    run_cli_teardown_hook();
    clear_event_observer_hook();

    if exit_code != 0 {
        std::process::exit(exit_code);
    }
}
