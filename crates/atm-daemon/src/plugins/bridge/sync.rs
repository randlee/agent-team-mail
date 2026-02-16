//! Sync engine for bridge plugin
//!
//! Coordinates push/pull cycles to synchronize inbox files across machines.

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use tokio::fs;
use tracing::{debug, info, warn};

use super::config::BridgePluginConfig;
use super::dedup::{assign_message_ids, SyncState};
use super::metrics::BridgeMetrics;
use super::self_write_filter::SelfWriteFilter;
use super::team_config_sync::sync_team_config;
use super::transport::Transport;
use atm_core::schema::{InboxMessage, TeamConfig};
use std::collections::HashSet;

/// Statistics from a sync operation
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct SyncStats {
    /// Number of messages pushed to remote(s)
    pub messages_pushed: usize,

    /// Number of messages pulled from remote(s)
    pub messages_pulled: usize,

    /// Number of errors encountered
    pub errors: usize,
}

impl SyncStats {
    /// Add stats from another sync operation
    pub fn add(&mut self, other: &SyncStats) {
        self.messages_pushed += other.messages_pushed;
        self.messages_pulled += other.messages_pulled;
        self.errors += other.errors;
    }
}

/// Circuit breaker threshold - disable remote after this many consecutive failures
const CIRCUIT_BREAKER_THRESHOLD: u64 = 5;

/// Sync engine for bridge plugin
///
/// Manages push/pull cycles, deduplication, and state persistence.
pub struct SyncEngine {
    /// Bridge configuration
    config: Arc<BridgePluginConfig>,

    /// Transport implementation
    transport: Arc<dyn Transport>,

    /// Team directory (e.g., ~/.claude/teams/my-team)
    team_dir: PathBuf,

    /// Sync state (cursors and dedup tracking)
    state: SyncState,

    /// Path to sync state file
    state_path: PathBuf,

    /// Metrics tracking
    metrics: BridgeMetrics,

    /// Path to metrics file
    metrics_path: PathBuf,

    /// Self-write filter to avoid watcher feedback loops
    self_write_filter: Arc<tokio::sync::Mutex<SelfWriteFilter>>,

    /// Known agent names from team config (if available)
    agent_names: Option<HashSet<String>>,
}

impl SyncEngine {
    /// Create a new sync engine
    ///
    /// # Arguments
    ///
    /// * `config` - Bridge plugin configuration
    /// * `transport` - Transport implementation for file transfers
    /// * `team_dir` - Path to team directory
    ///
    /// # Errors
    ///
    /// Returns error if sync state cannot be loaded
    pub async fn new(
        config: Arc<BridgePluginConfig>,
        transport: Arc<dyn Transport>,
        team_dir: PathBuf,
        self_write_filter: Arc<tokio::sync::Mutex<SelfWriteFilter>>,
    ) -> Result<Self> {
        let state_path = team_dir.join(".bridge-state.json");
        let state = SyncState::load(&state_path).await?;

        let metrics_path = team_dir.join(".bridge-metrics.json");
        let metrics = BridgeMetrics::load(&metrics_path).await.unwrap_or_default();

        let agent_names = load_agent_names(&team_dir);

        Ok(Self {
            config,
            transport,
            team_dir,
            state,
            state_path,
            metrics,
            metrics_path,
            self_write_filter,
            agent_names,
        })
    }

    /// Get a reference to the sync state
    ///
    /// Exposed for testing and monitoring purposes
    pub fn state(&self) -> &SyncState {
        &self.state
    }

    /// Get a reference to the metrics
    ///
    /// Exposed for monitoring and CLI status commands
    pub fn metrics(&self) -> &BridgeMetrics {
        &self.metrics
    }

