//! Integration tests for the CI Monitor plugin

use agent_team_mail_core::config::Config;
use agent_team_mail_core::context::{GitProvider, Platform, RepoContext, SystemContext};
use agent_team_mail_core::schema::InboxMessage;
use agent_team_mail_daemon::plugin::{MailService, Plugin, PluginContext};
use agent_team_mail_daemon::plugins::ci_monitor::{
    create_test_job, create_test_run, CiMonitorPlugin, CiRunConclusion, CiRunStatus, MockCall,
    MockCiProvider,
};
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
    plugin_config.insert(
        "poll_interval_secs".to_string(),
        toml::Value::Integer(10),
    ); // Minimum 10 seconds
    plugin_config.insert(
        "team".to_string(),
        toml::Value::String("test-team".to_string()),
    );
    plugin_config.insert(
        "agent".to_string(),
        toml::Value::String("ci-monitor".to_string()),
    );

    let mut config = (*ctx.config).clone();
    config.plugins.insert("ci_monitor".to_string(), plugin_config);
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
    let cancel_clone = cancel.clone();

    tokio::spawn(async move {
        // Cancel after 1.5 seconds (enough for one poll)
        tokio::time::sleep(Duration::from_millis(1500)).await;
        cancel_clone.cancel();
    });

    let _ = plugin.run(cancel).await;

    // Verify inbox message was delivered
    let messages = read_inbox(ctx.mail.teams_root(), "test-team", "ci-monitor");
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

#[tokio::test]
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
    config.plugins.insert("ci_monitor".to_string(), plugin_config);
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
        provider_config: None,
    };

    let mut plugin = CiMonitorPlugin::new()
        .with_provider(Box::new(mock_provider))
        .with_config(test_config);
    plugin.init(&ctx).await.unwrap();

    // Run for more than two poll cycles (21 seconds = 2 full cycles)
    let cancel = CancellationToken::new();
    let cancel_clone = cancel.clone();

    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(21000)).await;
        cancel_clone.cancel();
    });

    let _ = plugin.run(cancel).await;

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
    let messages = read_inbox(ctx.mail.teams_root(), "test-team", "ci-monitor");
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
    config.plugins.insert("ci_monitor".to_string(), plugin_config);
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
    let cancel1_clone = cancel1.clone();
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(1200)).await;
        cancel1_clone.cancel();
    });
    let _ = plugin1.run(cancel1).await;

    // No messages yet (in-progress doesn't trigger notification)
    let messages = read_inbox(ctx.mail.teams_root(), "test-team", "ci-monitor");
    assert_eq!(
        messages.len(),
        0,
        "In-progress run should not trigger notification"
    );

    // Remove synthetic member before re-init
    ctx.roster
        .cleanup_plugin("test-team", "ci_monitor", agent_team_mail_daemon::roster::CleanupMode::Hard)
        .unwrap();

    // Run plugin again with failed run
    let mut plugin2 = CiMonitorPlugin::new().with_provider(Box::new(mock_provider_v2));
    plugin2.init(&ctx).await.unwrap();

    let cancel2 = CancellationToken::new();
    let cancel2_clone = cancel2.clone();
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(1200)).await;
        cancel2_clone.cancel();
    });
    let _ = plugin2.run(cancel2).await;

    // Now we should have exactly one notification for the failure
    let messages = read_inbox(ctx.mail.teams_root(), "test-team", "ci-monitor");
    assert_eq!(
        messages.len(),
        1,
        "Should deliver notification when run transitions to failure"
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
    config.plugins.insert("ci_monitor".to_string(), plugin_config);
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
    let cancel_clone = cancel.clone();
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(1500)).await;
        cancel_clone.cancel();
    });
    let _ = plugin.run(cancel).await;

    // Should have one message per failure
    let messages = read_inbox(ctx.mail.teams_root(), "test-team", "ci-monitor");
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
    config.plugins.insert("ci_monitor".to_string(), plugin_config);
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
    let cancel_clone = cancel.clone();
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(1500)).await;
        cancel_clone.cancel();
    });
    let _ = plugin.run(cancel).await;

    // Verify list_runs was called with branch filters
    let calls = provider_clone.get_calls();
    let list_calls: Vec<_> = calls
        .iter()
        .filter_map(|c| match c {
            MockCall::ListRuns(filter) => Some(filter.clone()),
            _ => None,
        })
        .collect();

    assert!(!list_calls.is_empty());
    // Should have filtered by main and develop
    assert!(list_calls
        .iter()
        .any(|f| f.branch == Some("main".to_string())));
    assert!(list_calls
        .iter()
        .any(|f| f.branch == Some("develop".to_string())));

    // Only two messages (main and develop, not feature-x)
    let messages = read_inbox(ctx.mail.teams_root(), "test-team", "ci-monitor");
    assert_eq!(
        messages.len(),
        2,
        "Should only notify for watched branches"
    );
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
    config.plugins.insert("ci_monitor".to_string(), plugin_config);
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
    let cancel_clone = cancel.clone();
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(1500)).await;
        cancel_clone.cancel();
    });
    let _ = plugin.run(cancel).await;

    // Only failure should be notified (not success or cancelled)
    let messages = read_inbox(ctx.mail.teams_root(), "test-team", "ci-monitor");
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
    plugin_config.insert("team".to_string(), toml::Value::String("test-team".to_string()));
    plugin_config.insert("agent".to_string(), toml::Value::String("ci-monitor".to_string()));

    let mut config = (*ctx.config).clone();
    config.plugins.insert("ci_monitor".to_string(), plugin_config);
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
    assert_eq!(bot["agentType"].as_str().unwrap(), "plugin:ci_monitor");
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
    plugin_config.insert("team".to_string(), toml::Value::String("test-team".to_string()));
    plugin_config.insert("agent".to_string(), toml::Value::String("ci-monitor".to_string()));

    let mut config = (*ctx.config).clone();
    config.plugins.insert("ci_monitor".to_string(), plugin_config);
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
    plugin_config.insert("team".to_string(), toml::Value::String("test-team".to_string()));
    plugin_config.insert("agent".to_string(), toml::Value::String("ci-monitor".to_string()));

    let mut config = (*ctx.config).clone();
    config.plugins.insert("ci_monitor".to_string(), plugin_config);
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

    let run_task = tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(100)).await;
        cancel_clone.cancel();
    });

    let run_result = plugin.run(cancel).await;
    assert!(run_result.is_ok());

    run_task.await.unwrap();

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
