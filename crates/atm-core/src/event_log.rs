//! Shared structured JSONL event logging for ATM binaries.
//!
//! This module provides a compact, cross-process event sink used by `atm`,
//! `atm-daemon`, and `atm-agent-mcp`.

#[derive(Clone, Debug, Default)]
pub struct EventFields {
    pub level: &'static str,
    pub source: &'static str,
    pub action: &'static str,
    pub team: Option<String>,
    pub session_id: Option<String>,
    pub agent_id: Option<String>,
    pub agent_name: Option<String>,
    pub target: Option<String>,
    pub result: Option<String>,
    pub message_id: Option<String>,
    pub request_id: Option<String>,
    pub error: Option<String>,
    pub count: Option<u64>,
    pub message_text: Option<String>,
    pub runtime: Option<String>,
    pub runtime_session_id: Option<String>,
    pub teardown_stage: Option<String>,
    pub spawn_mode: Option<String>,
    pub extra_fields: serde_json::Map<String, serde_json::Value>,
    pub sender_agent: Option<String>,
    pub sender_team: Option<String>,
    pub sender_pid: Option<u32>,
    pub recipient_agent: Option<String>,
    pub recipient_team: Option<String>,
    pub recipient_pid: Option<u32>,
}

/// Forward `event` to the unified producer channel if a sender is registered.
fn forward_to_unified(event: crate::logging_event::LogEventV1) {
    if let Some(tx) = crate::logging::producer_sender() {
        // send returns Err if full or disconnected; we silently drop.
        let _ = tx.try_send(event);
    }
}

fn logging_enabled() -> bool {
    !matches!(
        std::env::var("ATM_LOG")
            .ok()
            .map(|v| v.trim().to_ascii_lowercase()),
        Some(ref v) if v == "0" || v == "false" || v == "off" || v == "disabled" || v == "no"
    )
}

