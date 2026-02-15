//! Integration tests for the Issues plugin

use atm_core::config::Config;
use atm_core::context::{GitProvider, Platform, RepoContext, SystemContext};
use atm_core::schema::InboxMessage;
use atm_daemon::plugin::{Plugin, PluginContext};
use atm_daemon::plugins::issues::{
    Issue, IssueLabel, IssueState, IssuesPlugin, MockCall, MockProvider,
};
use atm_daemon::plugin::MailService;
use atm_daemon::roster::RosterService;
use std::collections::HashMap;
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;
use tempfile::TempDir;
use tokio::time::timeout;
use tokio_util::sync::CancellationToken;

/// Helper to create a test PluginContext with mock provider injection
fn create_test_context(temp_dir: &TempDir, provider: Option<GitProvider>) -> PluginContext {
    // Set ATM_HOME for cross-platform compliance
    unsafe {
        std::env::set_var("ATM_HOME", temp_dir.path());
    }

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

/// Helper to create a test issue
fn create_test_issue(number: u64, title: &str, labels: Vec<&str>) -> Issue {
    Issue {
        id: number.to_string(),
        number,
        title: title.to_string(),
        body: Some(format!("Body for issue {number}")),
        state: IssueState::Open,
        labels: labels
            .into_iter()
            .map(|name| IssueLabel {
                name: name.to_string(),
                color: Some("ff0000".to_string()),
            })
            .collect(),
        assignees: Vec::new(),
        author: "test-author".to_string(),
        created_at: "2026-02-11T10:00:00Z".to_string(),
        updated_at: "2026-02-11T12:00:00Z".to_string(),
        url: format!("https://github.com/test/repo/issues/{number}"),
    }
}

#[tokio::test]
async fn test_issue_created_delivers_inbox_message() {
    let temp_dir = TempDir::new().unwrap();

    // Create provider with one issue
    let issues = vec![create_test_issue(42, "Test issue", vec!["bug"])];
    let mock_provider = MockProvider::with_issues(issues);

    // Create context with GitHub provider (needed for repo path in synthetic member)
    let git_provider = GitProvider::GitHub {
        owner: "test".to_string(),
        repo: "repo".to_string(),
    };
    let mut ctx = create_test_context(&temp_dir, Some(git_provider));

    // Add plugin config to context
    let mut plugin_config = toml::Table::new();
    plugin_config.insert("enabled".to_string(), toml::Value::Boolean(true));
    plugin_config.insert("poll_interval".to_string(), toml::Value::Integer(1)); // 1 second for fast test
    plugin_config.insert("team".to_string(), toml::Value::String("test-team".to_string()));
    plugin_config.insert("agent".to_string(), toml::Value::String("issues-bot".to_string()));

    let mut config = (*ctx.config).clone();
    config.plugins.insert("issues".to_string(), plugin_config);
    ctx = PluginContext::new(ctx.system.clone(), ctx.mail.clone(), Arc::new(config), ctx.roster.clone());

    // Create team structure
    create_team_config(ctx.mail.teams_root(), "test-team");

    // Create plugin with injected provider
    let mut plugin = IssuesPlugin::new()
        .with_provider(Box::new(mock_provider));

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
    let messages = read_inbox(ctx.mail.teams_root(), "test-team", "issues-bot");
    assert!(!messages.is_empty(), "Should have at least one message");

    let msg = &messages[0];
    assert!(msg.text.contains("[issue:42]"), "Message should have issue reference");
    assert!(msg.text.contains("Test issue"), "Message should have issue title");
    assert!(msg.text.contains("https://github.com/test/repo/issues/42"), "Message should have URL");
    assert_eq!(msg.from, "issues-bot");
}

#[tokio::test]
async fn test_inbox_reply_posts_comment() {
    let temp_dir = TempDir::new().unwrap();

    // Create mock provider that tracks calls
    let mock_provider = MockProvider::new();
    let provider_clone = mock_provider.clone();

    // Create context
    let git_provider = GitProvider::GitHub {
        owner: "test".to_string(),
        repo: "repo".to_string(),
    };
    let mut ctx = create_test_context(&temp_dir, Some(git_provider));

    // Add plugin config to context
    let mut plugin_config = toml::Table::new();
    plugin_config.insert("enabled".to_string(), toml::Value::Boolean(true));
    plugin_config.insert("poll_interval".to_string(), toml::Value::Integer(300));
    plugin_config.insert("team".to_string(), toml::Value::String("test-team".to_string()));
    plugin_config.insert("agent".to_string(), toml::Value::String("issues-bot".to_string()));

    let mut config = (*ctx.config).clone();
    config.plugins.insert("issues".to_string(), plugin_config);
    ctx = PluginContext::new(ctx.system.clone(), ctx.mail.clone(), Arc::new(config), ctx.roster.clone());

    // Create team structure
    create_team_config(ctx.mail.teams_root(), "test-team");

    // Create plugin with injected provider
    let mut plugin = IssuesPlugin::new()
        .with_provider(Box::new(mock_provider));

    plugin.init(&ctx).await.unwrap();

    // Send a message with issue reference (on its own line)
    let msg = InboxMessage {
        from: "test-user".to_string(),
        text: "[issue:42]\nThis is my reply\nWith multiple lines".to_string(),
        timestamp: chrono::Utc::now().to_rfc3339(),
        read: false,
        summary: None,
        message_id: None,
        unknown_fields: HashMap::new(),
    };

    // Handle the message
    plugin.handle_message(&msg).await.unwrap();

    // Verify add_comment was called
    let calls = provider_clone.get_calls();
    assert!(!calls.is_empty(), "Should have at least one call");

    let add_comment_call = calls
        .iter()
        .find(|c| matches!(c, MockCall::AddComment { .. }));

    assert!(add_comment_call.is_some(), "Should have AddComment call");

    if let MockCall::AddComment { issue_number, body } = add_comment_call.unwrap() {
        assert_eq!(*issue_number, 42);
        assert!(body.contains("This is my reply"), "Body should contain 'This is my reply', got: {body}");
        assert!(body.contains("With multiple lines"), "Body should contain 'With multiple lines', got: {body}");
    }
}

#[tokio::test]
async fn test_issue_filter_applies_labels() {
    let temp_dir = TempDir::new().unwrap();

    // Create issues with different labels
    let issues = vec![
        create_test_issue(1, "Bug issue", vec!["bug"]),
        create_test_issue(2, "Feature issue", vec!["feature"]),
        create_test_issue(3, "Bug and urgent", vec!["bug", "urgent"]),
    ];

    let mock_provider = MockProvider::with_issues(issues);
    let provider_clone = mock_provider.clone();

    // Create context with plugin config
    let git_provider = GitProvider::GitHub {
        owner: "test".to_string(),
        repo: "repo".to_string(),
    };
    let mut ctx = create_test_context(&temp_dir, Some(git_provider));

    // Add plugin config to context
    let mut plugin_config = toml::Table::new();
    plugin_config.insert("enabled".to_string(), toml::Value::Boolean(true));
    plugin_config.insert("poll_interval".to_string(), toml::Value::Integer(1));
    plugin_config.insert("labels".to_string(), toml::Value::Array(vec![toml::Value::String("bug".to_string())]));
    plugin_config.insert("team".to_string(), toml::Value::String("test-team".to_string()));
    plugin_config.insert("agent".to_string(), toml::Value::String("issues-bot".to_string()));

    let mut config = (*ctx.config).clone();
    config.plugins.insert("issues".to_string(), plugin_config);
    ctx = PluginContext::new(ctx.system.clone(), ctx.mail.clone(), Arc::new(config), ctx.roster.clone());

    // Create team structure
    create_team_config(ctx.mail.teams_root(), "test-team");

    // Create plugin with injected provider
    let mut plugin = IssuesPlugin::new()
        .with_provider(Box::new(mock_provider));

    plugin.init(&ctx).await.unwrap();

    // Run briefly
    let cancel = CancellationToken::new();
    let cancel_clone = cancel.clone();

    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(1500)).await;
        cancel_clone.cancel();
    });

    let _ = plugin.run(cancel).await;

    // Verify list_issues was called with bug label filter
    let calls = provider_clone.get_calls();
    let list_call = calls
        .iter()
        .find(|c| matches!(c, MockCall::ListIssues(_)));

    assert!(list_call.is_some(), "Should have ListIssues call");

    if let MockCall::ListIssues(filter) = list_call.unwrap() {
        assert!(filter.labels.contains(&"bug".to_string()), "Filter should include 'bug' label");
    }

    // Verify only bug issues were delivered
    let messages = read_inbox(ctx.mail.teams_root(), "test-team", "issues-bot");
    // Should have 2 messages (issues #1 and #3 have bug label)
    assert_eq!(messages.len(), 2, "Should deliver only issues matching filter");
}

