//! File system watcher for inbox directories

use anyhow::{Context, Result};
use notify::{Event, EventKind, RecommendedWatcher, RecursiveMode, Watcher};
use std::path::{Path, PathBuf};
use std::sync::mpsc::channel;
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;
use tracing::{debug, error, info, warn};

/// Event from the inbox watcher
#[derive(Debug, Clone)]
pub struct InboxEvent {
    pub team: String,
    pub agent: String,
    pub path: PathBuf,
    pub kind: InboxEventKind,
    /// Origin hostname for per-origin inbox files (None for local inbox)
    pub origin: Option<String>,
}

/// Type of inbox event
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InboxEventKind {
    /// New or modified inbox file
    MessageReceived,
    /// Inbox file deleted
    FileRemoved,
}

/// Watch team inbox directories for changes.
///
/// This sets up a file system watcher on the teams root directory and produces
/// InboxEvent messages on the event channel for each relevant file system change.
/// Events are filtered to only include inbox JSON files.
///
/// # Arguments
///
/// * `teams_root` - Root directory containing team inboxes (e.g., ~/.claude/teams)
/// * `event_tx` - Channel sender for inbox events
/// * `hostname_registry` - Optional hostname registry for parsing per-origin inbox files
/// * `cancel` - Cancellation token to stop watching
pub async fn watch_inboxes(
    teams_root: PathBuf,
    event_tx: mpsc::Sender<InboxEvent>,
    hostname_registry: Option<std::sync::Arc<agent_team_mail_core::config::HostnameRegistry>>,
    cancel: CancellationToken,
) -> Result<()> {
    info!("Starting inbox watcher for: {}", teams_root.display());

    // Create a channel to receive file system events from notify
    let (tx, rx) = channel();

    // Create the watcher
    let mut watcher: RecommendedWatcher = notify::recommended_watcher(move |res: notify::Result<Event>| {
        match res {
            Ok(event) => {
                if let Err(e) = tx.send(event) {
                    error!("Failed to send file system event: {}", e);
                }
            }
            Err(e) => {
                error!("File system watcher error: {}", e);
            }
        }
    })
    .context("Failed to create file system watcher")?;

    // Start watching the teams root directory recursively
    watcher
        .watch(&teams_root, RecursiveMode::Recursive)
        .context("Failed to watch teams directory")?;

    info!("Watching {} for changes", teams_root.display());

    // Event processing loop
    // Spawn a blocking task to handle the synchronous mpsc receiver
    let cancel_clone = cancel.clone();
    let teams_root_clone = teams_root.clone();
    let registry_clone = hostname_registry.clone();
    tokio::task::spawn_blocking(move || {
        loop {
            if cancel_clone.is_cancelled() {
                info!("Inbox watcher cancelled");
                break;
            }

            // Use recv_timeout to avoid busy-wait polling
            match rx.recv_timeout(std::time::Duration::from_millis(100)) {
                Ok(event) => {
                    debug!("File system event: {:?}", event);

                    // Parse the event and send to async channel if it's relevant
                    if let Some(inbox_events) = parse_event(&teams_root_clone, event, registry_clone.as_deref()) {
                        for inbox_event in inbox_events {
                            // Use blocking_send since we're in a blocking task
                            if let Err(e) = event_tx.blocking_send(inbox_event) {
                                error!("Failed to send inbox event: {}", e);
                            }
                        }
                    }
                }
                Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {
                    // Timeout - check cancellation and continue
                    continue;
                }
                Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => {
                    warn!("Watcher channel disconnected");
                    break;
                }
            }
        }
    })
    .await
    .context("Watcher task panicked")?;

    Ok(())
}

/// Parse a notify Event into zero or more InboxEvents.
///
/// Returns None if the event is not relevant (non-inbox file, config.json, etc).
/// Returns Some(vec) with InboxEvent(s) if the event is for an inbox file.
///
/// Path pattern: <teams_root>/<team>/inboxes/<agent>.json or <agent>.<hostname>.json
///
/// # Arguments
///
/// * `teams_root` - Root directory for teams
/// * `event` - File system event
/// * `hostname_registry` - Optional hostname registry for parsing per-origin files
fn parse_event(
    teams_root: &Path,
    event: Event,
    hostname_registry: Option<&agent_team_mail_core::config::HostnameRegistry>,
) -> Option<Vec<InboxEvent>> {
    let mut events = Vec::new();

    // Map event kind to InboxEventKind
    let inbox_kind = match event.kind {
        EventKind::Create(_) | EventKind::Modify(_) => InboxEventKind::MessageReceived,
        EventKind::Remove(_) => InboxEventKind::FileRemoved,
        _ => return None, // Ignore other event types
    };

    // Process each path in the event
    for path in event.paths {
        // Only process JSON files
        if path.extension().and_then(|s| s.to_str()) != Some("json") {
            continue;
        }

        // Extract team and agent from path
        // Path pattern: <teams_root>/<team>/inboxes/<agent>.json or <agent>.<hostname>.json
        let rel_path = match path.strip_prefix(teams_root) {
            Ok(p) => p,
            Err(_) => continue, // Not under teams_root
        };

        let components: Vec<_> = rel_path.components().collect();

        // Need at least: <team>/inboxes/<agent>.json (3 components)
        if components.len() < 3 {
            continue;
        }

        // Extract team name (first component)
        let team = match components[0].as_os_str().to_str() {
            Some(s) => s.to_string(),
            None => continue,
        };

        // Check if second component is "inboxes"
        if components[1].as_os_str().to_str() != Some("inboxes") {
            continue;
        }

        // Extract filename without extension
        let file_stem = match path.file_stem().and_then(|s| s.to_str()) {
            Some(s) => s,
            None => continue,
        };

        // Parse agent name and origin
        // Format: <agent>.json (local) or <agent>.<hostname>.json (origin)
        let (agent, origin) = parse_agent_and_origin(file_stem, hostname_registry);

        events.push(InboxEvent {
            team,
            agent,
            path: path.clone(),
            kind: inbox_kind,
            origin,
        });
    }

    if events.is_empty() {
        None
    } else {
        Some(events)
    }
}

