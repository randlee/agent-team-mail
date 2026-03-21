#![cfg(unix)]
//! Integration tests for the CI Monitor plugin

use agent_team_mail_core::config::Config;
use agent_team_mail_core::context::{GitProvider, Platform, RepoContext, SystemContext};
use agent_team_mail_core::schema::InboxMessage;
use agent_team_mail_daemon::plugin::{MailService, Plugin, PluginContext};
use agent_team_mail_daemon::plugins::ci_monitor::mock_support::{
    MockCall, MockCiProvider, create_test_job, create_test_run,
};
use agent_team_mail_daemon::plugins::ci_monitor::{CiMonitorPlugin, CiRunConclusion, CiRunStatus};
use agent_team_mail_daemon::roster::RosterService;
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;
use tempfile::TempDir;
use tokio::time::timeout;
use tokio_util::sync::CancellationToken;

/// Helper to create a test PluginContext with mock provider injection
fn create_test_context(temp_dir: &TempDir, provider: Option<GitProvider>) -> PluginContext {
    // ATM_HOME is set by the test runner for the whole test process
    // Individual tests should not mutate it (unsafe and causes races)
    // Instead, construct paths directly from temp_dir

    let claude_root = temp_dir.path().join(".claude");
    let teams_root = claude_root.join("teams");
    std::fs::create_dir_all(&teams_root).unwrap();

    let mut system = SystemContext::new(
        "test-host".to_string(),
        Platform::Linux,
        claude_root.clone(),
        temp_dir.path().to_path_buf(),
        "2.0.0".to_string(),
        "test-team".to_string(),
    );

    if let Some(git_provider) = provider {
        let repo = RepoContext::new("test-repo".to_string(), temp_dir.path().to_path_buf());
        let mut repo = repo.with_remote("https://github.com/test/repo.git".to_string());
        repo.provider = Some(git_provider);
        system = system.with_repo(repo);
    }

    let system = Arc::new(system);

    let mail = Arc::new(MailService::new(teams_root.clone()));

    let mut config = Config::default();
    config.core.default_team = "test-team".to_string();
    let config = Arc::new(config);

    let roster = Arc::new(RosterService::new(teams_root));

    PluginContext::new(system, mail, config, roster)
}

/// Helper to create a team config for testing
fn create_team_config(teams_root: &Path, team_name: &str) {
    let team_dir = teams_root.join(team_name);
    std::fs::create_dir_all(&team_dir).unwrap();

    let config = serde_json::json!({
        "name": team_name,
        "description": "Test team",
        "createdAt": 1739284800000i64,
        "leadAgentId": format!("team-lead@{}", team_name),
        "leadSessionId": "test-session-id",
        "members": []
    });

    std::fs::write(
        team_dir.join("config.json"),
        serde_json::to_string_pretty(&config).unwrap(),
    )
    .unwrap();
}

/// Helper to read inbox messages
fn read_inbox(teams_root: &Path, team: &str, agent: &str) -> Vec<InboxMessage> {
    let inbox_path = teams_root
        .join(team)
        .join("inboxes")
        .join(format!("{agent}.json"));

    if !inbox_path.exists() {
        return Vec::new();
    }

    let content = std::fs::read(&inbox_path).unwrap();
    serde_json::from_slice(&content).unwrap()
}

