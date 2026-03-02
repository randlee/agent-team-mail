use agent_team_mail_core::log_reader::format_event_human;
use agent_team_mail_core::logging_event::new_log_event;

#[test]
fn new_log_event_sets_pid() {
    let event = new_log_event("atm", "send", "atm::send", "info");
    assert!(event.pid > 0, "pid must be present and non-zero");
}

#[cfg(unix)]
#[test]
fn new_log_event_sets_ppid_field_on_unix() {
    let event = new_log_event("atm", "send", "atm::send", "info");
    assert!(
        event.fields.get("ppid").and_then(|v| v.as_u64()).is_some(),
        "ppid should be emitted in fields on unix"
    );
}

#[test]
fn format_event_human_renders_pid() {
    let event = new_log_event("atm", "send", "atm::send", "info");
    let rendered = format_event_human(&event);
    assert!(rendered.contains("pid="), "human logs should render pid");
}

#[test]
fn format_event_human_renders_ppid_when_present() {
    let mut event = new_log_event("atm", "send", "atm::send", "info");
    event
        .fields
        .insert("ppid".to_string(), serde_json::Value::Number(321u64.into()));
    let rendered = format_event_human(&event);
    assert!(
        rendered.contains("ppid=321"),
        "human logs should render ppid when present"
    );
}

#[test]
fn format_event_human_omits_ppid_when_absent() {
    let mut event = new_log_event("atm", "send", "atm::send", "info");
    event.fields.remove("ppid");
    let rendered = format_event_human(&event);
    assert!(
        !rendered.contains("ppid="),
        "human logs should omit ppid when field is absent"
    );
}

#[test]
fn send_actions_use_agent_at_team_target_contract() {
    let mut send_event = new_log_event("atm", "send", "atm::send", "info");
    send_event.target = "team-lead@atm-dev".to_string();
    let send_rendered = format_event_human(&send_event);
    assert!(
        send_rendered.contains("-> team-lead@atm-dev"),
        "send target must render as agent@team"
    );

    let mut dry_run_event = new_log_event("atm", "send_dry_run", "atm::send", "info");
    dry_run_event.target = "team-lead@atm-dev".to_string();
    let dry_run_rendered = format_event_human(&dry_run_event);
    assert!(
        dry_run_rendered.contains("-> team-lead@atm-dev"),
        "send_dry_run target must render as agent@team"
    );
}
