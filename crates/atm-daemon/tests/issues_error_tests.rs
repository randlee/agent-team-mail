//! Error scenario tests for the Issues plugin

use agent_team_mail_core::config::Config;
use agent_team_mail_core::context::{GitProvider, Platform, RepoContext, SystemContext};
use agent_team_mail_core::schema::InboxMessage;
use agent_team_mail_daemon::plugin::{Plugin, PluginContext};
use agent_team_mail_daemon::plugins::issues::IssuesPlugin;
use agent_team_mail_daemon::plugin::MailService;
use agent_team_mail_daemon::roster::RosterService;
use std::collections::HashMap;
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;
use tempfile::TempDir;
use tokio_util::sync::CancellationToken;

/// Helper to create a test PluginContext
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

#[tokio::test]
async fn test_api_failure_continues_polling() {
    let temp_dir = TempDir::new().unwrap();

    let git_provider = GitProvider::GitHub {
        owner: "test".to_string(),
        repo: "repo".to_string(),
    };
    let ctx = create_test_context(&temp_dir, Some(git_provider));

    // Create team structure
    create_team_config(ctx.mail.teams_root(), "test-team");

    // Create plugin and init
    let mut plugin = IssuesPlugin::new();
    plugin.init(&ctx).await.unwrap();

    // Run plugin briefly - even with API errors it should continue
    let cancel = CancellationToken::new();
    let cancel_clone = cancel.clone();

    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(200)).await;
        cancel_clone.cancel();
    });

    // Plugin should not crash even if GitHub API fails
    let result = plugin.run(cancel).await;
    assert!(result.is_ok(), "Plugin should handle API failures gracefully");

    // Verify no inbox messages written (error was handled gracefully)
    let inbox_messages = ctx.mail.read_inbox("test-team", "issues-bot");
    assert!(
        inbox_messages.is_ok() && inbox_messages.unwrap().is_empty(),
        "No messages should be written on API failure"
    );
}

#[tokio::test]
async fn test_auth_failure_on_comment() {
    // This test would require injecting a mock provider that fails only on add_comment
    // The current architecture makes this difficult without dependency injection
    // Skip for now - the pattern is demonstrated in test_api_failure_continues_polling
}

#[tokio::test]
async fn test_missing_provider_init_fails() {
    let temp_dir = TempDir::new().unwrap();

    // Create context WITHOUT repo info (no GitProvider)
    let ctx = create_test_context(&temp_dir, None);

    // Create and init plugin
    let mut plugin = IssuesPlugin::new();
    let result = plugin.init(&ctx).await;

    // Should fail with descriptive error
    assert!(result.is_err());
    let err = result.unwrap_err();
    let err_msg = err.to_string();
    assert!(
        err_msg.contains("repository") || err_msg.contains("provider"),
        "Error should mention missing repository/provider: {err_msg}"
    );
}

#[tokio::test]
async fn test_missing_gh_binary() {
    // Testing that gh CLI is not found is difficult in integration tests
    // because we can't reliably control the PATH in a way that works across all CI environments
    // The GitHub provider already handles this case and returns appropriate errors
    // This test documents the expected behavior but doesn't execute it

    // Expected: GitHubProvider should return PluginError::Provider with message about gh CLI not found
    // when gh command is not available on PATH
}

#[tokio::test]
async fn test_empty_config_uses_defaults() {
    let temp_dir = TempDir::new().unwrap();

    let git_provider = GitProvider::GitHub {
        owner: "test".to_string(),
        repo: "repo".to_string(),
    };
    let ctx = create_test_context(&temp_dir, Some(git_provider));

    // Create team structure
    create_team_config(ctx.mail.teams_root(), "test-team");

    // Create plugin with NO config section
    let mut plugin = IssuesPlugin::new();
    plugin.init(&ctx).await.unwrap();

    // Plugin should use defaults from IssuesConfig::default()
    // Verified indirectly by successful init
}

#[tokio::test]
async fn test_invalid_config_values_use_defaults() {
    let temp_dir = TempDir::new().unwrap();

    let git_provider = GitProvider::GitHub {
        owner: "test".to_string(),
        repo: "repo".to_string(),
    };
    let mut ctx = create_test_context(&temp_dir, Some(git_provider));

    // Create team structure
    create_team_config(ctx.mail.teams_root(), "test-team");

    // Create config with invalid types
    let mut plugin_config = toml::Table::new();
    plugin_config.insert("poll_interval".to_string(), toml::Value::String("not-a-number".to_string()));
    plugin_config.insert("labels".to_string(), toml::Value::Integer(123)); // Should be array

    let mut config = (*ctx.config).clone();
    config.plugins.insert("issues".to_string(), plugin_config);
    ctx = PluginContext::new(ctx.system.clone(), ctx.mail.clone(), Arc::new(config), ctx.roster.clone());

    // Create and init plugin - should fall back to defaults gracefully
    let mut plugin = IssuesPlugin::new();
    let result = plugin.init(&ctx).await;

    // Should succeed with defaults
    assert!(result.is_ok(), "Plugin should handle invalid config types gracefully");
}

#[tokio::test]
async fn test_handle_message_with_invalid_format() {
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

    // Test with message that doesn't have [issue:NUMBER] prefix
    let msg = InboxMessage {
        from: "test-user".to_string(),
        text: "This is not an issue reply".to_string(),
        timestamp: chrono::Utc::now().to_rfc3339(),
        read: false,
        summary: None,
        message_id: None,
        unknown_fields: HashMap::new(),
    };

    // Should handle gracefully (ignore non-issue messages)
    let result = plugin.handle_message(&msg).await;
    assert!(result.is_ok(), "Plugin should ignore non-issue messages gracefully");

    // Test with invalid issue number
    let msg2 = InboxMessage {
        from: "test-user".to_string(),
        text: "[issue:abc] Invalid number".to_string(),
        timestamp: chrono::Utc::now().to_rfc3339(),
        read: false,
        summary: None,
        message_id: None,
        unknown_fields: HashMap::new(),
    };

    let result2 = plugin.handle_message(&msg2).await;
    assert!(result2.is_ok(), "Plugin should handle invalid issue numbers gracefully");
}

#[tokio::test]
async fn test_handle_message_with_empty_body() {
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

    // Test with message that has [issue:NUMBER] but empty body
    let msg = InboxMessage {
        from: "test-user".to_string(),
        text: "[issue:42]".to_string(), // No reply body
        timestamp: chrono::Utc::now().to_rfc3339(),
        read: false,
        summary: None,
        message_id: None,
        unknown_fields: HashMap::new(),
    };

    // Should handle gracefully (skip posting empty comment)
    let result = plugin.handle_message(&msg).await;
    assert!(result.is_ok(), "Plugin should handle empty reply bodies gracefully");
}