async fn wait_for_condition(
    timeout_ms: u64,
    mut pred: impl FnMut() -> bool,
) -> std::time::Duration {
    let start = std::time::Instant::now();
    let deadline = start + Duration::from_millis(timeout_ms);
    loop {
        if pred() {
            return start.elapsed();
        }
        assert!(
            std::time::Instant::now() < deadline,
            "condition not met within {timeout_ms}ms"
        );
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
}

#[tokio::test]
async fn test_ci_failure_delivers_inbox_message() {
    let temp_dir = TempDir::new().unwrap();

    // Create provider with one failed run
    let failed_job = create_test_job(
        201,
        "build",
        CiRunStatus::Completed,
        Some(CiRunConclusion::Failure),
    );

    let mut failed_run = create_test_run(
        101,
        "CI",
        "main",
        CiRunStatus::Completed,
        Some(CiRunConclusion::Failure),
    );
    failed_run.jobs = Some(vec![failed_job]);

    let mock_provider = MockCiProvider::with_runs(vec![failed_run.clone()]);

    // Create context with GitHub provider
    let git_provider = GitProvider::GitHub {
        owner: "test".to_string(),
        repo: "repo".to_string(),
    };
    let mut ctx = create_test_context(&temp_dir, Some(git_provider));

    // Add plugin config to context
    let mut plugin_config = toml::Table::new();
    plugin_config.insert("enabled".to_string(), toml::Value::Boolean(true));
    plugin_config.insert("poll_interval_secs".to_string(), toml::Value::Integer(10)); // Minimum 10 seconds
    plugin_config.insert(
        "team".to_string(),
        toml::Value::String("test-team".to_string()),
    );
    plugin_config.insert(
        "agent".to_string(),
        toml::Value::String("ci-monitor".to_string()),
    );

    let mut config = (*ctx.config).clone();
    config
        .plugins
        .insert("gh_monitor".to_string(), plugin_config);
    ctx = PluginContext::new(
        ctx.system.clone(),
        ctx.mail.clone(),
        Arc::new(config),
        ctx.roster.clone(),
    );

    // Create team structure
    create_team_config(ctx.mail.teams_root(), "test-team");

    // Create plugin with injected provider
    let mut plugin = CiMonitorPlugin::new().with_provider(Box::new(mock_provider));

    // Init plugin (will skip provider creation since we injected one)
    plugin.init(&ctx).await.unwrap();

    // Run plugin briefly (one poll cycle)
    let cancel = CancellationToken::new();
    let run_cancel = cancel.clone();
    let run_task = tokio::spawn(async move { plugin.run(run_cancel).await });
    let observed_delivery = wait_for_condition(2_000, || {
        !read_inbox(ctx.mail.teams_root(), "test-team", "team-lead").is_empty()
    })
    .await;
    assert!(
        observed_delivery <= Duration::from_secs(2),
        "CI failure delivery should stay bounded: elapsed={observed_delivery:?}"
    );
    cancel.cancel();
    let _ = timeout(Duration::from_secs(2), run_task)
        .await
        .expect("plugin run should exit after cancellation")
        .unwrap();

    // Verify inbox message was delivered
    let messages = read_inbox(ctx.mail.teams_root(), "test-team", "team-lead");
    assert!(
        !messages.is_empty(),
        "Should have at least one message for CI failure"
    );

    let msg = &messages[0];
    assert!(
        msg.text.contains("[ci:101]"),
        "Message should have CI reference"
    );
    assert!(
        msg.text.contains("failed"),
        "Message should indicate failure"
    );
    assert!(
        msg.text.contains("build"),
        "Message should mention failed job"
    );
    assert_eq!(msg.from, "ci-monitor");
}

#[tokio::test(start_paused = true)]
async fn test_ci_deduplication() {
    let temp_dir = TempDir::new().unwrap();

    // Create provider with same failed run (will be polled twice)
    let failed_run = create_test_run(
        102,
        "CI",
        "main",
        CiRunStatus::Completed,
        Some(CiRunConclusion::Failure),
    );

    let mock_provider = MockCiProvider::with_runs(vec![failed_run.clone()]);
    let provider_clone = mock_provider.clone();

    let git_provider = GitProvider::GitHub {
        owner: "test".to_string(),
        repo: "repo".to_string(),
    };
    let mut ctx = create_test_context(&temp_dir, Some(git_provider));

    // Add plugin config
    let mut plugin_config = toml::Table::new();
    plugin_config.insert("enabled".to_string(), toml::Value::Boolean(true));
    plugin_config.insert("poll_interval_secs".to_string(), toml::Value::Integer(10));
    plugin_config.insert(
        "team".to_string(),
        toml::Value::String("test-team".to_string()),
    );
    plugin_config.insert(
        "agent".to_string(),
        toml::Value::String("ci-monitor".to_string()),
    );

    let mut config = (*ctx.config).clone();
    config
        .plugins
        .insert("gh_monitor".to_string(), plugin_config);
    ctx = PluginContext::new(
        ctx.system.clone(),
        ctx.mail.clone(),
        Arc::new(config),
        ctx.roster.clone(),
    );

    create_team_config(ctx.mail.teams_root(), "test-team");

    // Use with_config to override poll interval for fast testing
    let test_config = agent_team_mail_daemon::plugins::ci_monitor::CiMonitorConfig {
        enabled: true,
        provider: "github".to_string(),
        team: "test-team".to_string(),
        agent: "ci-monitor".to_string(),
        poll_interval_secs: 10, // Min 10 seconds
        watched_branches: Vec::new(),
        notify_on: vec![
            agent_team_mail_daemon::plugins::ci_monitor::CiRunConclusion::Failure,
            agent_team_mail_daemon::plugins::ci_monitor::CiRunConclusion::TimedOut,
        ],
        owner: None,
        repo: None,
        provider_libraries: std::collections::HashMap::new(),
        dedup_strategy: agent_team_mail_daemon::plugins::ci_monitor::DedupStrategy::PerCommit,
        dedup_ttl_hours: 24,
        report_dir: std::path::PathBuf::from("temp/atm/ci-monitor"),
        runtime_drift_enabled: false,
        runtime_drift_threshold_percent: 50,
        runtime_drift_min_samples: 3,
        runtime_history_limit: 50,
        alert_cooldown_secs: 300,
        provider_config: None,
        notify_target: Vec::new(),
        branch_matcher: None,
    };

    let mut plugin = CiMonitorPlugin::new()
        .with_provider(Box::new(mock_provider))
        .with_config(test_config);
    plugin.init(&ctx).await.unwrap();

    // Run with simulated time so this test stays fast and deterministic.
    let cancel = CancellationToken::new();
    let run_cancel = cancel.clone();
    let run_task = tokio::spawn(async move { plugin.run(run_cancel).await });

    // Drive virtual time until we've observed at least two poll cycles.
    for _ in 0..4 {
        tokio::task::yield_now().await;
        tokio::time::advance(Duration::from_secs(10)).await;
        tokio::task::yield_now().await;
        let list_calls = provider_clone
            .get_calls()
            .iter()
            .filter(|c| matches!(c, MockCall::ListRuns(_)))
            .count();
        if list_calls >= 2 {
            break;
        }
    }

    cancel.cancel();
    tokio::task::yield_now().await;

    let _ = run_task.await.unwrap();

    // Verify list_runs and get_run were called multiple times
    let calls = provider_clone.get_calls();
    let list_calls = calls
        .iter()
        .filter(|c| matches!(c, MockCall::ListRuns(_)))
        .count();
    let _get_calls = calls
        .iter()
        .filter(|c| matches!(c, MockCall::GetRun(_)))
        .count();

    assert!(list_calls >= 2, "Should poll at least twice");

    // But only ONE inbox message should be delivered (deduplication)
    let messages = read_inbox(ctx.mail.teams_root(), "test-team", "team-lead");
    assert_eq!(
        messages.len(),
        1,
        "Should only deliver one message despite multiple polls"
    );
}

#[tokio::test]
async fn test_status_transition_notification() {
    let temp_dir = TempDir::new().unwrap();

    // First poll: run is in progress
    let in_progress_run = create_test_run(103, "CI", "main", CiRunStatus::InProgress, None);
    let mock_provider_v1 = MockCiProvider::with_runs(vec![in_progress_run]);
    let provider_v1_clone = mock_provider_v1.clone();

    // Second poll: run has failed
    let failed_run = create_test_run(
        103,
        "CI",
        "main",
        CiRunStatus::Completed,
        Some(CiRunConclusion::Failure),
    );
    let mock_provider_v2 = MockCiProvider::with_runs(vec![failed_run]);

    let git_provider = GitProvider::GitHub {
        owner: "test".to_string(),
        repo: "repo".to_string(),
    };
    let mut ctx = create_test_context(&temp_dir, Some(git_provider));

    let mut plugin_config = toml::Table::new();
    plugin_config.insert("enabled".to_string(), toml::Value::Boolean(true));
    plugin_config.insert("poll_interval_secs".to_string(), toml::Value::Integer(10));
    plugin_config.insert(
        "team".to_string(),
        toml::Value::String("test-team".to_string()),
    );
    plugin_config.insert(
        "agent".to_string(),
        toml::Value::String("ci-monitor".to_string()),
    );

    let mut config = (*ctx.config).clone();
    config
        .plugins
        .insert("gh_monitor".to_string(), plugin_config);
    ctx = PluginContext::new(
        ctx.system.clone(),
        ctx.mail.clone(),
        Arc::new(config),
        ctx.roster.clone(),
    );

    create_team_config(ctx.mail.teams_root(), "test-team");

    // Run plugin with in-progress run
    let mut plugin1 = CiMonitorPlugin::new().with_provider(Box::new(mock_provider_v1));
    plugin1.init(&ctx).await.unwrap();

    let cancel1 = CancellationToken::new();
    let run_cancel1 = cancel1.clone();
    let run_task1 = tokio::spawn(async move { plugin1.run(run_cancel1).await });
    let first_poll_elapsed = wait_for_condition(2_000, || {
        provider_v1_clone
            .get_calls()
            .iter()
            .any(|call| matches!(call, MockCall::ListRuns(_)))
    })
    .await;
    assert!(
        first_poll_elapsed <= Duration::from_secs(2),
        "in-progress poll should start promptly: elapsed={first_poll_elapsed:?}"
    );
    cancel1.cancel();
    let _ = timeout(Duration::from_secs(2), run_task1)
        .await
        .expect("in-progress plugin run should exit after cancellation")
        .unwrap();

    // No messages yet (in-progress doesn't trigger notification)
    let messages = read_inbox(ctx.mail.teams_root(), "test-team", "ci-monitor");
    assert_eq!(
        messages.len(),
        0,
        "In-progress run should not trigger notification"
    );

    // Remove synthetic member before re-init
    ctx.roster
        .cleanup_plugin(
            "test-team",
            "gh_monitor",
            agent_team_mail_daemon::roster::CleanupMode::Hard,
        )
        .unwrap();

    // Run plugin again with failed run
    let mut plugin2 = CiMonitorPlugin::new().with_provider(Box::new(mock_provider_v2));
    plugin2.init(&ctx).await.unwrap();

    let cancel2 = CancellationToken::new();
    let run_cancel2 = cancel2.clone();
    let run_task2 = tokio::spawn(async move { plugin2.run(run_cancel2).await });
    let failure_elapsed = wait_for_condition(2_000, || {
        read_inbox(ctx.mail.teams_root(), "test-team", "team-lead").len() == 1
    })
    .await;
    assert!(
        failure_elapsed <= Duration::from_secs(2),
        "failure transition notification should stay bounded: elapsed={failure_elapsed:?}"
    );
    cancel2.cancel();
    let _ = timeout(Duration::from_secs(2), run_task2)
        .await
        .expect("failure plugin run should exit after cancellation")
        .unwrap();

    // Now we should have exactly one notification for the failure
    let messages = read_inbox(ctx.mail.teams_root(), "test-team", "team-lead");
    assert_eq!(
        messages.len(),
        1,
        "Should deliver notification when run transitions to failure"
    );
}

#[tokio::test(start_paused = true)]
async fn test_runtime_drift_alert_persisted_across_restart() {
    let temp_dir = TempDir::new().unwrap();

    let git_provider = GitProvider::GitHub {
        owner: "test".to_string(),
        repo: "repo".to_string(),
    };
    let mut ctx = create_test_context(&temp_dir, Some(git_provider));
    create_team_config(ctx.mail.teams_root(), "test-team");

    let mut plugin_config = toml::Table::new();
    plugin_config.insert("enabled".to_string(), toml::Value::Boolean(true));
    plugin_config.insert("poll_interval_secs".to_string(), toml::Value::Integer(10));
    plugin_config.insert(
        "team".to_string(),
        toml::Value::String("test-team".to_string()),
    );
    plugin_config.insert(
        "agent".to_string(),
        toml::Value::String("ci-monitor".to_string()),
    );
    plugin_config.insert(
        "runtime_drift_enabled".to_string(),
        toml::Value::Boolean(true),
    );
    plugin_config.insert(
        "runtime_drift_threshold_percent".to_string(),
        toml::Value::Integer(50),
    );
    plugin_config.insert(
        "runtime_drift_min_samples".to_string(),
        toml::Value::Integer(1),
    );
    plugin_config.insert(
        "runtime_history_limit".to_string(),
        toml::Value::Integer(20),
    );

    let mut config = (*ctx.config).clone();
    config
        .plugins
        .insert("gh_monitor".to_string(), plugin_config);
    ctx = PluginContext::new(
        ctx.system.clone(),
        ctx.mail.clone(),
        Arc::new(config),
        ctx.roster.clone(),
    );

    let mut baseline_job = create_test_job(
        901,
        "build",
        CiRunStatus::Completed,
        Some(CiRunConclusion::Failure),
    );
    baseline_job.started_at = Some("2026-02-13T10:01:00Z".to_string());
    baseline_job.completed_at = Some("2026-02-13T10:04:00Z".to_string()); // 180s
    let mut baseline_run = create_test_run(
        901,
        "CI",
        "main",
        CiRunStatus::Completed,
        Some(CiRunConclusion::Failure),
    );
    baseline_run.created_at = "2026-02-13T10:00:00Z".to_string();
    baseline_run.updated_at = "2026-02-13T10:05:00Z".to_string(); // 300s
    baseline_run.jobs = Some(vec![baseline_job]);
    let replayed_baseline_run = baseline_run.clone();

    let mut slow_job = create_test_job(
        902,
        "build",
        CiRunStatus::Completed,
        Some(CiRunConclusion::Failure),
    );
    slow_job.started_at = Some("2026-02-13T11:01:00Z".to_string());
    slow_job.completed_at = Some("2026-02-13T11:09:00Z".to_string()); // 480s (> +50%)
    let mut slow_run = create_test_run(
        902,
        "CI",
        "main",
        CiRunStatus::Completed,
        Some(CiRunConclusion::Failure),
    );
    slow_run.created_at = "2026-02-13T11:00:00Z".to_string();
    slow_run.updated_at = "2026-02-13T11:15:00Z".to_string(); // 900s (> +50%)
    slow_run.jobs = Some(vec![slow_job]);

    // First process start: persist baseline history from run #901.
    let provider_v1 = MockCiProvider::with_runs(vec![baseline_run]);
    let mut plugin_v1 = CiMonitorPlugin::new().with_provider(Box::new(provider_v1));
    plugin_v1.init(&ctx).await.unwrap();
    let cancel_v1 = CancellationToken::new();
    let run_cancel_v1 = cancel_v1.clone();
    let run_task_v1 = tokio::spawn(async move { plugin_v1.run(run_cancel_v1).await });
    tokio::task::yield_now().await;
    tokio::time::advance(Duration::from_secs(10)).await;
    tokio::task::yield_now().await;
    cancel_v1.cancel();
    tokio::task::yield_now().await;
    let _ = run_task_v1.await.unwrap();

    // Restart plugin process and feed run #901 (already processed) and #902.
    // Run #901 must not produce a duplicate drift alert after restart.
    ctx.roster
        .cleanup_plugin(
            "test-team",
            "gh_monitor",
            agent_team_mail_daemon::roster::CleanupMode::Hard,
        )
        .unwrap();
    let provider_v2 = MockCiProvider::with_runs(vec![replayed_baseline_run, slow_run]);
    let mut plugin_v2 = CiMonitorPlugin::new().with_provider(Box::new(provider_v2));
    plugin_v2.init(&ctx).await.unwrap();
    let cancel_v2 = CancellationToken::new();
    let run_cancel_v2 = cancel_v2.clone();
    let run_task_v2 = tokio::spawn(async move { plugin_v2.run(run_cancel_v2).await });
    tokio::task::yield_now().await;
    tokio::time::advance(Duration::from_secs(10)).await;
    tokio::task::yield_now().await;
    cancel_v2.cancel();
    tokio::task::yield_now().await;
    let _ = run_task_v2.await.unwrap();

    let messages = read_inbox(ctx.mail.teams_root(), "test-team", "team-lead");
    let drift_messages: Vec<_> = messages
        .iter()
        .filter(|m| m.text.contains("[runtime-drift:"))
        .collect();
    assert_eq!(
        drift_messages.len(),
        1,
        "Expected exactly one runtime drift alert across restart replay"
    );
    assert!(
        drift_messages[0].text.contains("[runtime-drift:902]"),
        "Expected persisted-baseline runtime drift alert for run 902"
    );
    assert!(
        !drift_messages
            .iter()
            .any(|m| m.text.contains("[runtime-drift:901]")),
        "Run 901 replay after restart must not generate a duplicate drift alert"
    );

    let history_path = temp_dir
        .path()
        .join("temp/atm/ci-monitor/runtime-history.json");
    assert!(
        history_path.exists(),
        "runtime history file should be persisted"
    );
    let history: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&history_path).unwrap()).unwrap();
    assert_eq!(
        history["workflow_samples"]["CI"].as_array().unwrap().len(),
        2,
        "workflow baseline should include both samples across restart"
    );
    assert_eq!(
        history["processed_run_ids"].as_array().unwrap().len(),
        2,
        "processed run ids should persist across restart"
    );
}

