//! Team config synchronization
//!
//! Hub is source of truth for team config. On pull cycle, spokes download
//! config.json from hub and merge with local config, preserving local agent
//! membership.

use anyhow::{Context, Result};
use atm_core::config::HostnameRegistry;
use atm_core::schema::TeamConfig;
use std::path::Path;
use tokio::fs;
use tracing::{info, warn};

use super::transport::Transport;

/// Sync team config from hub to local
///
/// Downloads config.json from hub and merges with local config.
/// Preserves local agent membership (local team-lead owns local agents).
///
/// # Arguments
///
/// * `transport` - Transport implementation
/// * `team_dir` - Local team directory
/// * `hub_hostname` - Hub hostname
/// * `registry` - Hostname registry (for collision warnings)
///
/// # Errors
///
/// Returns error if download or merge fails
pub async fn sync_team_config(
    transport: &dyn Transport,
    team_dir: &Path,
    hub_hostname: &str,
    registry: &HostnameRegistry,
) -> Result<bool> {
    // Remote path: <team>/config.json
    let remote_config_path = Path::new(team_dir.file_name().unwrap()).join("config.json");

    // Download to temp file
    let temp_path = team_dir.join(".bridge-config-tmp");

    // Download config from hub
    match transport.download(&remote_config_path, &temp_path).await {
        Ok(()) => {
            info!("Downloaded team config from hub: {}", hub_hostname);
        }
        Err(e) => {
            warn!("Failed to download team config from hub: {}", e);
            return Ok(false);
        }
    }

    // Parse hub config
    let hub_config_content = fs::read_to_string(&temp_path).await?;
    let hub_config: TeamConfig = serde_json::from_str(&hub_config_content)
        .context("Failed to parse hub team config")?;

    // Read local config
    let local_config_path = team_dir.join("config.json");
    let local_config: TeamConfig = if local_config_path.exists() {
        let content = fs::read_to_string(&local_config_path).await?;
        serde_json::from_str(&content).context("Failed to parse local team config")?
    } else {
        // No local config yet - use hub config as-is
        fs::write(&local_config_path, &hub_config_content).await?;
        fs::remove_file(&temp_path).await?;
        info!("Initialized local team config from hub");
        return Ok(true);
    };

    // Merge configs: preserve local agent membership
    let merged = merge_team_config(&local_config, &hub_config, registry);

    // Write merged config atomically
    let merged_json = serde_json::to_string_pretty(&merged)?;
    let tmp_write_path = team_dir.join(".bridge-config-write-tmp");
    fs::write(&tmp_write_path, &merged_json).await?;
    fs::rename(&tmp_write_path, &local_config_path).await?;

    // Cleanup download temp file
    fs::remove_file(&temp_path).await?;

    info!("Team config synced from hub");
    Ok(true)
}

/// Merge hub config with local config
///
/// Rules:
/// - Hub config is source of truth for: name, description, created_at
/// - Local agent membership is preserved (local team-lead owns local agents)
/// - Warn if hub config introduces new hostnames that collide with registry
fn merge_team_config(
    local: &TeamConfig,
    hub: &TeamConfig,
    registry: &HostnameRegistry,
) -> TeamConfig {
    // Start with hub config (source of truth)
    let mut merged = hub.clone();

    // Preserve local agent membership
    // Keep local members that are not in hub config
    let hub_member_names: std::collections::HashSet<_> = hub.members.iter()
        .map(|m| m.name.as_str())
        .collect();

    for local_member in &local.members {
        if !hub_member_names.contains(local_member.name.as_str()) {
            merged.members.push(local_member.clone());
        }
    }

    // Warn about hostname collisions in hub config
    // Extract any hostnames mentioned in member names (e.g., "agent@hostname")
    for member in &hub.members {
        if let Some(hostname) = extract_hostname_from_agent_name(&member.name) {
            if !registry.is_known_hostname(&hostname) {
                warn!(
                    "Hub config introduces unknown hostname '{}' in agent '{}'",
                    hostname, member.name
                );
            }
        }
    }

    merged
}

/// Extract hostname from agent name if present (e.g., "agent@hostname" -> "hostname")
fn extract_hostname_from_agent_name(name: &str) -> Option<String> {
    name.split('@').nth(1).map(|s| s.to_string())
}

/// Cleanup stale .bridge-tmp files
///
/// On plugin startup, scan team directories for stale temp files
/// (leftovers from interrupted atomic writes) and delete them.
pub async fn cleanup_stale_tmp_files(team_dir: &Path) -> Result<usize> {
    let mut cleaned = 0;

    // Check team directory
    if let Ok(mut entries) = fs::read_dir(team_dir).await {
        while let Ok(Some(entry)) = entries.next_entry().await {
            let path = entry.path();
            if let Some(name) = path.file_name().and_then(|n| n.to_str()) {
                if name.contains(".bridge-tmp") || name.ends_with("-tmp") {
                    match fs::remove_file(&path).await {
                        Ok(()) => {
                            warn!("Cleaned up stale temp file: {}", path.display());
                            cleaned += 1;
                        }
                        Err(e) => {
                            warn!("Failed to clean up stale temp file {}: {}", path.display(), e);
                        }
                    }
                }
            }
        }
    }

    // Check inboxes directory
    let inboxes_dir = team_dir.join("inboxes");
    if inboxes_dir.exists() {
        if let Ok(mut entries) = fs::read_dir(&inboxes_dir).await {
            while let Ok(Some(entry)) = entries.next_entry().await {
                let path = entry.path();
                if let Some(name) = path.file_name().and_then(|n| n.to_str()) {
                    if name.contains(".bridge-tmp") || name.ends_with("-tmp") {
                        match fs::remove_file(&path).await {
                            Ok(()) => {
                                warn!("Cleaned up stale temp file: {}", path.display());
                                cleaned += 1;
                            }
                            Err(e) => {
                                warn!("Failed to clean up stale temp file {}: {}", path.display(), e);
                            }
                        }
                    }
                }
            }
        }
    }

    if cleaned > 0 {
        info!("Cleaned up {} stale temp file(s)", cleaned);
    }

    Ok(cleaned)
}

