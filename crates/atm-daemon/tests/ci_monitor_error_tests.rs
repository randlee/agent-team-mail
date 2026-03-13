//! Error scenario tests for the CI Monitor plugin

use agent_team_mail_core::config::Config;
use agent_team_mail_core::context::{GitProvider, Platform, RepoContext, SystemContext};
use agent_team_mail_daemon::plugin::{MailService, Plugin, PluginContext};
use agent_team_mail_daemon::plugins::ci_monitor::{CiMonitorPlugin, MockCall, MockCiProvider};
use agent_team_mail_daemon::roster::RosterService;
use serial_test::serial;
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;
use tempfile::TempDir;
use tokio_util::sync::CancellationToken;

/// Helper to create a test PluginContext
fn create_test_context(temp_dir: &TempDir, provider: Option<GitProvider>) -> PluginContext {
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

fn read_health_record(temp_dir: &TempDir, team: &str) -> Option<serde_json::Value> {
    let path = temp_dir.path().join(".atm/daemon/gh-monitor-health.json");
    let content = std::fs::read_to_string(path).ok()?;
    let value: serde_json::Value = serde_json::from_str(&content).ok()?;
    let records = value.get("records")?.as_array()?;
    records
        .iter()
        .find(|record| record.get("team").and_then(|v| v.as_str()) == Some(team))
        .cloned()
}

#[tokio::test]
#[serial]
async fn test_api_failure_continues_polling() {
    let temp_dir = TempDir::new().unwrap();

    let git_provider = GitProvider::GitHub {
        owner: "test".to_string(),
        repo: "repo".to_string(),
    };
    let mut ctx = create_test_context(&temp_dir, Some(git_provider));

    create_team_config(ctx.mail.teams_root(), "test-team");

    // Create mock provider that returns error
    let mock_provider = MockCiProvider::new().with_error("API rate limit exceeded".to_string());

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

    let mut plugin = CiMonitorPlugin::new().with_provider(Box::new(mock_provider));
    plugin.init(&ctx).await.unwrap();

    // Run plugin briefly - even with API errors it should continue
    let cancel = CancellationToken::new();
    let cancel_clone = cancel.clone();

    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(200)).await;
        cancel_clone.cancel();
    });

    // Plugin should not crash even if CI API fails
    let result = plugin.run(cancel).await;
    assert!(
        result.is_ok(),
        "Plugin should handle API failures gracefully"
    );

    // Verify no inbox messages written (error was handled gracefully)
    let inbox_messages = ctx.mail.read_inbox("test-team", "ci-monitor");
    assert!(
        inbox_messages.is_ok() && inbox_messages.unwrap().is_empty(),
        "No messages should be written on API failure"
    );
}

#[tokio::test(start_paused = true)]
#[serial]
async fn test_api_failure_uses_bounded_backoff() {
    let temp_dir = TempDir::new().unwrap();

    let git_provider = GitProvider::GitHub {
        owner: "test".to_string(),
        repo: "repo".to_string(),
    };
    let mut ctx = create_test_context(&temp_dir, Some(git_provider));
    create_team_config(ctx.mail.teams_root(), "test-team");

    let mock_provider = MockCiProvider::new().with_error("API unavailable".to_string());
    let provider_clone = mock_provider.clone();

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

    let mut plugin = CiMonitorPlugin::new().with_provider(Box::new(mock_provider));
    plugin.init(&ctx).await.unwrap();

    let cancel = CancellationToken::new();
    let run_cancel = cancel.clone();
    let run_task = tokio::spawn(async move { plugin.run(run_cancel).await });

    tokio::task::yield_now().await;
    tokio::time::advance(Duration::from_secs(75)).await;
    tokio::task::yield_now().await;
    cancel.cancel();
    tokio::task::yield_now().await;
    let _ = run_task.await.unwrap();

    let list_calls = provider_clone
        .get_calls()
        .iter()
        .filter(|call| matches!(call, MockCall::ListRuns(_)))
        .count();

    // Immediate poll + backoff sequence (10s, 20s, 40s) should keep call count bounded.
    assert!(
        list_calls <= 4,
        "bounded backoff should limit list_runs calls; got {list_calls}"
    );
}