#[tokio::test]
async fn test_multiple_failures() {
    let temp_dir = TempDir::new().unwrap();

    // Multiple failed runs
    let runs = vec![
        create_test_run(
            201,
            "CI",
            "main",
            CiRunStatus::Completed,
            Some(CiRunConclusion::Failure),
        ),
        create_test_run(
            202,
            "CI",
            "main",
            CiRunStatus::Completed,
            Some(CiRunConclusion::Failure),
        ),
        create_test_run(
            203,
            "CI",
            "main",
            CiRunStatus::Completed,
            Some(CiRunConclusion::TimedOut),
        ),
    ];

    let mock_provider = MockCiProvider::with_runs(runs);

    let git_provider = GitProvider::GitHub {
        owner: "test".to_string(),
        repo: "repo".to_string(),
    };
    let mut ctx = create_test_context(&temp_dir, Some(git_provider));

    let mut plugin_config = toml::Table::new();
    plugin_config.insert("enabled".to_string(), toml::Value::Boolean(true));
    plugin_config.insert("poll_interval_secs".to_string(), toml::Value::Integer(10));
    plugin_config.insert(
        "team".to_string(),
        toml::Value::String("test-team".to_string()),
    );
    plugin_config.insert(
        "agent".to_string(),
        toml::Value::String("ci-monitor".to_string()),
    );

    let mut config = (*ctx.config).clone();
    config
        .plugins
        .insert("gh_monitor".to_string(), plugin_config);
    ctx = PluginContext::new(
        ctx.system.clone(),
        ctx.mail.clone(),
        Arc::new(config),
        ctx.roster.clone(),
    );

    create_team_config(ctx.mail.teams_root(), "test-team");

    let mut plugin = CiMonitorPlugin::new().with_provider(Box::new(mock_provider));
    plugin.init(&ctx).await.unwrap();

    let cancel = CancellationToken::new();
    let run_cancel = cancel.clone();
    let run_task = tokio::spawn(async move { plugin.run(run_cancel).await });
    let delivery_elapsed = wait_for_condition(2_000, || {
        read_inbox(ctx.mail.teams_root(), "test-team", "team-lead").len() == 3
    })
    .await;
    assert!(
        delivery_elapsed <= Duration::from_secs(2),
        "multiple-failure notifications should stay bounded: elapsed={delivery_elapsed:?}"
    );
    cancel.cancel();
    let _ = timeout(Duration::from_secs(2), run_task)
        .await
        .expect("multi-failure plugin run should exit after cancellation")
        .unwrap();

    // Should have one message per failure
    let messages = read_inbox(ctx.mail.teams_root(), "test-team", "team-lead");
    assert_eq!(messages.len(), 3, "Should deliver one message per failure");

    // Verify each run ID is present
    assert!(messages.iter().any(|m| m.text.contains("[ci:201]")));
    assert!(messages.iter().any(|m| m.text.contains("[ci:202]")));
    assert!(messages.iter().any(|m| m.text.contains("[ci:203]")));
}