    /// Push local messages to remote(s)
    ///
    /// Reads local inbox files, identifies new messages (based on cursors),
    /// and uploads them to remote hosts.
    ///
    /// # Errors
    ///
    /// Returns error if critical operations fail (state save, etc.).
    /// Individual push failures are logged but don't fail the entire operation.
    pub async fn sync_push(&mut self) -> Result<SyncStats> {
        let mut stats = SyncStats::default();

        // Get list of local inbox files
        let inboxes_dir = self.team_dir.join("inboxes");
        if !inboxes_dir.exists() {
            debug!("No inboxes directory, skipping push");
            return Ok(stats);
        }

        let mut entries = match fs::read_dir(&inboxes_dir).await {
            Ok(entries) => entries,
            Err(e) => {
                warn!("Failed to read inboxes directory: {}", e);
                stats.errors += 1;
                return Ok(stats);
            }
        };

        // Process each inbox file
        while let Ok(Some(entry)) = entries.next_entry().await {
            let path = entry.path();

            // Only process local inbox files (not per-origin files)
            if !self.is_local_inbox_file(&path) {
                continue;
            }

            // Push this inbox file to all remotes
            match self.push_inbox_file(&path).await {
                Ok(pushed) => stats.messages_pushed += pushed,
                Err(e) => {
                    warn!("Failed to push {}: {}", path.display(), e);
                    stats.errors += 1;
                }
            }
        }

        // Save updated state
        self.state.save(&self.state_path).await?;

        Ok(stats)
    }

    /// Pull messages from remote(s) to local
    ///
    /// Downloads per-origin inbox files from remote hosts and writes them locally.
    ///
    /// # Errors
    ///
    /// Returns error if critical operations fail (state save, etc.).
    /// Individual pull failures are logged but don't fail the entire operation.
    pub async fn sync_pull(&mut self) -> Result<SyncStats> {
        let mut stats = SyncStats::default();

        // Iterate over all configured remotes (skip disabled ones)
        let remotes: Vec<String> = self.config.registry.remotes()
            .map(|r| r.hostname.clone())
            .collect();

        for remote_hostname in remotes {
            // Check circuit breaker
            if self.metrics.is_remote_disabled(&remote_hostname) {
                debug!("Skipping disabled remote: {}", remote_hostname);
                continue;
            }

            match self.pull_from_remote(&remote_hostname).await {
                Ok(pulled) => {
                    stats.messages_pulled += pulled;
                    // Reset failure count on success
                    self.metrics.reset_remote_failures(&remote_hostname);
                }
                Err(e) => {
                    warn!("Failed to pull from {}: {}", remote_hostname, e);
                    stats.errors += 1;
                    self.metrics.record_remote_failure(&remote_hostname);

                    // Check if we should disable this remote
                    if self.metrics.get_remote_failures(&remote_hostname) >= CIRCUIT_BREAKER_THRESHOLD {
                        warn!(
                            "Remote {} disabled after {} consecutive failures",
                            remote_hostname, CIRCUIT_BREAKER_THRESHOLD
                        );
                        self.metrics.disable_remote(&remote_hostname);
                    }
                }
            }
        }

        // Save updated state
        self.state.save(&self.state_path).await?;

        Ok(stats)
    }

    /// Run a full sync cycle (push then pull)
    ///
    /// # Errors
    ///
    /// Returns error if critical operations fail
    pub async fn sync_cycle(&mut self) -> Result<SyncStats> {
        info!("Starting sync cycle");
        let mut stats = SyncStats::default();

        // Sync team config from hub (if we're a spoke)
        if let Some(hub_hostname) = self.get_hub_hostname() {
            match sync_team_config(
                self.transport.as_ref(),
                &self.team_dir,
                &hub_hostname,
                &self.config.registry,
            )
            .await
            {
                Ok(true) => {
                    debug!("Team config synced from hub");
                }
                Ok(false) => {
                    debug!("Team config sync skipped (no changes or hub unreachable)");
                }
                Err(e) => {
                    warn!("Failed to sync team config from hub: {}", e);
                    stats.errors += 1;
                }
            }
        }

        // Push local changes
        let push_stats = self.sync_push().await?;
        stats.add(&push_stats);

        // Pull remote changes
        let pull_stats = self.sync_pull().await?;
        stats.add(&pull_stats);

        // Update metrics
        self.metrics.record_sync(
            stats.messages_pushed,
            stats.messages_pulled,
            stats.errors,
        );

        // Save metrics
        if let Err(e) = self.metrics.save(&self.metrics_path).await {
            warn!("Failed to save metrics: {}", e);
        }

        info!(
            "Sync cycle complete: pushed={}, pulled={}, errors={}",
            stats.messages_pushed, stats.messages_pulled, stats.errors
        );

        Ok(stats)
    }

    /// Get hub hostname if this is a spoke node
    fn get_hub_hostname(&self) -> Option<String> {
        use atm_core::config::BridgeRole;

        if self.config.core.role == BridgeRole::Spoke {
            // For spoke, first remote is the hub
            self.config.registry.remotes().next().map(|r| r.hostname.clone())
        } else {
            None
        }
    }