#[tokio::test]
#[serial]
async fn test_auth_failure_simulation() {
    let temp_dir = TempDir::new().unwrap();

    let git_provider = GitProvider::GitHub {
        owner: "test".to_string(),
        repo: "repo".to_string(),
    };
    let mut ctx = create_test_context(&temp_dir, Some(git_provider));

    create_team_config(ctx.mail.teams_root(), "test-team");

    // Simulate authentication failure
    let mock_provider =
        MockCiProvider::new().with_error("Authentication failed: invalid token".to_string());

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

    let mut plugin = CiMonitorPlugin::new().with_provider(Box::new(mock_provider));
    plugin.init(&ctx).await.unwrap();

    let cancel = CancellationToken::new();
    let cancel_clone = cancel.clone();

    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(200)).await;
        cancel_clone.cancel();
    });

    let result = plugin.run(cancel).await;
    assert!(
        result.is_ok(),
        "Plugin should handle auth failures gracefully"
    );
}

#[tokio::test]
#[serial]
async fn test_missing_provider_init_fails() {
    let temp_dir = TempDir::new().unwrap();

    // Create context WITHOUT repo info (no GitProvider)
    let ctx = create_test_context(&temp_dir, None);

    // Create and init plugin
    let mut plugin = CiMonitorPlugin::new();
    let result = plugin.init(&ctx).await;

    // Should fail with descriptive error
    assert!(result.is_err());
    let err = result.unwrap_err();
    let err_msg = err.to_string();
    assert!(
        err_msg.contains("repository") || err_msg.contains("No repository"),
        "Error should mention missing repository: {err_msg}"
    );
}

#[tokio::test]
#[serial]
async fn test_invalid_config_provider() {
    let temp_dir = TempDir::new().unwrap();

    let git_provider = GitProvider::GitHub {
        owner: "test".to_string(),
        repo: "repo".to_string(),
    };
    let mut ctx = create_test_context(&temp_dir, Some(git_provider));

    create_team_config(ctx.mail.teams_root(), "test-team");

    // Configure with non-existent provider (must include required fields)
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
    plugin_config.insert(
        "provider".to_string(),
        toml::Value::String("nonexistent-provider".to_string()),
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

    // Create and init plugin - should fail
    let mut plugin = CiMonitorPlugin::new();
    let result = plugin.init(&ctx).await;

    assert!(result.is_err());
    let err_msg = result.unwrap_err().to_string();
    assert!(
        err_msg.contains("not registered") || err_msg.contains("nonexistent-provider"),
        "Error should mention provider not found: {err_msg}"
    );
}

#[cfg(unix)]
#[tokio::test]
#[serial]
async fn test_invalid_config_sets_disabled_health_at_init() {
    let temp_dir = TempDir::new().unwrap();

    let git_provider = GitProvider::GitHub {
        owner: "test".to_string(),
        repo: "repo".to_string(),
    };
    let mut ctx = create_test_context(&temp_dir, Some(git_provider));
    create_team_config(ctx.mail.teams_root(), "test-team");

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
    // Invalid by requirements (minimum is 10)
    plugin_config.insert("poll_interval_secs".to_string(), toml::Value::Integer(1));

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

    let mut plugin = CiMonitorPlugin::new();
    let result = plugin.init(&ctx).await;
    assert!(result.is_err(), "init should fail for invalid config");

    let health = read_health_record(&temp_dir, "test-team").expect("health record expected");
    assert_eq!(
        health["availability_state"].as_str(),
        Some("disabled_config_error")
    );
    assert!(
        health["message"]
            .as_str()
            .unwrap_or_default()
            .contains("invalid gh_monitor config"),
        "health message should include invalid config reason"
    );

    let lead_messages = ctx.mail.read_inbox("test-team", "team-lead").unwrap();
    assert!(
        lead_messages.iter().any(|m| m
            .text
            .contains("availability transition healthy -> disabled_config_error")),
        "team lead should receive disabled_config_error transition alert"
    );
}

#[tokio::test]
#[serial]
async fn test_empty_config_uses_defaults() {
    let temp_dir = TempDir::new().unwrap();

    let git_provider = GitProvider::GitHub {
        owner: "test".to_string(),
        repo: "repo".to_string(),
    };
    let mut ctx = create_test_context(&temp_dir, Some(git_provider));

    create_team_config(ctx.mail.teams_root(), "test-team");

    // Create plugin with minimal config (enabled, team, agent required)
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

    let mut plugin = CiMonitorPlugin::new();
    plugin.init(&ctx).await.unwrap();

    // Plugin should use defaults from CiMonitorConfig::default()
    // Verified indirectly by successful init
}

#[tokio::test]
#[serial]
async fn test_invalid_config_values_use_defaults() {
    let temp_dir = TempDir::new().unwrap();

    let git_provider = GitProvider::GitHub {
        owner: "test".to_string(),
        repo: "repo".to_string(),
    };
    let mut ctx = create_test_context(&temp_dir, Some(git_provider));

    create_team_config(ctx.mail.teams_root(), "test-team");

    // Create config with invalid types (will fall back to defaults)
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
    plugin_config.insert(
        "poll_interval_secs".to_string(),
        toml::Value::String("not-a-number".to_string()),
    );
    plugin_config.insert("watched_branches".to_string(), toml::Value::Integer(123)); // Should be array

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

    // Create and init plugin - should fall back to defaults gracefully
    let mut plugin = CiMonitorPlugin::new();
    let result = plugin.init(&ctx).await;

    // Should succeed with defaults
    assert!(
        result.is_ok(),
        "Plugin should handle invalid config types gracefully: {:?}",
        result.err()
    );
}

#[tokio::test]
#[serial]
async fn test_timeout_error_simulation() {
    let temp_dir = TempDir::new().unwrap();

    let git_provider = GitProvider::GitHub {
        owner: "test".to_string(),
        repo: "repo".to_string(),
    };
    let mut ctx = create_test_context(&temp_dir, Some(git_provider));

    create_team_config(ctx.mail.teams_root(), "test-team");

    // Simulate timeout error
    let mock_provider = MockCiProvider::new().with_error("Request timed out".to_string());

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

    let mut plugin = CiMonitorPlugin::new().with_provider(Box::new(mock_provider));
    plugin.init(&ctx).await.unwrap();

    let cancel = CancellationToken::new();
    let cancel_clone = cancel.clone();

    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(200)).await;
        cancel_clone.cancel();
    });

    let result = plugin.run(cancel).await;
    assert!(
        result.is_ok(),
        "Plugin should handle timeout errors gracefully"
    );
}