#[tokio::test]
async fn test_branch_filtering() {
    let temp_dir = TempDir::new().unwrap();

    // Runs on different branches
    let runs = vec![
        create_test_run(
            301,
            "CI",
            "main",
            CiRunStatus::Completed,
            Some(CiRunConclusion::Failure),
        ),
        create_test_run(
            302,
            "CI",
            "develop",
            CiRunStatus::Completed,
            Some(CiRunConclusion::Failure),
        ),
        create_test_run(
            303,
            "CI",
            "feature-x",
            CiRunStatus::Completed,
            Some(CiRunConclusion::Failure),
        ),
    ];

    let mock_provider = MockCiProvider::with_runs(runs);
    let provider_clone = mock_provider.clone();

    let git_provider = GitProvider::GitHub {
        owner: "test".to_string(),
        repo: "repo".to_string(),
    };
    let mut ctx = create_test_context(&temp_dir, Some(git_provider));

    // Configure to only watch "main" and "develop"
    let mut plugin_config = toml::Table::new();
    plugin_config.insert("enabled".to_string(), toml::Value::Boolean(true));
    plugin_config.insert("poll_interval_secs".to_string(), toml::Value::Integer(10));
    plugin_config.insert(
        "watched_branches".to_string(),
        toml::Value::Array(vec![
            toml::Value::String("main".to_string()),
            toml::Value::String("develop".to_string()),
        ]),
    );
    plugin_config.insert(
        "team".to_string(),
        toml::Value::String("test-team".to_string()),
    );
    plugin_config.insert(
        "agent".to_string(),
        toml::Value::String("ci-monitor".to_string()),
    );

    let mut config = (*ctx.config).clone();
    config
        .plugins
        .insert("gh_monitor".to_string(), plugin_config);
    ctx = PluginContext::new(
        ctx.system.clone(),
        ctx.mail.clone(),
        Arc::new(config),
        ctx.roster.clone(),
    );

    create_team_config(ctx.mail.teams_root(), "test-team");

    let mut plugin = CiMonitorPlugin::new().with_provider(Box::new(mock_provider));
    plugin.init(&ctx).await.unwrap();

    let cancel = CancellationToken::new();
    let run_cancel = cancel.clone();
    let run_task = tokio::spawn(async move { plugin.run(run_cancel).await });
    let filter_elapsed = wait_for_condition(2_000, || {
        read_inbox(ctx.mail.teams_root(), "test-team", "team-lead").len() == 2
    })
    .await;
    assert!(
        filter_elapsed <= Duration::from_secs(2),
        "branch filtering should produce bounded notifications: elapsed={filter_elapsed:?}"
    );
    cancel.cancel();
    let _ = timeout(Duration::from_secs(2), run_task)
        .await
        .expect("branch filter plugin run should exit after cancellation")
        .unwrap();

    // Verify list_runs was called (client-side filtering, so no branch filter in API call)
    let calls = provider_clone.get_calls();
    let list_calls: Vec<_> = calls
        .iter()
        .filter_map(|c| match c {
            MockCall::ListRuns(filter) => Some(filter.clone()),
            _ => None,
        })
        .collect();

    assert!(!list_calls.is_empty());
    // After Sprint 9.3: Branch filtering is done client-side using glob patterns
    // API calls fetch all branches without branch filter
    for filter in &list_calls {
        assert!(
            filter.branch.is_none(),
            "Branch filtering should be client-side"
        );
    }

    // Only two messages (main and develop, not feature-x)
    // Client-side glob matching filters out feature-x
    let messages = read_inbox(ctx.mail.teams_root(), "test-team", "team-lead");
    assert_eq!(messages.len(), 2, "Should only notify for watched branches");
    assert!(messages.iter().any(|m| m.text.contains("[ci:301]")));
    assert!(messages.iter().any(|m| m.text.contains("[ci:302]")));
    assert!(!messages.iter().any(|m| m.text.contains("[ci:303]")));
}