    /// Push a single inbox file to all remotes
    async fn push_inbox_file(&mut self, local_path: &Path) -> Result<usize> {
        // Read inbox file
        let content = fs::read(local_path).await?;
        let mut messages: Vec<InboxMessage> = serde_json::from_slice(&content)
            .context("Failed to parse inbox file")?;

        // Assign message_ids to messages that don't have one
        assign_message_ids(&mut messages);

        // Get cursor for this file
        let rel_path = local_path.strip_prefix(&self.team_dir)?;
        let cursor = self.state.get_cursor(rel_path);

        // Identify new messages to sync
        let new_messages: Vec<InboxMessage> = messages
            .iter()
            .skip(cursor)
            .filter(|msg| {
                if let Some(ref msg_id) = msg.message_id {
                    !self.state.is_synced(msg_id)
                } else {
                    true // Should not happen after assign_message_ids
                }
            })
            .cloned()
            .collect();

        if new_messages.is_empty() {
            debug!("No new messages to push from {}", local_path.display());
            return Ok(0);
        }

        // Extract agent name from filename
        let agent_name = self.extract_agent_name(local_path)?;

        // Push to all remotes (skip disabled ones)
        let mut pushed_count = 0;
        for remote in self.config.registry.remotes() {
            // Check circuit breaker
            if self.metrics.is_remote_disabled(&remote.hostname) {
                debug!("Skipping disabled remote: {}", remote.hostname);
                continue;
            }

            match self
                .push_to_remote(&agent_name, &new_messages, &remote.hostname)
                .await
            {
                Ok(count) => {
                    pushed_count += count;
                    debug!("Pushed {} messages to {}", count, remote.hostname);
                    // Reset failure count on success
                    self.metrics.reset_remote_failures(&remote.hostname);
                }
                Err(e) => {
                    warn!("Failed to push to {}: {}", remote.hostname, e);
                    self.metrics.record_remote_failure(&remote.hostname);

                    // Check if we should disable this remote
                    if self.metrics.get_remote_failures(&remote.hostname) >= CIRCUIT_BREAKER_THRESHOLD {
                        warn!(
                            "Remote {} disabled after {} consecutive failures",
                            remote.hostname, CIRCUIT_BREAKER_THRESHOLD
                        );
                        self.metrics.disable_remote(&remote.hostname);
                    }
                }
            }
        }

        // Update cursor and mark messages as synced
        self.state.set_cursor(rel_path.to_path_buf(), messages.len());
        for msg in &new_messages {
            if let Some(ref msg_id) = msg.message_id {
                self.state.mark_synced(msg_id.clone());
            }
        }

        Ok(pushed_count)
    }

    /// Push messages to a specific remote
    async fn push_to_remote(
        &self,
        agent_name: &str,
        messages: &[InboxMessage],
        remote_hostname: &str,
    ) -> Result<usize> {
        if messages.is_empty() {
            return Ok(0);
        }

        // Remote path: <team>/inboxes/<agent>.<local-hostname>.json
        let local_hostname = &self.config.local_hostname;
        let remote_filename = format!("{agent_name}.{local_hostname}.json");
        let remote_inboxes_dir = PathBuf::from(self.team_dir.file_name().unwrap())
            .join("inboxes");
        let remote_path = remote_inboxes_dir.join(&remote_filename);

        // Read existing messages from remote (if file exists)
        let remote_temp_path = self.team_dir.join(format!(".bridge-pull-{remote_hostname}.json"));

        let mut existing_messages = if self.transport.is_connected().await {
            match self.transport.download(&remote_path, &remote_temp_path).await {
                Ok(()) => {
                    let content = fs::read(&remote_temp_path).await?;
                    let msgs: Vec<InboxMessage> = serde_json::from_slice(&content)
                        .unwrap_or_default();
                    let _ = fs::remove_file(&remote_temp_path).await; // Cleanup
                    msgs
                }
                Err(_) => {
                    // File doesn't exist on remote yet - start with empty
                    Vec::new()
                }
            }
        } else {
            Vec::new()
        };

        // Merge new messages (append)
        existing_messages.extend_from_slice(messages);

        // Serialize merged messages
        let content = serde_json::to_vec_pretty(&existing_messages)?;

        // Write to local temp file
        let local_temp = self.team_dir.join(format!(".bridge-push-{remote_hostname}.json"));
        fs::write(&local_temp, &content).await?;

        // Upload to remote temp path
        let remote_temp = remote_inboxes_dir.join(format!(".bridge-tmp-{remote_filename}"));
        self.transport.upload(&local_temp, &remote_temp).await?;

        // Atomic rename on remote
        self.transport.rename(&remote_temp, &remote_path).await?;

        // Cleanup local temp file
        let _ = fs::remove_file(&local_temp).await;

        Ok(messages.len())
    }

