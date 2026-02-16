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

    /// Transport implementations per remote hostname
    transports: std::collections::HashMap<String, Arc<tokio::sync::Mutex<dyn Transport>>>,

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
    /// * `transports` - Transport implementations per remote hostname
    /// * `team_dir` - Path to team directory
    ///
    /// # Errors
    ///
    /// Returns error if sync state cannot be loaded
    pub async fn new(
        config: Arc<BridgePluginConfig>,
        transports: std::collections::HashMap<String, Arc<tokio::sync::Mutex<dyn Transport>>>,
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
            transports,
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

        // Collect all local inbox files first
        let mut inbox_files = Vec::new();
        while let Ok(Some(entry)) = entries.next_entry().await {
            let path = entry.path();

            // Only process local inbox files (not per-origin files)
            if self.is_local_inbox_file(&path) {
                inbox_files.push(path);
            }
        }

        // Sort remote hostnames for deterministic ordering
        let mut remote_hostnames: Vec<_> = self.transports.keys().cloned().collect();
        remote_hostnames.sort();

        // Lazy connect to each remote (if not already connected)
        for remote_hostname in &remote_hostnames {
            if self.metrics.is_remote_disabled(remote_hostname) {
                continue;
            }

            if let Some(transport) = self.transports.get(remote_hostname) {
                let mut transport_guard = transport.lock().await;
                if !transport_guard.is_connected().await {
                    if let Err(e) = transport_guard.connect().await {
                        warn!("Failed to connect to {}: {}", remote_hostname, e);
                        stats.errors += 1;
                        self.metrics.record_remote_failure(remote_hostname);
                    }
                }
            }
        }

        // Push each inbox file to all remotes
        for path in inbox_files {
            for remote_hostname in &remote_hostnames {
                // Check circuit breaker
                if self.metrics.is_remote_disabled(remote_hostname) {
                    debug!("Skipping disabled remote: {}", remote_hostname);
                    continue;
                }

                match self.push_inbox_file_to_remote(&path, remote_hostname).await {
                    Ok(pushed) => {
                        stats.messages_pushed += pushed;
                        if pushed > 0 {
                            debug!("Pushed {} messages to {}", pushed, remote_hostname);
                        }
                        // Reset failure count on success
                        self.metrics.reset_remote_failures(remote_hostname);
                    }
                    Err(e) => {
                        warn!("Failed to push {} to {}: {}", path.display(), remote_hostname, e);
                        stats.errors += 1;
                        self.metrics.record_remote_failure(remote_hostname);

                        // Check if we should disable this remote
                        if self.metrics.get_remote_failures(remote_hostname) >= CIRCUIT_BREAKER_THRESHOLD {
                            warn!(
                                "Remote {} disabled after {} consecutive failures",
                                remote_hostname, CIRCUIT_BREAKER_THRESHOLD
                            );
                            self.metrics.disable_remote(remote_hostname);
                        }
                    }
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

        // Sort remote hostnames for deterministic ordering
        let mut remote_hostnames: Vec<_> = self.transports.keys().cloned().collect();
        remote_hostnames.sort();

        // Lazy connect to each remote (if not already connected)
        for remote_hostname in &remote_hostnames {
            if self.metrics.is_remote_disabled(remote_hostname) {
                continue;
            }

            if let Some(transport) = self.transports.get(remote_hostname) {
                let mut transport_guard = transport.lock().await;
                if !transport_guard.is_connected().await {
                    if let Err(e) = transport_guard.connect().await {
                        warn!("Failed to connect to {}: {}", remote_hostname, e);
                        stats.errors += 1;
                        self.metrics.record_remote_failure(remote_hostname);
                    }
                }
            }
        }

        for remote_hostname in remote_hostnames {
            // Check circuit breaker
            if self.metrics.is_remote_disabled(&remote_hostname) {
                debug!("Skipping disabled remote: {}", remote_hostname);
                continue;
            }

            match self.pull_from_remote(&remote_hostname).await {
                Ok(pulled) => {
                    stats.messages_pulled += pulled;
                    if pulled > 0 {
                        debug!("Pulled {} messages from {}", pulled, remote_hostname);
                    }
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
            if let Some(hub_transport_arc) = self.get_transport(&hub_hostname) {
                let hub_transport = hub_transport_arc.lock().await;
                match sync_team_config(
                    &*hub_transport,
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
            } else {
                warn!("No transport configured for hub: {}", hub_hostname);
                stats.errors += 1;
            }
        }

        // Push local changes
        let push_stats = self.sync_push().await?;
        stats.add(&push_stats);

        // Pull remote changes
        let pull_stats = self.sync_pull().await?;
        stats.add(&pull_stats);

        // Apply retention policy to per-origin files
        if let Err(e) = self.apply_retention_policy().await {
            warn!("Failed to apply retention policy: {}", e);
            stats.errors += 1;
        }

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

    /// Apply retention policy to per-origin inbox files
    ///
    /// Per-origin files (`<agent>.<hostname>.json`) grow unbounded over time.
    /// This function trims old messages beyond a configured retention limit.
    ///
    /// Current policy: Keep last 1000 messages per per-origin file.
    ///
    /// # Errors
    ///
    /// Returns error if critical file operations fail
    async fn apply_retention_policy(&self) -> Result<usize> {
        const MAX_MESSAGES_PER_ORIGIN: usize = 1000;

        let inboxes_dir = self.team_dir.join("inboxes");
        if !inboxes_dir.exists() {
            return Ok(0);
        }

        let mut entries = fs::read_dir(&inboxes_dir).await?;
        let mut trimmed_count = 0;

        while let Ok(Some(entry)) = entries.next_entry().await {
            let path = entry.path();

            // Only process per-origin files (contain hostname suffix)
            if !self.is_local_inbox_file(&path) && path.extension().and_then(|s| s.to_str()) == Some("json") {
                match self.trim_per_origin_file(&path, MAX_MESSAGES_PER_ORIGIN).await {
                    Ok(trimmed) => {
                        if trimmed > 0 {
                            debug!("Trimmed {} messages from {}", trimmed, path.display());
                            trimmed_count += trimmed;
                        }
                    }
                    Err(e) => {
                        warn!("Failed to trim {}: {}", path.display(), e);
                    }
                }
            }
        }

        Ok(trimmed_count)
    }

    /// Trim a per-origin inbox file to a maximum number of messages
    ///
    /// Keeps the most recent N messages and discards older ones.
    async fn trim_per_origin_file(&self, path: &Path, max_messages: usize) -> Result<usize> {
        // Read current messages
        let content = fs::read(path).await?;
        let messages: Vec<InboxMessage> = serde_json::from_slice(&content)?;

        // Check if trimming is needed
        if messages.len() <= max_messages {
            return Ok(0);
        }

        // Keep only the last N messages
        let trimmed_count = messages.len() - max_messages;
        let kept_messages = &messages[trimmed_count..];

        // Write back to file (atomic)
        let temp_path = path.with_extension("retention-tmp");
        let content = serde_json::to_vec_pretty(kept_messages)?;
        fs::write(&temp_path, &content).await?;
        fs::rename(&temp_path, path).await?;

        Ok(trimmed_count)
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

    /// Get transport for a specific remote hostname
    fn get_transport(&self, remote_hostname: &str) -> Option<&Arc<tokio::sync::Mutex<dyn Transport>>> {
        self.transports.get(remote_hostname)
    }

    /// Push a single inbox file to a specific remote
    async fn push_inbox_file_to_remote(&mut self, local_path: &Path, remote_hostname: &str) -> Result<usize> {
        // Get transport for this remote
        let transport_arc = self.get_transport(remote_hostname)
            .ok_or_else(|| anyhow::anyhow!("No transport for remote: {remote_hostname}"))?
            .clone();

        // Read inbox file
        let content = fs::read(local_path).await?;
        let mut messages: Vec<InboxMessage> = serde_json::from_slice(&content)
            .context("Failed to parse inbox file")?;

        // Assign message_ids to messages that don't have one
        assign_message_ids(&mut messages);

        // Get cursor for this file + remote combination
        let rel_path = local_path.strip_prefix(&self.team_dir)?;
        let cursor_key = format!("{}:{}", rel_path.display(), remote_hostname);
        let cursor = self.state.get_cursor(Path::new(&cursor_key));

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
            return Ok(0);
        }

        // Extract agent name from filename
        let agent_name = self.extract_agent_name(local_path)?;

        // Push to this remote
        let pushed_count = self
            .push_to_remote(&agent_name, &new_messages, remote_hostname, &transport_arc)
            .await?;

        // Update cursor and mark messages as synced
        self.state.set_cursor(PathBuf::from(cursor_key), messages.len());
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
        transport_arc: &Arc<tokio::sync::Mutex<dyn Transport>>,
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

        let transport = transport_arc.lock().await;
        let mut existing_messages = if transport.is_connected().await {
            match transport.download(&remote_path, &remote_temp_path).await {
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
        transport.upload(&local_temp, &remote_temp).await?;

        // Atomic rename on remote
        transport.rename(&remote_temp, &remote_path).await?;

        // Cleanup local temp file
        let _ = fs::remove_file(&local_temp).await;

        Ok(messages.len())
    }

    /// Pull messages from a specific remote
    async fn pull_from_remote(&mut self, remote_hostname: &str) -> Result<usize> {
        // Get transport for this remote
        let transport_arc = self.get_transport(remote_hostname)
            .ok_or_else(|| anyhow::anyhow!("No transport for remote: {remote_hostname}"))?
            .clone();

        // Remote path: <team>/inboxes/*.json (base inbox files, not per-origin files)
        let remote_inboxes_dir = PathBuf::from(self.team_dir.file_name().unwrap())
            .join("inboxes");

        // List files on remote matching pattern *.json
        let pattern = "*.json";
        let transport = transport_arc.lock().await;
        let remote_files = match transport.list(&remote_inboxes_dir, pattern).await {
            Ok(files) => files,
            Err(_) => {
                // Remote directory doesn't exist or is empty
                return Ok(0);
            }
        };

        let mut pulled_count = 0;
        for filename in remote_files {
            // Skip temp files created by bridge operations
            if filename.starts_with(".bridge-") {
                debug!("Skipping temp file: {}", filename);
                continue;
            }

            if !filename.ends_with(".json") {
                continue;
            }

            let stem = filename.strip_suffix(".json").unwrap();

            // Check if this is a per-origin file from another machine
            // Per-origin files have format: agent.hostname.json
            // We only want to pull BASE inbox files (agent.json), not per-origin files
            let is_per_origin_file = if let Some(last_dot_idx) = stem.rfind('.') {
                let potential_hostname = &stem[last_dot_idx + 1..];
                // Check if the suffix after the last dot is a known hostname
                self.config.registry.is_known_hostname(potential_hostname)
            } else {
                false
            };

            if is_per_origin_file {
                debug!("Skipping per-origin file: {}", filename);
                continue;
            }

            // This is a base inbox file - download it and save as agent.remote-hostname.json
            let remote_path = remote_inboxes_dir.join(&filename);
            let local_filename = format!("{stem}.{remote_hostname}.json");
            let local_path = self.team_dir.join("inboxes").join(&local_filename);

            match self.pull_file(&remote_path, &local_path, &transport).await {
                Ok(count) => {
                    pulled_count += count;
                }
                Err(e) => {
                    warn!("Failed to pull {} from {}: {}", filename, remote_hostname, e);
                }
            }
        }

        Ok(pulled_count)
    }

    /// Pull a single file from remote
    async fn pull_file(&mut self, remote_path: &Path, local_path: &Path, transport: &tokio::sync::MutexGuard<'_, dyn Transport>) -> Result<usize> {
        // Download to temp file first
        let temp_path = local_path.with_extension("tmp");
        transport.download(remote_path, &temp_path).await?;

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
            if let Some((agent, _hostname)) = self.split_origin(stem)
                && agent_names.contains(&agent)
            {
                return false;
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

    pub(crate) async fn push_inbox_path(&mut self, path: &Path) -> Result<usize> {
        if !self.is_local_inbox_file(path) {
            return Ok(0);
        }

        let mut total_pushed = 0;
        // Sort remote hostnames for deterministic ordering
        let mut remote_hostnames: Vec<_> = self.transports.keys().cloned().collect();
        remote_hostnames.sort();

        for remote_hostname in remote_hostnames {
            // Check circuit breaker
            if self.metrics.is_remote_disabled(&remote_hostname) {
                debug!("Skipping disabled remote: {}", remote_hostname);
                continue;
            }

            match self.push_inbox_file_to_remote(path, &remote_hostname).await {
                Ok(pushed) => {
                    total_pushed += pushed;
                }
                Err(e) => {
                    warn!("Failed to push {} to {}: {}", path.display(), remote_hostname, e);
                }
            }
        }

        Ok(total_pushed)
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
        let transport = Arc::new(tokio::sync::Mutex::new(MockTransport::new())) as Arc<tokio::sync::Mutex<dyn Transport>>;
        let mut transports = HashMap::new();
        transports.insert("desktop".to_string(), transport);

        write_team_config(&team_dir, &["agent-1", "dev.mac"]).await;
        let engine = SyncEngine::new(config, transports, team_dir, new_filter()).await.unwrap();
        assert!(engine.state.synced_message_ids.is_empty());
    }

    #[tokio::test]
    async fn test_sync_push_empty_inbox() {
        let temp_dir = TempDir::new().unwrap();
        let team_dir = temp_dir.path().to_path_buf();

        let config = create_test_config("laptop", "desktop");
        let transport = Arc::new(tokio::sync::Mutex::new(MockTransport::new())) as Arc<tokio::sync::Mutex<dyn Transport>>;
        let mut transports = HashMap::new();
        transports.insert("desktop".to_string(), transport);

        let mut engine = SyncEngine::new(config, transports, team_dir, new_filter()).await.unwrap();

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
        let transport = Arc::new(tokio::sync::Mutex::new(MockTransport::new())) as Arc<tokio::sync::Mutex<dyn Transport>>;
        let mut transports = HashMap::new();
        transports.insert("desktop".to_string(), transport);

        let engine = SyncEngine::new(config, transports, team_dir, new_filter()).await.unwrap();

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
        let transport = Arc::new(tokio::sync::Mutex::new(MockTransport::new())) as Arc<tokio::sync::Mutex<dyn Transport>>;
        let mut transports = HashMap::new();
        transports.insert("desktop".to_string(), transport);

        let engine = SyncEngine::new(config, transports, team_dir.clone(), new_filter()).await.unwrap();

        let path = team_dir.join("inboxes/agent-1.json");
        let name = engine.extract_agent_name(&path).unwrap();
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

    #[tokio::test]
    async fn test_trim_per_origin_file() {
        let temp_dir = TempDir::new().unwrap();
        let team_dir = temp_dir.path().to_path_buf();
        let inboxes_dir = team_dir.join("inboxes");
        fs::create_dir_all(&inboxes_dir).await.unwrap();

        let config = create_test_config("laptop", "desktop");
        let transport = Arc::new(tokio::sync::Mutex::new(MockTransport::new())) as Arc<tokio::sync::Mutex<dyn Transport>>;
        let mut transports = HashMap::new();
        transports.insert("desktop".to_string(), transport);

        let engine = SyncEngine::new(config, transports, team_dir.clone(), new_filter())
            .await
            .unwrap();

        // Create per-origin file with 10 messages
        let mut messages = Vec::new();
        for i in 0..10 {
            messages.push(create_test_message(&format!("user-{i}"), &format!("Message {i}"), None));
        }

        let per_origin_file = inboxes_dir.join("agent-1.desktop.json");
        let content = serde_json::to_vec_pretty(&messages).unwrap();
        fs::write(&per_origin_file, content).await.unwrap();

        // Trim to 5 messages
        let trimmed = engine.trim_per_origin_file(&per_origin_file, 5).await.unwrap();
        assert_eq!(trimmed, 5);

        // Verify only last 5 messages remain
        let content = fs::read(&per_origin_file).await.unwrap();
        let trimmed_messages: Vec<InboxMessage> = serde_json::from_slice(&content).unwrap();
        assert_eq!(trimmed_messages.len(), 5);
        assert_eq!(trimmed_messages[0].from, "user-5");
        assert_eq!(trimmed_messages[4].from, "user-9");
    }

    #[tokio::test]
    async fn test_trim_per_origin_file_no_trim_needed() {
        let temp_dir = TempDir::new().unwrap();
        let team_dir = temp_dir.path().to_path_buf();
        let inboxes_dir = team_dir.join("inboxes");
        fs::create_dir_all(&inboxes_dir).await.unwrap();

        let config = create_test_config("laptop", "desktop");
        let transport = Arc::new(tokio::sync::Mutex::new(MockTransport::new())) as Arc<tokio::sync::Mutex<dyn Transport>>;
        let mut transports = HashMap::new();
        transports.insert("desktop".to_string(), transport);

        let engine = SyncEngine::new(config, transports, team_dir.clone(), new_filter())
            .await
            .unwrap();

        // Create per-origin file with 3 messages
        let messages = vec![
            create_test_message("user-1", "Message 1", None),
            create_test_message("user-2", "Message 2", None),
            create_test_message("user-3", "Message 3", None),
        ];

        let per_origin_file = inboxes_dir.join("agent-1.desktop.json");
        let content = serde_json::to_vec_pretty(&messages).unwrap();
        fs::write(&per_origin_file, content).await.unwrap();

        // Trim to 5 messages (no trim should happen)
        let trimmed = engine.trim_per_origin_file(&per_origin_file, 5).await.unwrap();
        assert_eq!(trimmed, 0);

        // Verify all 3 messages remain
        let content = fs::read(&per_origin_file).await.unwrap();
        let trimmed_messages: Vec<InboxMessage> = serde_json::from_slice(&content).unwrap();
        assert_eq!(trimmed_messages.len(), 3);
    }

    #[tokio::test]
    async fn test_apply_retention_policy() {
        let temp_dir = TempDir::new().unwrap();
        let team_dir = temp_dir.path().to_path_buf();
        let inboxes_dir = team_dir.join("inboxes");
        fs::create_dir_all(&inboxes_dir).await.unwrap();

        let config = create_test_config("laptop", "desktop");
        let transport = Arc::new(tokio::sync::Mutex::new(MockTransport::new())) as Arc<tokio::sync::Mutex<dyn Transport>>;
        let mut transports = HashMap::new();
        transports.insert("desktop".to_string(), transport);

        write_team_config(&team_dir, &["agent-1"]).await;
        let engine = SyncEngine::new(config, transports, team_dir.clone(), new_filter())
            .await
            .unwrap();

        // Create local inbox (should NOT be trimmed)
        let local_messages = vec![create_test_message("user-1", "Local message", None)];
        let local_inbox = inboxes_dir.join("agent-1.json");
        fs::write(&local_inbox, serde_json::to_vec_pretty(&local_messages).unwrap())
            .await
            .unwrap();

        // Create per-origin file with 1100 messages (exceeds limit)
        let mut origin_messages = Vec::new();
        for i in 0..1100 {
            origin_messages.push(create_test_message(&format!("user-{i}"), &format!("Msg {i}"), None));
        }
        let origin_file = inboxes_dir.join("agent-1.desktop.json");
        fs::write(&origin_file, serde_json::to_vec_pretty(&origin_messages).unwrap())
            .await
            .unwrap();

        // Apply retention policy
        let trimmed = engine.apply_retention_policy().await.unwrap();
        assert_eq!(trimmed, 100); // 1100 - 1000 = 100 trimmed

        // Verify per-origin file was trimmed
        let content = fs::read(&origin_file).await.unwrap();
        let trimmed_messages: Vec<InboxMessage> = serde_json::from_slice(&content).unwrap();
        assert_eq!(trimmed_messages.len(), 1000);
        assert_eq!(trimmed_messages[0].from, "user-100");
        assert_eq!(trimmed_messages[999].from, "user-1099");

        // Verify local inbox was NOT trimmed
        let content = fs::read(&local_inbox).await.unwrap();
        let local: Vec<InboxMessage> = serde_json::from_slice(&content).unwrap();
        assert_eq!(local.len(), 1);
    }
}