#[cfg(test)]
mod tests {
    use super::*;
    use atm_core::config::HostnameRegistry;
    use atm_core::schema::AgentMember;
    use std::collections::HashMap;

    fn create_test_member(name: &str, agent_type: &str) -> AgentMember {
        AgentMember {
            agent_id: format!("{name}@test-team"),
            name: name.to_string(),
            agent_type: agent_type.to_string(),
            model: "claude-sonnet-4-5".to_string(),
            prompt: None,
            color: None,
            plan_mode_required: None,
            joined_at: 1000000,
            tmux_pane_id: None,
            cwd: "/tmp".to_string(),
            subscriptions: Vec::new(),
            backend_type: None,
            is_active: Some(false),
            last_active: None,
            unknown_fields: HashMap::new(),
        }
    }

    fn create_test_team_config(name: &str, members: Vec<AgentMember>) -> TeamConfig {
        TeamConfig {
            name: name.to_string(),
            description: Some(format!("Test team {name}")),
            created_at: 1000000,
            lead_agent_id: format!("team-lead@{name}"),
            lead_session_id: "test-session-id".to_string(),
            members,
            unknown_fields: Default::default(),
        }
    }

    #[test]
    fn test_merge_team_config_preserves_local_agents() {
        let local = create_test_team_config(
            "test-team",
            vec![
                create_test_member("agent-1", "general-purpose"),
                create_test_member("agent-2", "general-purpose"),
            ],
        );

        let hub = create_test_team_config(
            "test-team-hub",
            vec![
                create_test_member("agent-1", "general-purpose"), // Shared
                create_test_member("agent-3", "general-purpose"), // Hub-only
            ],
        );

        let registry = HostnameRegistry::new();
        let merged = merge_team_config(&local, &hub, &registry);

        // Should have: agent-1 (from hub), agent-3 (from hub), agent-2 (local-only preserved)
        assert_eq!(merged.members.len(), 3);

        let member_names: Vec<_> = merged.members.iter().map(|m| m.name.as_str()).collect();
        assert!(member_names.contains(&"agent-1"));
        assert!(member_names.contains(&"agent-2")); // Local preserved
        assert!(member_names.contains(&"agent-3"));

        // Hub metadata should win
        assert_eq!(merged.name, "test-team-hub");
        assert_eq!(merged.description, Some("Test team test-team-hub".to_string()));
    }

    #[test]
    fn test_merge_team_config_hub_is_source_of_truth() {
        let local = create_test_team_config(
            "old-name",
            vec![create_test_member("local-agent", "general-purpose")],
        );

        let mut hub = create_test_team_config(
            "new-name",
            vec![create_test_member("hub-agent", "general-purpose")],
        );
        hub.created_at = 2000000;

        let registry = HostnameRegistry::new();
        let merged = merge_team_config(&local, &hub, &registry);

        // Hub metadata wins
        assert_eq!(merged.name, "new-name");
        assert_eq!(merged.created_at, 2000000);

        // Both agents preserved
        assert_eq!(merged.members.len(), 2);
    }

    #[test]
    fn test_extract_hostname_from_agent_name() {
        assert_eq!(
            extract_hostname_from_agent_name("agent@laptop"),
            Some("laptop".to_string())
        );
        assert_eq!(
            extract_hostname_from_agent_name("dev-agent@desktop.local"),
            Some("desktop.local".to_string())
        );
        assert_eq!(extract_hostname_from_agent_name("agent"), None);
        assert_eq!(extract_hostname_from_agent_name(""), None);
    }

    #[tokio::test]
    async fn test_cleanup_stale_tmp_files() {
        use tempfile::TempDir;

        let temp_dir = TempDir::new().unwrap();
        let team_dir = temp_dir.path();

        // Create some stale temp files
        let inboxes_dir = team_dir.join("inboxes");
        fs::create_dir_all(&inboxes_dir).await.unwrap();

        fs::write(team_dir.join(".bridge-config-tmp"), b"stale").await.unwrap();
        fs::write(team_dir.join(".bridge-state-tmp"), b"stale").await.unwrap();
        fs::write(inboxes_dir.join(".bridge-tmp-agent.json"), b"stale").await.unwrap();
        fs::write(team_dir.join("config.json"), b"valid").await.unwrap();

        let cleaned = cleanup_stale_tmp_files(team_dir).await.unwrap();
        assert_eq!(cleaned, 3);

        // Verify cleanup
        assert!(!team_dir.join(".bridge-config-tmp").exists());
        assert!(!team_dir.join(".bridge-state-tmp").exists());
        assert!(!inboxes_dir.join(".bridge-tmp-agent.json").exists());
        assert!(team_dir.join("config.json").exists()); // Valid file preserved
    }
}