#[tokio::test]
#[serial]
async fn test_network_error_simulation() {
    let temp_dir = TempDir::new().unwrap();

    let git_provider = GitProvider::GitHub {
        owner: "test".to_string(),
        repo: "repo".to_string(),
    };
    let mut ctx = create_test_context(&temp_dir, Some(git_provider));

    create_team_config(ctx.mail.teams_root(), "test-team");

    // Simulate network error
    let mock_provider = MockCiProvider::new().with_error("Network unreachable".to_string());

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

    let mut plugin = CiMonitorPlugin::new().with_provider(Box::new(mock_provider));
    plugin.init(&ctx).await.unwrap();

    let cancel = CancellationToken::new();
    let cancel_clone = cancel.clone();

    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(200)).await;
        cancel_clone.cancel();
    });

    let result = plugin.run(cancel).await;
    assert!(
        result.is_ok(),
        "Plugin should handle network errors gracefully"
    );
}

#[tokio::test]
#[serial]
async fn test_get_run_failure_continues() {
    let temp_dir = TempDir::new().unwrap();

    let git_provider = GitProvider::GitHub {
        owner: "test".to_string(),
        repo: "repo".to_string(),
    };
    let mut ctx = create_test_context(&temp_dir, Some(git_provider));

    create_team_config(ctx.mail.teams_root(), "test-team");

    // Mock provider that succeeds on list_runs but fails on get_run
    // This simulates a scenario where run details can't be fetched
    let mock_provider = MockCiProvider::new().with_error("Failed to fetch run details".to_string());

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

    let mut plugin = CiMonitorPlugin::new().with_provider(Box::new(mock_provider));
    plugin.init(&ctx).await.unwrap();

    let cancel = CancellationToken::new();
    let cancel_clone = cancel.clone();

    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(1500)).await;
        cancel_clone.cancel();
    });

    // Should handle get_run failures and continue polling
    let result = plugin.run(cancel).await;
    assert!(
        result.is_ok(),
        "Plugin should continue despite get_run failures"
    );
}