    /// Pull messages from a specific remote
    async fn pull_from_remote(&mut self, remote_hostname: &str) -> Result<usize> {
        // Remote path: <team>/inboxes/*.{remote-hostname}.json
        let remote_inboxes_dir = PathBuf::from(self.team_dir.file_name().unwrap())
            .join("inboxes");

        // List files on remote matching pattern
        let pattern = format!("*.{remote_hostname}.json");
        let remote_files = match self.transport.list(&remote_inboxes_dir, &pattern).await {
            Ok(files) => files,
            Err(_) => {
                // Remote directory doesn't exist or is empty
                return Ok(0);
            }
        };

        let mut pulled_count = 0;
        for filename in remote_files {
            let remote_path = remote_inboxes_dir.join(&filename);

            // Local path: inboxes/<agent>.<remote-hostname>.json
            // Need to rewrite filename from <agent>.<local-hostname>.json to <agent>.<remote-hostname>.json
            let agent_name = self.extract_agent_from_origin_filename(&filename, remote_hostname)?;
            let local_filename = format!("{agent_name}.{remote_hostname}.json");
            let local_path = self.team_dir.join("inboxes").join(&local_filename);

            match self.pull_file(&remote_path, &local_path).await {
                Ok(count) => {
                    pulled_count += count;
                    debug!("Pulled {} messages from {}", count, remote_path.display());
                }
                Err(e) => {
                    warn!("Failed to pull {}: {}", remote_path.display(), e);
                }
            }
        }

        Ok(pulled_count)
    }

    /// Pull a single file from remote
    async fn pull_file(&mut self, remote_path: &Path, local_path: &Path) -> Result<usize> {
        // Download to temp file first
        let temp_path = local_path.with_extension("tmp");
        self.transport.download(remote_path, &temp_path).await?;

        // Read messages
        let content = fs::read(&temp_path).await?;
        let messages: Vec<InboxMessage> = serde_json::from_slice(&content)?;

        // Register self-write to avoid watcher feedback
        {
            let mut filter = self.self_write_filter.lock().await;
            filter.register(local_path.to_path_buf());
        }

        // Atomic rename to final path
        fs::rename(&temp_path, local_path).await?;

        Ok(messages.len())
    }

    /// Check if a path is a local inbox file (not a per-origin file)
    fn is_local_inbox_file(&self, path: &Path) -> bool {
        // Must end with .json
        if path.extension().and_then(|s| s.to_str()) != Some("json") {
            return false;
        }

        // Extract filename stem
        let Some(stem) = path.file_stem().and_then(|s| s.to_str()) else {
            return false;
        };

        if let Some(agent_names) = &self.agent_names {
            // Exact agent match is always local
            if agent_names.contains(stem) {
                return true;
            }

            // Check for per-origin pattern <agent>.<hostname> where hostname is known
            if let Some((agent, _hostname)) = self.split_origin(stem) {
                if agent_names.contains(&agent) {
                    return false;
                }
            }
        } else {
            // Fallback: treat any known hostname suffix as per-origin
            for remote in self.config.registry.remotes() {
                if stem.ends_with(&format!(".{}", remote.hostname)) {
                    return false;
                }
            }
            if stem.ends_with(&format!(".{}", self.config.local_hostname)) {
                return false;
            }
        }

        true
    }

    /// Extract agent name from inbox file path
    fn extract_agent_name(&self, path: &Path) -> Result<String> {
        let stem = path
            .file_stem()
            .and_then(|s| s.to_str())
            .context("Invalid filename")?;

        Ok(stem.to_string())
    }

    /// Extract agent name from origin filename
    ///
    /// Input: "agent-1.laptop.json"
    /// Output: "agent-1"
    fn extract_agent_from_origin_filename(
        &self,
        filename: &str,
        origin_hostname: &str,
    ) -> Result<String> {
        let stem = filename
            .strip_suffix(".json")
            .context("Filename must end with .json")?;

        // Remove origin hostname suffix
        let agent_name = stem
            .strip_suffix(&format!(".{origin_hostname}"))
            .context("Filename must end with origin hostname")?;

        Ok(agent_name.to_string())
    }