#[tokio::test]
async fn test_issue_updates_deliver_multiple_messages() {
    let temp_dir = TempDir::new().unwrap();

    // First poll: initial issue
    let issue_v1 = create_test_issue(101, "Update issue", vec!["bug"]);
    let mock_provider_v1 = MockProvider::with_issues(vec![issue_v1.clone()]);

    // Second poll: same issue, updated timestamp
    let mut issue_v2 = issue_v1.clone();
    issue_v2.updated_at = "2026-02-11T12:30:00Z".to_string();
    let mock_provider_v2 = MockProvider::with_issues(vec![issue_v2.clone()]);

    let git_provider = GitProvider::GitHub {
        owner: "test".to_string(),
        repo: "repo".to_string(),
    };
    let mut ctx = create_test_context(&temp_dir, Some(git_provider));

    // Add plugin config to context
    let mut plugin_config = toml::Table::new();
    plugin_config.insert("enabled".to_string(), toml::Value::Boolean(true));
    plugin_config.insert("poll_interval".to_string(), toml::Value::Integer(1));
    plugin_config.insert("team".to_string(), toml::Value::String("test-team".to_string()));
    plugin_config.insert("agent".to_string(), toml::Value::String("issues-bot".to_string()));

    let mut config = (*ctx.config).clone();
    config.plugins.insert("issues".to_string(), plugin_config);
    ctx = PluginContext::new(ctx.system.clone(), ctx.mail.clone(), Arc::new(config), ctx.roster.clone());

    // Create team structure
    create_team_config(ctx.mail.teams_root(), "test-team");

    // Run plugin with initial issue
    let mut plugin1 = IssuesPlugin::new().with_provider(Box::new(mock_provider_v1));
    plugin1.init(&ctx).await.unwrap();

    let cancel1 = CancellationToken::new();
    let cancel1_clone = cancel1.clone();
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(1200)).await;
        cancel1_clone.cancel();
    });
    let _ = plugin1.run(cancel1).await;

    // Remove synthetic member before re-init to avoid duplicate member error
    ctx.roster
        .cleanup_plugin("test-team", "issues", atm_daemon::roster::CleanupMode::Hard)
        .unwrap();

    // Run plugin again with updated issue
    let mut plugin2 = IssuesPlugin::new().with_provider(Box::new(mock_provider_v2));
    plugin2.init(&ctx).await.unwrap();

    let cancel2 = CancellationToken::new();
    let cancel2_clone = cancel2.clone();
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(1200)).await;
        cancel2_clone.cancel();
    });
    let _ = plugin2.run(cancel2).await;

    // Verify two messages for the same issue were delivered
    let messages = read_inbox(ctx.mail.teams_root(), "test-team", "issues-bot");
    assert_eq!(messages.len(), 2, "Should deliver updated issue twice");
}