/// Map [`EventFields`] to a [`LogEventV1`] for the unified pipeline.
fn fields_to_log_event(fields: &EventFields) -> crate::logging_event::LogEventV1 {
    use crate::logging_event::LogEventV1;
    use chrono::Utc;

    let hostname = hostname::get()
        .map(|h| h.to_string_lossy().to_string())
        .unwrap_or_default();

    LogEventV1 {
        v: 1,
        ts: Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
        level: fields.level.to_string(),
        source_binary: fields.source.to_string(),
        hostname,
        pid: std::process::id(),
        target: fields.target.clone().unwrap_or_default(),
        action: fields.action.to_string(),
        team: fields.team.clone(),
        agent: fields
            .agent_id
            .clone()
            .or_else(|| fields.agent_name.clone()),
        session_id: fields.session_id.clone(),
        request_id: fields.request_id.clone(),
        correlation_id: None,
        outcome: fields.result.clone(),
        error: fields.error.clone(),
        fields: {
            let mut map = serde_json::Map::new();
            if let Some(mid) = &fields.message_id {
                map.insert(
                    "message_id".to_string(),
                    serde_json::Value::String(mid.clone()),
                );
            }
            if let Some(cnt) = fields.count {
                map.insert("count".to_string(), serde_json::Value::Number(cnt.into()));
            }
            // Intentionally exclude EventFields::message_text from persistent logs.
            // Message body text can contain sensitive content; logging keeps
            // transport metadata only (target/result/message_id/count/etc.).
            if let Some(runtime) = &fields.runtime {
                map.insert(
                    "runtime".to_string(),
                    serde_json::Value::String(runtime.clone()),
                );
            }
            if let Some(runtime_session_id) = &fields.runtime_session_id {
                map.insert(
                    "runtime_session_id".to_string(),
                    serde_json::Value::String(runtime_session_id.clone()),
                );
            }
            if let Some(teardown_stage) = &fields.teardown_stage {
                map.insert(
                    "teardown_stage".to_string(),
                    serde_json::Value::String(teardown_stage.clone()),
                );
            }
            if let Some(spawn_mode) = &fields.spawn_mode {
                map.insert(
                    "spawn_mode".to_string(),
                    serde_json::Value::String(spawn_mode.clone()),
                );
            }
            if let Some(sender_agent) = &fields.sender_agent {
                map.insert(
                    "sender_agent".to_string(),
                    serde_json::Value::String(sender_agent.clone()),
                );
            }
            if let Some(sender_team) = &fields.sender_team {
                map.insert(
                    "sender_team".to_string(),
                    serde_json::Value::String(sender_team.clone()),
                );
            }
            if let Some(sender_pid) = fields.sender_pid {
                map.insert(
                    "sender_pid".to_string(),
                    serde_json::Value::Number(sender_pid.into()),
                );
            }
            if let Some(recipient_agent) = &fields.recipient_agent {
                map.insert(
                    "recipient_agent".to_string(),
                    serde_json::Value::String(recipient_agent.clone()),
                );
            }
            if let Some(recipient_team) = &fields.recipient_team {
                map.insert(
                    "recipient_team".to_string(),
                    serde_json::Value::String(recipient_team.clone()),
                );
            }
            if let Some(recipient_pid) = fields.recipient_pid {
                map.insert(
                    "recipient_pid".to_string(),
                    serde_json::Value::Number(recipient_pid.into()),
                );
            }
            if fields.action == "send"
                && std::env::var("ATM_LOG_MSG")
                    .ok()
                    .map(|v| v.trim() == "1")
                    .unwrap_or(false)
                && let Some(preview) = message_preview(fields.message_text.as_deref())
            {
                map.insert(
                    "message_preview".to_string(),
                    serde_json::Value::String(preview),
                );
            }
            for (k, v) in &fields.extra_fields {
                map.entry(k.clone()).or_insert_with(|| v.clone());
            }
            if let Some(ppid) = crate::pid::parent_pid() {
                map.entry("ppid".to_string())
                    .or_insert_with(|| serde_json::Value::Number(ppid.into()));
            }
            map
        },
        spans: vec![],
    }
}

/// Emit a single structured event to the unified logging channel.
///
/// This function is intentionally fail-open: if the unified channel is not
/// initialised or the send fails, the event is silently dropped.
///
/// The legacy `events.jsonl` dual-write path was removed in Phase M.1b.
/// Events flow exclusively through the unified producer channel.
pub fn emit_event_best_effort(mut fields: EventFields) {
    if !logging_enabled() {
        return;
    }

    if fields.level.is_empty() || fields.source.is_empty() || fields.action.is_empty() {
        return;
    }

    // Pick up CLAUDE_SESSION_ID if the caller did not supply a session_id.
    // If the env var is absent, leave session_id as None — do NOT fall back to
    // a sentinel string like "unknown".  The LogEventV1 schema uses Option<String>
    // to distinguish "no session" from a known session ID.
    if fields.session_id.is_none() {
        fields.session_id = std::env::var("CLAUDE_SESSION_ID").ok();
    }

    let event = fields_to_log_event(&fields);
    forward_to_unified(event);
}