/// Parse agent name and origin from file stem
///
/// Handles both local files (<agent>) and per-origin files (<agent>.<hostname>).
/// Uses hostname registry to determine if a suffix is a known hostname.
///
/// # Arguments
///
/// * `file_stem` - Filename without .json extension (e.g., "dev-agent" or "dev-agent.mac-studio")
/// * `hostname_registry` - Optional hostname registry
///
/// # Returns
///
/// Tuple of (agent_name, origin) where origin is Some(hostname) for origin files, None for local files
fn parse_agent_and_origin(
    file_stem: &str,
    hostname_registry: Option<&agent_team_mail_core::config::HostnameRegistry>,
) -> (String, Option<String>) {
    // If no hostname registry, treat entire stem as agent name
    let Some(registry) = hostname_registry else {
        return (file_stem.to_string(), None);
    };

    // Try to find a suffix that matches a known hostname
    // Walk backwards through dots to handle agent names with dots
    let parts: Vec<&str> = file_stem.split('.').collect();

    for i in (1..parts.len()).rev() {
        let potential_hostname = parts[i..].join(".");
        if registry.is_known_hostname(&potential_hostname) {
            // Found a match - everything before this is the agent name
            let agent_name = parts[..i].join(".");
            return (agent_name, Some(potential_hostname));
        }
    }

    // No hostname match found - treat entire stem as agent name (local inbox)
    (file_stem.to_string(), None)
}