    pub(crate) async fn push_inbox_path(&mut self, path: &Path) -> Result<usize> {
        if !self.is_local_inbox_file(path) {
            return Ok(0);
        }
        self.push_inbox_file(path).await
    }

    fn split_origin(&self, stem: &str) -> Option<(String, String)> {
        let parts: Vec<&str> = stem.split('.').collect();
        if parts.len() < 2 {
            return None;
        }
        for i in (1..parts.len()).rev() {
            let potential_hostname = parts[i..].join(".");
            if self.config.registry.is_known_hostname(&potential_hostname) {
                let agent_name = parts[..i].join(".");
                return Some((agent_name, potential_hostname));
            }
        }
        None
    }
}

fn load_agent_names(team_dir: &Path) -> Option<HashSet<String>> {
    let config_path = team_dir.join("config.json");
    if !config_path.exists() {
        return None;
    }
    let content = std::fs::read(&config_path).ok()?;
    let config: TeamConfig = serde_json::from_slice(&content).ok()?;
    let mut names = HashSet::new();
    for member in config.members {
        names.insert(member.name);
    }
    Some(names)
}

#[cfg(test)]
mod tests {
    use super::*;
    use super::super::self_write_filter::SelfWriteFilter;
    use super::super::mock_transport::MockTransport;
    use atm_core::config::{BridgeConfig, BridgeRole, RemoteConfig, HostnameRegistry};
    use std::collections::HashMap;
    use tempfile::TempDir;
    use tokio::sync::Mutex as TokioMutex;

    fn create_test_config(local_hostname: &str, remote_hostname: &str) -> Arc<BridgePluginConfig> {
        let mut registry = HostnameRegistry::new();
        registry
            .register(RemoteConfig {
                hostname: remote_hostname.to_string(),
                address: format!("user@{remote_hostname}"),
                ssh_key_path: None,
                aliases: Vec::new(),
            })
            .unwrap();

        Arc::new(BridgePluginConfig {
            core: BridgeConfig {
                enabled: true,
                local_hostname: Some(local_hostname.to_string()),
                role: BridgeRole::Spoke,
                sync_interval_secs: 60,
                remotes: vec![RemoteConfig {
                    hostname: remote_hostname.to_string(),
                    address: format!("user@{remote_hostname}"),
                    ssh_key_path: None,
                    aliases: Vec::new(),
                }],
            },
            registry,
            local_hostname: local_hostname.to_string(),
        })
    }

    fn create_test_message(from: &str, text: &str, message_id: Option<String>) -> InboxMessage {
        InboxMessage {
            from: from.to_string(),
            text: text.to_string(),
            timestamp: chrono::Utc::now().to_rfc3339(),
            read: false,
            summary: None,
            message_id,
            unknown_fields: HashMap::new(),
        }
    }

    fn new_filter() -> Arc<TokioMutex<SelfWriteFilter>> {
        Arc::new(TokioMutex::new(SelfWriteFilter::default()))
    }

    async fn write_team_config(team_dir: &Path, members: &[&str]) {
        let mut members_json = Vec::new();
        for name in members {
            members_json.push(serde_json::json!({
                "agentId": format!("{name}@test-team"),
                "name": name,
                "agentType": "general-purpose",
                "model": "claude-opus-4-6",
                "joinedAt": 1234567890,
                "tmuxPaneId": null,
                "cwd": "/tmp",
                "subscriptions": []
            }));
        }
        let config = serde_json::json!({
            "name": "test-team",
            "createdAt": 1234567890,
            "leadAgentId": "team-lead@test-team",
            "leadSessionId": "session-123",
            "members": members_json
        });
        let path = team_dir.join("config.json");
        fs::write(path, serde_json::to_vec_pretty(&config).unwrap()).await.unwrap();
    }

    #[tokio::test]
    async fn test_sync_engine_new() {
        let temp_dir = TempDir::new().unwrap();
        let team_dir = temp_dir.path().to_path_buf();

        let config = create_test_config("laptop", "desktop");
        let transport = Arc::new(MockTransport::new()) as Arc<dyn Transport>;

        write_team_config(&team_dir, &["agent-1", "dev.mac"]).await;
        let engine = SyncEngine::new(config, transport, team_dir, new_filter()).await.unwrap();
        assert!(engine.state.synced_message_ids.is_empty());
    }