#[tokio::test]
async fn test_conclusion_filtering() {
    let temp_dir = TempDir::new().unwrap();

    // Runs with different conclusions
    let runs = vec![
        create_test_run(
            401,
            "CI",
            "main",
            CiRunStatus::Completed,
            Some(CiRunConclusion::Success),
        ),
        create_test_run(
            402,
            "CI",
            "main",
            CiRunStatus::Completed,
            Some(CiRunConclusion::Failure),
        ),
        create_test_run(
            403,
            "CI",
            "main",
            CiRunStatus::Completed,
            Some(CiRunConclusion::Cancelled),
        ),
    ];

    let mock_provider = MockCiProvider::with_runs(runs);

    let git_provider = GitProvider::GitHub {
        owner: "test".to_string(),
        repo: "repo".to_string(),
    };
    let mut ctx = create_test_context(&temp_dir, Some(git_provider));

    // Default config notifies only on Failure and TimedOut
    let mut plugin_config = toml::Table::new();
    plugin_config.insert("enabled".to_string(), toml::Value::Boolean(true));
    plugin_config.insert("poll_interval_secs".to_string(), toml::Value::Integer(10));
    plugin_config.insert(
        "team".to_string(),
        toml::Value::String("test-team".to_string()),
    );
    plugin_config.insert(
        "agent".to_string(),
        toml::Value::String("ci-monitor".to_string()),
    );

    let mut config = (*ctx.config).clone();
    config
        .plugins
        .insert("gh_monitor".to_string(), plugin_config);
    ctx = PluginContext::new(
        ctx.system.clone(),
        ctx.mail.clone(),
        Arc::new(config),
        ctx.roster.clone(),
    );

    create_team_config(ctx.mail.teams_root(), "test-team");

    let mut plugin = CiMonitorPlugin::new().with_provider(Box::new(mock_provider));
    plugin.init(&ctx).await.unwrap();

    let cancel = CancellationToken::new();
    let run_cancel = cancel.clone();
    let run_task = tokio::spawn(async move { plugin.run(run_cancel).await });
    let filter_elapsed = wait_for_condition(2_000, || {
        read_inbox(ctx.mail.teams_root(), "test-team", "team-lead").len() == 1
    })
    .await;
    assert!(
        filter_elapsed <= Duration::from_secs(2),
        "conclusion filtering should produce bounded notifications: elapsed={filter_elapsed:?}"
    );
    cancel.cancel();
    let _ = timeout(Duration::from_secs(2), run_task)
        .await
        .expect("conclusion filter plugin run should exit after cancellation")
        .unwrap();

    // Only failure should be notified (not success or cancelled)
    let messages = read_inbox(ctx.mail.teams_root(), "test-team", "team-lead");
    assert_eq!(
        messages.len(),
        1,
        "Should only notify for configured conclusions"
    );
    assert!(messages.iter().any(|m| m.text.contains("[ci:402]")));
}