fn message_preview(text: Option<&str>) -> Option<String> {
    let trimmed = text.map(str::trim).unwrap_or_default();
    if trimmed.is_empty() {
        return None;
    }
    let mut chars = trimmed.chars();
    let preview: String = chars.by_ref().take(20).collect();
    if chars.next().is_some() {
        Some(format!("{preview}..."))
    } else {
        Some(preview)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serial_test::serial;

    /// Verify that `fields_to_log_event` does NOT set session_id to "unknown"
    /// when no session ID is available.  The LogEventV1 schema uses Option<String>
    /// and None is the correct value when no session is active.
    #[test]
    #[serial]
    fn test_fields_to_log_event_session_id_none_when_unset() {
        // Ensure the env var is absent for this test.
        // SAFETY: direct env manipulation — this test reads from_env directly.
        unsafe { std::env::remove_var("CLAUDE_SESSION_ID") };

        let fields = EventFields {
            level: "info",
            source: "atm",
            action: "test_action",
            session_id: None,
            ..Default::default()
        };
        let event = fields_to_log_event(&fields);
        assert!(
            event.session_id.is_none(),
            "session_id should be None when CLAUDE_SESSION_ID is unset and caller passed None"
        );
    }

    /// Verify that fields_to_log_event propagates a caller-supplied session_id.
    #[test]
    fn test_fields_to_log_event_session_id_propagated() {
        let fields = EventFields {
            level: "info",
            source: "atm",
            action: "test_action",
            session_id: Some("my-session-42".to_string()),
            ..Default::default()
        };
        let event = fields_to_log_event(&fields);
        assert_eq!(event.session_id.as_deref(), Some("my-session-42"));
    }

    #[test]
    fn test_fields_to_log_event_omits_message_text_for_privacy() {
        let fields = EventFields {
            level: "info",
            source: "atm",
            action: "send",
            message_text: Some("secret body".to_string()),
            ..Default::default()
        };
        let event = fields_to_log_event(&fields);
        assert!(
            !event.fields.contains_key("message_text"),
            "message_text should be intentionally omitted from persisted log fields"
        );
    }

    #[test]
    #[serial]
    fn test_logging_enabled_honors_disabled_values() {
        // SAFETY: test-scoped env mutation.
        unsafe { std::env::set_var("ATM_LOG", "0") };
        assert!(!logging_enabled());

        // SAFETY: test-scoped env mutation.
        unsafe { std::env::set_var("ATM_LOG", "false") };
        assert!(!logging_enabled());

        // SAFETY: test-scoped env mutation.
        unsafe { std::env::set_var("ATM_LOG", "off") };
        assert!(!logging_enabled());

        // SAFETY: test-scoped env mutation.
        unsafe { std::env::set_var("ATM_LOG", "disabled") };
        assert!(!logging_enabled());

        // SAFETY: test-scoped env mutation.
        unsafe { std::env::set_var("ATM_LOG", "no") };
        assert!(!logging_enabled());

        // SAFETY: cleanup.
        unsafe { std::env::remove_var("ATM_LOG") };
        assert!(logging_enabled());
    }

    #[test]
    #[serial]
    fn test_fields_to_log_event_send_includes_identity_pid_fields() {
        // SAFETY: test-scoped environment update.
        unsafe { std::env::remove_var("ATM_LOG_MSG") };
        let fields = EventFields {
            level: "info",
            source: "atm",
            action: "send",
            sender_agent: Some("team-lead".to_string()),
            sender_team: Some("atm-dev".to_string()),
            sender_pid: Some(44201),
            recipient_agent: Some("arch-ctm".to_string()),
            recipient_team: Some("atm-dev".to_string()),
            recipient_pid: Some(8009),
            ..Default::default()
        };
        let event = fields_to_log_event(&fields);
        assert_eq!(
            event.fields.get("sender_agent").and_then(|v| v.as_str()),
            Some("team-lead")
        );
        assert_eq!(
            event.fields.get("sender_pid").and_then(|v| v.as_u64()),
            Some(44201)
        );
        assert_eq!(
            event.fields.get("recipient_agent").and_then(|v| v.as_str()),
            Some("arch-ctm")
        );
        assert_eq!(
            event.fields.get("recipient_pid").and_then(|v| v.as_u64()),
            Some(8009)
        );
        assert!(
            event.fields.get("message_preview").is_none(),
            "preview must be omitted unless ATM_LOG_MSG=1"
        );
    }

    #[test]
    #[serial]
    fn test_fields_to_log_event_send_preview_gated_by_atm_log_msg() {
        // SAFETY: test-scoped environment update.
        unsafe { std::env::set_var("ATM_LOG_MSG", "1") };
        let fields = EventFields {
            level: "info",
            source: "atm",
            action: "send",
            message_text: Some("this message should be truncated for preview".to_string()),
            ..Default::default()
        };
        let event = fields_to_log_event(&fields);
        assert_eq!(
            event.fields.get("message_preview").and_then(|v| v.as_str()),
            Some("this message should ...")
        );

        // SAFETY: test-scoped environment update.
        unsafe { std::env::set_var("ATM_LOG_MSG", "truncated") };
        let no_preview_event = fields_to_log_event(&fields);
        assert!(
            no_preview_event.fields.get("message_preview").is_none(),
            "preview should only be emitted when ATM_LOG_MSG=1"
        );

        // SAFETY: test-scoped environment cleanup.
        unsafe { std::env::remove_var("ATM_LOG_MSG") };
    }

    #[test]
    #[serial]
    fn test_fields_to_log_event_send_preview_disabled_for_empty_atm_log_msg() {
        // SAFETY: test-scoped environment update.
        unsafe { std::env::set_var("ATM_LOG_MSG", "") };
        let fields = EventFields {
            level: "info",
            source: "atm",
            action: "send",
            message_text: Some("preview should remain disabled".to_string()),
            ..Default::default()
        };
        let event = fields_to_log_event(&fields);
        assert!(
            event.fields.get("message_preview").is_none(),
            "empty ATM_LOG_MSG must disable message preview"
        );
        // SAFETY: test-scoped environment cleanup.
        unsafe { std::env::remove_var("ATM_LOG_MSG") };
    }

    #[cfg(unix)]
    #[test]
    fn test_fields_to_log_event_includes_parent_pid_field_on_unix() {
        let fields = EventFields {
            level: "info",
            source: "atm",
            action: "test_action",
            ..Default::default()
        };
        let event = fields_to_log_event(&fields);
        assert!(
            event.fields.get("ppid").is_some(),
            "fields_to_log_event should include ppid field on unix"
        );
    }

    /// Verify that `emit_event_best_effort` is fail-open: calling it without
    /// an initialised unified channel must not panic.
    #[test]
    fn test_emit_event_best_effort_is_fail_open() {
        // No unified channel is registered in unit-test context; the call
        // should silently drop the event rather than panicking.
        emit_event_best_effort(EventFields {
            level: "info",
            source: "atm",
            action: "test_fail_open",
            team: Some("atm-dev".to_string()),
            session_id: Some("sess-123".to_string()),
            ..Default::default()
        });
    }

    /// Verify that `emit_event_best_effort` drops events with empty required fields.
    #[test]
    fn test_emit_event_best_effort_drops_empty_required_fields() {
        // level is empty — should return early without panicking.
        emit_event_best_effort(EventFields {
            level: "",
            source: "atm",
            action: "test_action",
            ..Default::default()
        });
        // source is empty
        emit_event_best_effort(EventFields {
            level: "info",
            source: "",
            action: "test_action",
            ..Default::default()
        });
        // action is empty
        emit_event_best_effort(EventFields {
            level: "info",
            source: "atm",
            action: "",
            ..Default::default()
        });
    }

    #[test]
    fn test_fields_to_log_event_includes_extra_fields() {
        let mut extra = serde_json::Map::new();
        extra.insert(
            "source".to_string(),
            serde_json::Value::String("daemon".to_string()),
        );
        extra.insert("pid".to_string(), serde_json::Value::Number(123_u64.into()));

        let fields = EventFields {
            level: "info",
            source: "atm",
            action: "state_change",
            extra_fields: extra,
            ..Default::default()
        };
        let event = fields_to_log_event(&fields);
        assert_eq!(
            event.fields.get("source").and_then(|v| v.as_str()),
            Some("daemon")
        );
        assert_eq!(event.fields.get("pid").and_then(|v| v.as_u64()), Some(123));
    }
}