#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_event_inbox_create() {
        let teams_root = PathBuf::from("/tmp/teams");
        let inbox_path = teams_root.join("my-team/inboxes/agent-1.json");

        let event = Event {
            kind: EventKind::Create(notify::event::CreateKind::File),
            paths: vec![inbox_path.clone()],
            attrs: Default::default(),
        };

        let result = parse_event(&teams_root, event, None);
        assert!(result.is_some());

        let events = result.unwrap();
        assert_eq!(events.len(), 1);

        let inbox_event = &events[0];
        assert_eq!(inbox_event.team, "my-team");
        assert_eq!(inbox_event.agent, "agent-1");
        assert_eq!(inbox_event.path, inbox_path);
        assert_eq!(inbox_event.kind, InboxEventKind::MessageReceived);
        assert_eq!(inbox_event.origin, None);
    }

    #[test]
    fn test_parse_event_inbox_modify() {
        let teams_root = PathBuf::from("/tmp/teams");
        let inbox_path = teams_root.join("team-2/inboxes/agent-x.json");

        let event = Event {
            kind: EventKind::Modify(notify::event::ModifyKind::Data(notify::event::DataChange::Any)),
            paths: vec![inbox_path.clone()],
            attrs: Default::default(),
        };

        let result = parse_event(&teams_root, event, None);
        assert!(result.is_some());

        let events = result.unwrap();
        assert_eq!(events.len(), 1);

        let inbox_event = &events[0];
        assert_eq!(inbox_event.kind, InboxEventKind::MessageReceived);
        assert_eq!(inbox_event.origin, None);
    }

    #[test]
    fn test_parse_event_inbox_remove() {
        let teams_root = PathBuf::from("/tmp/teams");
        let inbox_path = teams_root.join("team-3/inboxes/agent-y.json");

        let event = Event {
            kind: EventKind::Remove(notify::event::RemoveKind::File),
            paths: vec![inbox_path.clone()],
            attrs: Default::default(),
        };

        let result = parse_event(&teams_root, event, None);
        assert!(result.is_some());

        let events = result.unwrap();
        assert_eq!(events.len(), 1);

        let inbox_event = &events[0];
        assert_eq!(inbox_event.kind, InboxEventKind::FileRemoved);
        assert_eq!(inbox_event.origin, None);
    }

    #[test]
    fn test_parse_event_non_inbox_file() {
        let teams_root = PathBuf::from("/tmp/teams");
        let config_path = teams_root.join("my-team/config.json");

        let event = Event {
            kind: EventKind::Create(notify::event::CreateKind::File),
            paths: vec![config_path],
            attrs: Default::default(),
        };

        // Should be ignored (not in inbox/ subdirectory)
        let result = parse_event(&teams_root, event, None);
        assert!(result.is_none());
    }

    #[test]
    fn test_parse_event_non_json_file() {
        let teams_root = PathBuf::from("/tmp/teams");
        let txt_path = teams_root.join("my-team/inboxes/agent-1.txt");

        let event = Event {
            kind: EventKind::Create(notify::event::CreateKind::File),
            paths: vec![txt_path],
            attrs: Default::default(),
        };

        // Should be ignored (not .json)
        let result = parse_event(&teams_root, event, None);
        assert!(result.is_none());
    }

    #[test]
    fn test_parse_event_multiple_paths() {
        let teams_root = PathBuf::from("/tmp/teams");
        let inbox_path1 = teams_root.join("team-1/inboxes/agent-a.json");
        let inbox_path2 = teams_root.join("team-1/inboxes/agent-b.json");

        let event = Event {
            kind: EventKind::Create(notify::event::CreateKind::File),
            paths: vec![inbox_path1.clone(), inbox_path2.clone()],
            attrs: Default::default(),
        };

        let result = parse_event(&teams_root, event, None);
        assert!(result.is_some());

        let events = result.unwrap();
        assert_eq!(events.len(), 2);
        assert_eq!(events[0].agent, "agent-a");
        assert_eq!(events[1].agent, "agent-b");
    }

    #[test]
    fn test_parse_event_per_origin_file() {
        use agent_team_mail_core::config::{HostnameRegistry, RemoteConfig};

        let teams_root = PathBuf::from("/tmp/teams");
        let origin_path = teams_root.join("my-team/inboxes/agent-1.mac-studio.json");

        // Create hostname registry
        let mut registry = HostnameRegistry::new();
        registry
            .register(RemoteConfig {
                hostname: "mac-studio".to_string(),
                address: "user@mac".to_string(),
                ssh_key_path: None,
                aliases: Vec::new(),
            })
            .unwrap();

        let event = Event {
            kind: EventKind::Create(notify::event::CreateKind::File),
            paths: vec![origin_path.clone()],
            attrs: Default::default(),
        };

        let result = parse_event(&teams_root, event, Some(&registry));
        assert!(result.is_some());

        let events = result.unwrap();
        assert_eq!(events.len(), 1);

        let inbox_event = &events[0];
        assert_eq!(inbox_event.team, "my-team");
        assert_eq!(inbox_event.agent, "agent-1");
        assert_eq!(inbox_event.origin, Some("mac-studio".to_string()));
    }

    #[test]
    fn test_parse_event_per_origin_file_with_dotted_agent() {
        use agent_team_mail_core::config::{HostnameRegistry, RemoteConfig};

        let teams_root = PathBuf::from("/tmp/teams");
        let origin_path = teams_root.join("my-team/inboxes/dev.agent.mac-studio.json");

        // Create hostname registry
        let mut registry = HostnameRegistry::new();
        registry
            .register(RemoteConfig {
                hostname: "mac-studio".to_string(),
                address: "user@mac".to_string(),
                ssh_key_path: None,
                aliases: Vec::new(),
            })
            .unwrap();

        let event = Event {
            kind: EventKind::Create(notify::event::CreateKind::File),
            paths: vec![origin_path.clone()],
            attrs: Default::default(),
        };

        let result = parse_event(&teams_root, event, Some(&registry));
        assert!(result.is_some());

        let events = result.unwrap();
        assert_eq!(events.len(), 1);

        let inbox_event = &events[0];
        assert_eq!(inbox_event.team, "my-team");
        assert_eq!(inbox_event.agent, "dev.agent");
        assert_eq!(inbox_event.origin, Some("mac-studio".to_string()));
    }

    #[test]
    fn test_parse_event_unknown_hostname_treated_as_local() {
        use agent_team_mail_core::config::{HostnameRegistry, RemoteConfig};

        let teams_root = PathBuf::from("/tmp/teams");
        let unknown_path = teams_root.join("my-team/inboxes/agent-1.unknown-host.json");

        // Create hostname registry without unknown-host
        let mut registry = HostnameRegistry::new();
        registry
            .register(RemoteConfig {
                hostname: "known-host".to_string(),
                address: "user@known".to_string(),
                ssh_key_path: None,
                aliases: Vec::new(),
            })
            .unwrap();

        let event = Event {
            kind: EventKind::Create(notify::event::CreateKind::File),
            paths: vec![unknown_path.clone()],
            attrs: Default::default(),
        };

        let result = parse_event(&teams_root, event, Some(&registry));
        assert!(result.is_some());

        let events = result.unwrap();
        assert_eq!(events.len(), 1);

        let inbox_event = &events[0];
        // Entire stem treated as agent name since hostname not recognized
        assert_eq!(inbox_event.agent, "agent-1.unknown-host");
        assert_eq!(inbox_event.origin, None);
    }
}