#[tokio::test]
async fn test_synthetic_member_lifecycle() {
    let temp_dir = TempDir::new().unwrap();

    let git_provider = GitProvider::GitHub {
        owner: "test".to_string(),
        repo: "repo".to_string(),
    };
    let mut ctx = create_test_context(&temp_dir, Some(git_provider));

    let team_name = "test-team";
    create_team_config(ctx.mail.teams_root(), team_name);

    // Add minimal plugin config
    let mut plugin_config = toml::Table::new();
    plugin_config.insert("enabled".to_string(), toml::Value::Boolean(true));
    plugin_config.insert(
        "team".to_string(),
        toml::Value::String("test-team".to_string()),
    );
    plugin_config.insert(
        "agent".to_string(),
        toml::Value::String("ci-monitor".to_string()),
    );

    let mut config = (*ctx.config).clone();
    config
        .plugins
        .insert("gh_monitor".to_string(), plugin_config);
    ctx = PluginContext::new(
        ctx.system.clone(),
        ctx.mail.clone(),
        Arc::new(config),
        ctx.roster.clone(),
    );

    // Create and init plugin
    let mut plugin = CiMonitorPlugin::new();
    plugin.init(&ctx).await.unwrap();

    // Verify synthetic member was registered
    let config_path = ctx.mail.teams_root().join(team_name).join("config.json");
    let config_content = std::fs::read_to_string(&config_path).unwrap();
    let config: serde_json::Value = serde_json::from_str(&config_content).unwrap();

    let members = config["members"].as_array().unwrap();
    let ci_monitor = members
        .iter()
        .find(|m| m["name"].as_str() == Some("ci-monitor"));

    assert!(ci_monitor.is_some(), "ci-monitor should be registered");
    let bot = ci_monitor.unwrap();
    assert_eq!(bot["agentType"].as_str().unwrap(), "plugin:gh_monitor");
    assert_eq!(bot["isActive"].as_bool(), Some(true));

    // Shutdown plugin
    plugin.shutdown().await.unwrap();

    // Verify member is marked inactive (soft cleanup)
    let config_content = std::fs::read_to_string(&config_path).unwrap();
    let config: serde_json::Value = serde_json::from_str(&config_content).unwrap();

    let members = config["members"].as_array().unwrap();
    let ci_monitor = members
        .iter()
        .find(|m| m["name"].as_str() == Some("ci-monitor"));

    assert!(ci_monitor.is_some(), "ci-monitor should still exist");
    assert_eq!(ci_monitor.unwrap()["isActive"].as_bool(), Some(false));
}

