use agent_team_mail_core::schema::InboxMessage;
use agent_team_mail_tui::app::{App, MemberRow};
use agent_team_mail_tui::config::TuiConfig;
use agent_team_mail_tui::ui::draw;
use ratatui::{Terminal, backend::TestBackend};
use std::collections::HashMap;

fn render_text(app: &App) -> String {
    let backend = TestBackend::new(100, 30);
    let mut terminal = Terminal::new(backend).expect("terminal");
    terminal.draw(|f| draw(f, app)).expect("draw");
    let buf = terminal.backend().buffer().clone();
    let mut out = String::new();
    for y in 0..buf.area.height {
        for x in 0..buf.area.width {
            out.push_str(buf[(x, y)].symbol());
        }
        out.push('\n');
    }
    out
}

fn sample_app() -> App {
    let mut app = App::new("atm-dev".to_string(), TuiConfig::default());
    app.members = vec![MemberRow {
        agent: "arch-ctm".to_string(),
        state: "busy".to_string(),
        inbox_count: 2,
    }];
    app.selected_index = 0;
    app.streaming_agent = Some("arch-ctm".to_string());
    app.inbox_messages = vec![
        InboxMessage {
            from: "team-lead".to_string(),
            text: "Please investigate CI failure and report findings.".to_string(),
            timestamp: "2026-03-02T00:00:00Z".to_string(),
            read: false,
            summary: Some("CI failure investigation".to_string()),
            message_id: Some("msg-1".to_string()),
            unknown_fields: HashMap::new(),
        },
        InboxMessage {
            from: "quality-mgr".to_string(),
            text: "Smoke tests passed.".to_string(),
            timestamp: "2026-03-02T00:01:00Z".to_string(),
            read: true,
            summary: Some("Smoke tests passed".to_string()),
            message_id: Some("msg-2".to_string()),
            unknown_fields: HashMap::new(),
        },
    ];
    app
}

#[test]
fn list_detail_mark_read_render_flow() {
    let mut app = sample_app();

    // 1) Inbox list render includes unread marker and summary rows.
    let list_render = render_text(&app);
    assert!(list_render.contains("team-lead"));
    assert!(list_render.contains("quality-mgr"));

    // 2) Detail panel render shows from/timestamp/status context.
    app.inbox_detail_open = true;
    app.selected_message_index = 0;
    let detail_render = render_text(&app);
    assert!(detail_render.contains("From: team-lead  [unread]"));
    assert!(detail_render.contains("At: 2026-03-02T00:00:00Z"));

    // 3) Mark read and assert rendered status updates.
    app.inbox_messages[0].read = true;
    let detail_after_mark_read = render_text(&app);
    assert!(detail_after_mark_read.contains("From: team-lead  [read]"));
}

#[test]
fn panel_consistency_uses_same_snapshot() {
    let mut app = sample_app();
    app.daemon_turn_state = Some(agent_team_mail_core::daemon_stream::AgentStreamState {
        turn_id: Some("turn-1".to_string()),
        thread_id: Some("thread-1".to_string()),
        transport: Some("cli".to_string()),
        turn_status: agent_team_mail_core::daemon_stream::StreamTurnStatus::Busy,
    });

    let rendered = render_text(&app);
    assert!(rendered.contains("arch-ctm"));
    assert!(rendered.contains("busy"));
    assert!(rendered.contains("[LIVE]"));
}