#[tokio::test]
async fn test_synthetic_member_lifecycle() {
    let temp_dir = TempDir::new().unwrap();

    let git_provider = GitProvider::GitHub {
        owner: "test".to_string(),
        repo: "repo".to_string(),
    };
    let ctx = create_test_context(&temp_dir, Some(git_provider));

    // Create team structure
    let team_name = "test-team";
    create_team_config(ctx.mail.teams_root(), team_name);

    // Create and init plugin
    let mut plugin = IssuesPlugin::new();
    plugin.init(&ctx).await.unwrap();

    // Verify synthetic member was registered
    let config_path = ctx.mail.teams_root().join(team_name).join("config.json");
    let config_content = std::fs::read_to_string(&config_path).unwrap();
    let config: serde_json::Value = serde_json::from_str(&config_content).unwrap();

    let members = config["members"].as_array().unwrap();
    let issues_bot = members
        .iter()
        .find(|m| m["name"].as_str() == Some("issues-bot"));

    assert!(issues_bot.is_some(), "issues-bot should be registered");
    let bot = issues_bot.unwrap();
    assert_eq!(bot["agentType"].as_str().unwrap(), "plugin:issues");
    assert_eq!(bot["isActive"].as_bool(), Some(true));

    // Shutdown plugin
    plugin.shutdown().await.unwrap();

    // Verify member is marked inactive (soft cleanup)
    let config_content = std::fs::read_to_string(&config_path).unwrap();
    let config: serde_json::Value = serde_json::from_str(&config_content).unwrap();

    let members = config["members"].as_array().unwrap();
    let issues_bot = members
        .iter()
        .find(|m| m["name"].as_str() == Some("issues-bot"));

    assert!(issues_bot.is_some(), "issues-bot should still exist");
    assert_eq!(issues_bot.unwrap()["isActive"].as_bool(), Some(false));
}