#[tokio::test]
async fn test_disabled_plugin_skips_init() {
    let temp_dir = TempDir::new().unwrap();

    let git_provider = GitProvider::GitHub {
        owner: "test".to_string(),
        repo: "repo".to_string(),
    };
    let mut ctx = create_test_context(&temp_dir, Some(git_provider));

    create_team_config(ctx.mail.teams_root(), "test-team");

    // Configure plugin as disabled (but still needs team/agent for config validation)
    let mut plugin_config = toml::Table::new();
    plugin_config.insert("enabled".to_string(), toml::Value::Boolean(false));
    plugin_config.insert(
        "team".to_string(),
        toml::Value::String("test-team".to_string()),
    );
    plugin_config.insert(
        "agent".to_string(),
        toml::Value::String("ci-monitor".to_string()),
    );

    let mut config = (*ctx.config).clone();
    config
        .plugins
        .insert("gh_monitor".to_string(), plugin_config);
    ctx = PluginContext::new(
        ctx.system.clone(),
        ctx.mail.clone(),
        Arc::new(config),
        ctx.roster.clone(),
    );

    // Create and init plugin
    let mut plugin = CiMonitorPlugin::new();
    plugin.init(&ctx).await.unwrap();

    // Run plugin briefly - should return quickly
    let cancel = CancellationToken::new();
    cancel.cancel(); // Cancel immediately

    let result = timeout(Duration::from_millis(100), plugin.run(cancel)).await;
    assert!(result.is_ok(), "Disabled plugin should return quickly");
    assert!(result.unwrap().is_ok());

    // Verify no synthetic member was registered
    let config_path = ctx.mail.teams_root().join("test-team").join("config.json");
    let config_content = std::fs::read_to_string(&config_path).unwrap();
    let config: serde_json::Value = serde_json::from_str(&config_content).unwrap();

    let members = config["members"].as_array().unwrap();
    assert!(
        members.is_empty(),
        "No members should be registered when disabled"
    );
}