    #[tokio::test]
    async fn test_sync_push_empty_inbox() {
        let temp_dir = TempDir::new().unwrap();
        let team_dir = temp_dir.path().to_path_buf();

        let config = create_test_config("laptop", "desktop");
        let transport = Arc::new(MockTransport::new()) as Arc<dyn Transport>;

        let mut engine = SyncEngine::new(config, transport, team_dir, new_filter()).await.unwrap();

        // Push with no inboxes directory
        let stats = engine.sync_push().await.unwrap();
        assert_eq!(stats.messages_pushed, 0);
        assert_eq!(stats.errors, 0);
    }

    #[tokio::test]
    async fn test_assign_message_ids() {
        let mut messages = vec![
            create_test_message("user-a", "Message 1", None),
            create_test_message("user-b", "Message 2", Some("existing-id".to_string())),
        ];

        assign_message_ids(&mut messages);

        assert!(messages[0].message_id.is_some());
        assert_eq!(messages[1].message_id, Some("existing-id".to_string()));
    }

    #[tokio::test]
    async fn test_is_local_inbox_file() {
        let temp_dir = TempDir::new().unwrap();
        let team_dir = temp_dir.path().to_path_buf();
        let inboxes_dir = team_dir.join("inboxes");
        fs::create_dir_all(&inboxes_dir).await.unwrap();

        let config = create_test_config("laptop", "desktop");
        let transport = Arc::new(MockTransport::new()) as Arc<dyn Transport>;

        let engine = SyncEngine::new(config, transport, team_dir, new_filter()).await.unwrap();

        // Local inbox file
        let local = inboxes_dir.join("agent-1.json");
        assert!(engine.is_local_inbox_file(&local));

        // Per-origin inbox file (remote hostname)
        let origin_remote = inboxes_dir.join("agent-1.desktop.json");
        assert!(!engine.is_local_inbox_file(&origin_remote));

        // Per-origin inbox file (local hostname)
        let origin_local = inboxes_dir.join("agent-1.laptop.json");
        assert!(!engine.is_local_inbox_file(&origin_local));

        // Agent name containing hostname suffix should still be local
        let dot_agent = inboxes_dir.join("dev.mac.json");
        assert!(engine.is_local_inbox_file(&dot_agent));

        // Per-origin for dot agent should be treated as origin
        let dot_origin = inboxes_dir.join("dev.mac.desktop.json");
        assert!(!engine.is_local_inbox_file(&dot_origin));

        // Non-JSON file
        let txt_file = inboxes_dir.join("agent-1.txt");
        assert!(!engine.is_local_inbox_file(&txt_file));
    }

    #[tokio::test]
    async fn test_extract_agent_name() {
        let temp_dir = TempDir::new().unwrap();
        let team_dir = temp_dir.path().to_path_buf();

        let config = create_test_config("laptop", "desktop");
        let transport = Arc::new(MockTransport::new()) as Arc<dyn Transport>;

        let engine = SyncEngine::new(config, transport, team_dir.clone(), new_filter()).await.unwrap();

        let path = team_dir.join("inboxes/agent-1.json");
        let name = engine.extract_agent_name(&path).unwrap();
        assert_eq!(name, "agent-1");
    }

    #[tokio::test]
    async fn test_extract_agent_from_origin_filename() {
        let temp_dir = TempDir::new().unwrap();
        let team_dir = temp_dir.path().to_path_buf();

        let config = create_test_config("laptop", "desktop");
        let transport = Arc::new(MockTransport::new()) as Arc<dyn Transport>;

        let engine = SyncEngine::new(config, transport, team_dir, new_filter()).await.unwrap();

        let filename = "agent-1.laptop.json";
        let name = engine
            .extract_agent_from_origin_filename(filename, "laptop")
            .unwrap();
        assert_eq!(name, "agent-1");
    }

    #[tokio::test]
    async fn test_sync_stats_add() {
        let mut stats1 = SyncStats {
            messages_pushed: 5,
            messages_pulled: 3,
            errors: 1,
        };

        let stats2 = SyncStats {
            messages_pushed: 2,
            messages_pulled: 4,
            errors: 0,
        };

        stats1.add(&stats2);

        assert_eq!(stats1.messages_pushed, 7);
        assert_eq!(stats1.messages_pulled, 7);
        assert_eq!(stats1.errors, 1);
    }
}