#[tokio::test]
async fn test_disabled_plugin_skips_init() {
    let temp_dir = TempDir::new().unwrap();

    let git_provider = GitProvider::GitHub {
        owner: "test".to_string(),
        repo: "repo".to_string(),
    };
    let mut ctx = create_test_context(&temp_dir, Some(git_provider));

    // Create team structure
    create_team_config(ctx.mail.teams_root(), "test-team");

    // Configure plugin as disabled
    let mut plugin_config = toml::Table::new();
    plugin_config.insert("enabled".to_string(), toml::Value::Boolean(false));

    // Update context config
    let mut config = (*ctx.config).clone();
    config.plugins.insert("issues".to_string(), plugin_config);
    ctx = PluginContext::new(ctx.system.clone(), ctx.mail.clone(), Arc::new(config), ctx.roster.clone());

    // Create and init plugin
    let mut plugin = IssuesPlugin::new();
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
    assert!(members.is_empty(), "No members should be registered when disabled");
}

#[tokio::test]
async fn test_full_lifecycle_init_run_shutdown() {
    let temp_dir = TempDir::new().unwrap();

    let git_provider = GitProvider::GitHub {
        owner: "test".to_string(),
        repo: "repo".to_string(),
    };
    let ctx = create_test_context(&temp_dir, Some(git_provider));

    // Create team structure
    create_team_config(ctx.mail.teams_root(), "test-team");

    // Create and init plugin
    let mut plugin = IssuesPlugin::new();
    plugin.init(&ctx).await.unwrap();

    // Run briefly with cancellation
    let cancel = CancellationToken::new();
    let cancel_clone = cancel.clone();

    let run_task = tokio::spawn(async move {
        // Cancel after 100ms
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
    let issues_bot = members
        .iter()
        .find(|m| m["name"].as_str() == Some("issues-bot"));

    assert!(issues_bot.is_some());
    assert_eq!(issues_bot.unwrap()["isActive"].as_bool(), Some(false));
}

#[tokio::test]
async fn test_shutdown_without_init() {
    let mut plugin = IssuesPlugin::new();

    // Shutdown without init should handle gracefully
    let result = plugin.shutdown().await;
    assert!(result.is_ok());
}

#[tokio::test]
async fn test_missing_provider_init_fails() {
    let temp_dir = TempDir::new().unwrap();

    // Create context WITHOUT git provider
    let ctx = create_test_context(&temp_dir, None);

    // Create and init plugin
    let mut plugin = IssuesPlugin::new();
    let result = plugin.init(&ctx).await;

    // Should fail with descriptive error
    assert!(result.is_err());
    let err = result.unwrap_err();
    assert!(err.to_string().contains("repository") || err.to_string().contains("provider"));
}