#[tokio::test]
async fn test_full_lifecycle_init_run_shutdown() {
    let temp_dir = TempDir::new().unwrap();

    let git_provider = GitProvider::GitHub {
        owner: "test".to_string(),
        repo: "repo".to_string(),
    };
    let mut ctx = create_test_context(&temp_dir, Some(git_provider));

    create_team_config(ctx.mail.teams_root(), "test-team");

    // Add minimal plugin config
    let mut plugin_config = toml::Table::new();
    plugin_config.insert("enabled".to_string(), toml::Value::Boolean(true));
    plugin_config.insert(
        "team".to_string(),
        toml::Value::String("test-team".to_string()),
    );
    plugin_config.insert(
        "agent".to_string(),
        toml::Value::String("ci-monitor".to_string()),
    );

    let mut config = (*ctx.config).clone();
    config
        .plugins
        .insert("gh_monitor".to_string(), plugin_config);
    ctx = PluginContext::new(
        ctx.system.clone(),
        ctx.mail.clone(),
        Arc::new(config),
        ctx.roster.clone(),
    );

    // Create and init plugin
    let mut plugin = CiMonitorPlugin::new();
    plugin.init(&ctx).await.unwrap();

    // Run briefly with cancellation
    let cancel = CancellationToken::new();
    let cancel_clone = cancel.clone();
    let cancel_task = tokio::spawn(async move {
        tokio::task::yield_now().await;
        cancel_clone.cancel();
    });
    let run_result = timeout(Duration::from_secs(1), plugin.run(cancel))
        .await
        .expect("plugin run should stop after cancellation");
    assert!(run_result.is_ok());
    cancel_task.await.unwrap();

    // Shutdown
    let shutdown_result = plugin.shutdown().await;
    assert!(shutdown_result.is_ok());

    // Verify member cleanup
    let config_path = ctx.mail.teams_root().join("test-team").join("config.json");
    let config_content = std::fs::read_to_string(&config_path).unwrap();
    let config: serde_json::Value = serde_json::from_str(&config_content).unwrap();

    let members = config["members"].as_array().unwrap();
    let ci_monitor = members
        .iter()
        .find(|m| m["name"].as_str() == Some("ci-monitor"));

    assert!(ci_monitor.is_some());
    assert_eq!(ci_monitor.unwrap()["isActive"].as_bool(), Some(false));
}

#[tokio::test]
async fn test_shutdown_without_init() {
    let mut plugin = CiMonitorPlugin::new();

    // Shutdown without init should handle gracefully
    let result = plugin.shutdown().await;
    assert!(result.is_ok());
}
