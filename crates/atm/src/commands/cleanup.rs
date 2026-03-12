//! Cleanup command implementation - apply retention policies to inboxes

use agent_team_mail_core::config::{ConfigOverrides, resolve_config};
use agent_team_mail_core::io::inbox::inbox_append;
use agent_team_mail_core::retention::apply_retention;
use agent_team_mail_core::schema::{InboxMessage, TeamConfig};
use anyhow::{Context, Result};
use chrono::Utc;
use clap::Args;
use std::collections::HashMap;
use std::path::Path;
use std::time::{Duration, Instant};
use uuid::Uuid;

use crate::commands::teams;
use crate::util::settings::{get_home_dir, teams_root_dir_for};

/// Apply retention policies to clean up old messages
#[derive(Args, Debug)]
pub struct CleanupArgs {
    /// Team name (uses default if not specified)
    #[arg(short, long)]
    team: Option<String>,

    /// Remove stale state for a specific agent (compatibility alias for teams cleanup)
    #[arg(long)]
    agent: Option<String>,

    /// Apply to all teams
    #[arg(long)]
    all_teams: bool,

    /// Show what would be cleaned without modifying
    #[arg(long)]
    dry_run: bool,

    /// Force cleanup when daemon liveness checks are unavailable (agent mode only)
    #[arg(long)]
    force: bool,
    /// Enable kill semantics for active agents (agent mode only)
    #[arg(long)]
    kill: bool,

    /// Wait timeout in seconds for graceful shutdown (agent mode only)
    #[arg(long, default_value_t = 10)]
    timeout: u64,
}

/// Execute the cleanup command
pub fn execute(args: CleanupArgs) -> Result<()> {
    let home_dir = get_home_dir()?;
    let current_dir = std::env::current_dir()?;

    let overrides = ConfigOverrides {
        team: args.team.clone(),
        ..Default::default()
    };

    let config = resolve_config(&overrides, &current_dir, &home_dir)?;

    // Agent cleanup compatibility mode:
    // `atm cleanup --agent <name> [--team <team>] [--force]`
    if let Some(agent) = &args.agent {
        if args.all_teams {
            anyhow::bail!("--agent cannot be combined with --all-teams");
        }
        let team_name = args
            .team
            .clone()
            .unwrap_or_else(|| config.core.default_team.clone());
        if args.dry_run {
            return teams::cleanup_single_agent_dry_run(team_name, agent.clone(), args.force);
        }
        return execute_agent_cleanup(
            &home_dir,
            &team_name,
            agent,
            args.force,
            args.kill,
            args.timeout.max(1),
        );
    }

    let teams_dir = teams_root_dir_for(&home_dir);
    if !teams_dir.exists() {
        let display = teams_dir.display();
        anyhow::bail!("Teams directory not found at {display}");
    }

    // Check if retention policy is configured
    if config.retention.max_age.is_none() && config.retention.max_count.is_none() {
        println!(
            "No retention policy configured. Set retention.max_age and/or retention.max_count in .atm.toml"
        );
        return Ok(());
    }

    if args.dry_run {
        println!("DRY RUN - no files will be modified\n");
    }

    if args.all_teams {
        // Apply to all teams
        let entries = std::fs::read_dir(&teams_dir)?;
        let mut team_names: Vec<String> = Vec::new();

        for entry in entries {
            let entry = entry?;
            let path = entry.path();
            if path.is_dir()
                && let Some(name_str) = path.file_name().and_then(|n| n.to_str())
            {
                team_names.push(name_str.to_string());
            }
        }

        team_names.sort();

        for team_name in team_names {
            cleanup_team(&home_dir, &team_name, &config.retention, args.dry_run)?;
        }
    } else {
        // Apply to single team
        let team_name = &config.core.default_team;
        cleanup_team(&home_dir, team_name, &config.retention, args.dry_run)?;
    }

    Ok(())
}

fn execute_agent_cleanup(
    home_dir: &Path,
    team_name: &str,
    agent_name: &str,
    force: bool,
    kill_mode: bool,
    timeout_secs: u64,
) -> Result<()> {
    if agent_name == "team-lead" {
        anyhow::bail!("team-lead is protected and cannot be removed by cleanup");
    }

    let daemon_available = if daemon_is_running() {
        true
    } else {
        ensure_daemon_running().is_ok() && daemon_is_running()
    };

    if daemon_available {
        let session = query_session_for_team(team_name, agent_name);
        match session {
            Ok(Some(info)) if info.alive => {
                let pid = validated_signal_pid(info.process_id).ok_or_else(|| {
                    anyhow::anyhow!(
                        "refusing to signal invalid pid {} for {}@{}",
                        info.process_id,
                        agent_name,
                        team_name
                    )
                })?;
                if !kill_mode {
                    anyhow::bail!(
                        "agent '{}' is active; refusing destructive cleanup without --kill",
                        agent_name
                    );
                }
                send_shutdown_request(home_dir, team_name, agent_name)?;

                if !wait_for_session_dead(team_name, agent_name, timeout_secs) {
                    #[cfg(unix)]
                    {
                        // SAFETY: SIGINT requests graceful interrupt of the target process.
                        let _ = unsafe { libc::kill(pid, libc::SIGINT) };
                    }
                    #[cfg(not(unix))]
                    {
                        anyhow::bail!(
                            "forced process termination is not supported on this platform"
                        );
                    }
                    if !wait_for_session_dead(team_name, agent_name, 10) {
                        #[cfg(unix)]
                        {
                            // SAFETY: SIGTERM requests cooperative shutdown after SIGINT timeout.
                            let _ = unsafe { libc::kill(pid, libc::SIGTERM) };
                        }
                        if !wait_for_session_dead(team_name, agent_name, 10) {
                            #[cfg(unix)]
                            {
                                // SAFETY: SIGKILL force-terminates process after graceful attempts.
                                let _ = unsafe { libc::kill(pid, libc::SIGKILL) };
                            }
                            if !wait_for_session_dead(team_name, agent_name, 3) {
                                anyhow::bail!(
                                    "failed to terminate '{}' within timeout; cleanup aborted",
                                    agent_name
                                );
                            }
                        }
                    }
                }
            }
            Ok(Some(_)) => {}
            Ok(None) => {
                if !force {
                    anyhow::bail!(
                        "daemon is running but has no session record for '{}'; refusing cleanup without --force",
                        agent_name
                    );
                }
            }
            Err(e) => {
                if !force {
                    anyhow::bail!(
                        "failed to query daemon session for '{}': {e}. Re-run with --force to override",
                        agent_name
                    );
                }
            }
        }
    } else if !force {
        anyhow::bail!(
            "daemon is not running; cannot safely confirm liveness for '{}'. Start daemon or re-run with --force",
            agent_name
        );
    }

    teams::cleanup_single_agent(team_name.to_string(), agent_name.to_string(), force)
}

fn daemon_is_running() -> bool {
    #[cfg(test)]
    if let Some(state) = test_daemon_state::snapshot() {
        return state.running;
    }
    agent_team_mail_core::daemon_client::daemon_is_running()
}

fn query_session_for_team(
    team_name: &str,
    agent_name: &str,
) -> Result<Option<agent_team_mail_core::daemon_client::SessionQueryResult>> {
    #[cfg(test)]
    if let Some(state) = test_daemon_state::snapshot() {
        return match state.query_error {
            Some(msg) => Err(anyhow::Error::msg(msg)),
            None => Ok(state.query_result),
        };
    }
    agent_team_mail_core::daemon_client::query_session_for_team(team_name, agent_name)
}

fn ensure_daemon_running() -> Result<()> {
    #[cfg(test)]
    if let Some(state) = test_daemon_state::snapshot() {
        return if state.running {
            Ok(())
        } else {
            Err(anyhow::anyhow!("daemon is not running"))
        };
    }
    // Use query path as autostart trigger to stay compatible with published
    // atm-core APIs available to packaged CLI builds.
    let _ = agent_team_mail_core::daemon_client::query_list_agents();
    if agent_team_mail_core::daemon_client::daemon_is_running() {
        Ok(())
    } else {
        Err(anyhow::anyhow!("daemon is not running"))
    }
}

fn send_shutdown_request(home_dir: &Path, team_name: &str, agent_name: &str) -> Result<()> {
    let request_id = Uuid::new_v4().to_string();
    let shutdown_payload = serde_json::json!({
        "type": "shutdown_request",
        "requestId": request_id,
        "from": "atm",
        "reason": "cleanup --agent",
        "timestamp": Utc::now().to_rfc3339(),
    });

    let msg = InboxMessage {
        from: "atm".to_string(),
        text: shutdown_payload.to_string(),
        timestamp: Utc::now().to_rfc3339(),
        read: false,
        summary: Some("shutdown_request".to_string()),
        message_id: Some(Uuid::new_v4().to_string()),
        unknown_fields: HashMap::new(),
    };

    let inbox_path = teams_root_dir_for(home_dir)
        .join(team_name)
        .join("inboxes")
        .join(format!("{agent_name}.json"));
    inbox_append(&inbox_path, &msg, team_name, "atm")?;
    Ok(())
}

fn wait_for_session_dead(team_name: &str, agent_name: &str, timeout_secs: u64) -> bool {
    let deadline = Instant::now() + Duration::from_secs(timeout_secs);
    while Instant::now() < deadline {
        match query_session_for_team(team_name, agent_name) {
            Ok(Some(info)) if !info.alive => return true,
            Ok(None) => return true,
            Ok(Some(_)) => {}
            Err(_) => {}
        }
        std::thread::sleep(Duration::from_millis(500));
    }
    false
}

fn validated_signal_pid(pid: u32) -> Option<i32> {
    if pid > 1 && pid <= i32::MAX as u32 {
        Some(pid as i32)
    } else {
        None
    }
}

#[cfg(test)]
mod test_daemon_state {
    use std::sync::{Mutex, OnceLock};

    #[derive(Clone)]
    pub(super) struct State {
        pub(super) running: bool,
        pub(super) query_result: Option<agent_team_mail_core::daemon_client::SessionQueryResult>,
        pub(super) query_error: Option<String>,
    }

    fn slot() -> &'static Mutex<Option<State>> {
        static SLOT: OnceLock<Mutex<Option<State>>> = OnceLock::new();
        SLOT.get_or_init(|| Mutex::new(None))
    }

    pub(super) fn set(state: State) {
        *slot().lock().unwrap() = Some(state);
    }

    pub(super) fn clear() {
        *slot().lock().unwrap() = None;
    }

    pub(super) fn snapshot() -> Option<State> {
        slot().lock().unwrap().clone()
    }
}
/// Clean up a single team's inboxes
fn cleanup_team(
    home_dir: &Path,
    team_name: &str,
    retention_config: &agent_team_mail_core::config::RetentionConfig,
    dry_run: bool,
) -> Result<()> {
    let team_dir = teams_root_dir_for(home_dir).join(team_name);

    if !team_dir.exists() {
        println!("Team '{team_name}' not found, skipping");
        return Ok(());
    }

    // Load team config to get member list
    let team_config_path = team_dir.join("config.json");
    if !team_config_path.exists() {
        println!("Team '{team_name}' has no config.json, skipping");
        return Ok(());
    }

    let team_config: TeamConfig =
        serde_json::from_str(&std::fs::read_to_string(&team_config_path)?)
            .with_context(|| format!("Failed to parse team config for '{team_name}'"))?;

    println!("Team: {team_name}\n");
    println!(
        "  {:<20} {:>8} {:>8} {:>10}",
        "Agent", "Kept", "Removed", "Archived"
    );
    println!("  {}", "─".repeat(50));

    let mut total_kept = 0;
    let mut total_removed = 0;
    let mut total_archived = 0;

    // Apply retention to each agent's inbox (local files)
    for member in &team_config.members {
        let inbox_path = team_dir
            .join("inboxes")
            .join(format!("{}.json", member.name));

        if !inbox_path.exists() {
            // Skip agents with no inbox file
            continue;
        }

        let result = apply_retention(
            &inbox_path,
            team_name,
            &member.name,
            retention_config,
            dry_run,
        )?;

        // Only show agents where something happened
        if result.removed > 0 || result.kept > 0 {
            println!(
                "  {:<20} {:>8} {:>8} {:>10}",
                member.name, result.kept, result.removed, result.archived
            );

            total_kept += result.kept;
            total_removed += result.removed;
            total_archived += result.archived;
        }
    }

    // Also clean up per-origin inbox files (format: agent.hostname.json)
    let inboxes_dir = team_dir.join("inboxes");
    if inboxes_dir.exists()
        && let Ok(entries) = std::fs::read_dir(&inboxes_dir)
    {
        for entry in entries.flatten() {
            let path = entry.path();
            if !path.is_file() {
                continue;
            }

            let Some(filename) = path.file_name().and_then(|n| n.to_str()) else {
                continue;
            };

            // Per-origin files have format: agent.hostname.json
            // Skip if it's a local file (no dots in stem)
            if !filename.ends_with(".json") {
                continue;
            }

            let stem = filename.strip_suffix(".json").unwrap();
            if !stem.contains('.') {
                // This is a local file, already processed above
                continue;
            }

            // Extract agent name and hostname
            // Format: agent-name.hostname.json
            let parts: Vec<_> = stem.rsplitn(2, '.').collect();
            if parts.len() != 2 {
                continue;
            }

            let hostname = parts[0];
            let agent_name = parts[1];
            let display_name = format!("{agent_name}@{hostname}");

            let result =
                apply_retention(&path, team_name, &display_name, retention_config, dry_run)?;

            if result.removed > 0 || result.kept > 0 {
                println!(
                    "  {:<20} {:>8} {:>8} {:>10}",
                    display_name, result.kept, result.removed, result.archived
                );

                total_kept += result.kept;
                total_removed += result.removed;
                total_archived += result.archived;
            }
        }
    }

    if total_kept == 0 && total_removed == 0 {
        println!("  (no messages in any inbox)");
    } else {
        println!("  {}", "─".repeat(50));
        println!(
            "  {:<20} {:>8} {:>8} {:>10}",
            "TOTAL", total_kept, total_removed, total_archived
        );
    }

    println!();

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serial_test::serial;
    use tempfile::TempDir;

    fn set_autostart_disabled_for_test() -> Option<String> {
        let original = std::env::var("ATM_DAEMON_AUTOSTART").ok();
        // SAFETY: test-only env mutation, callers use #[serial].
        unsafe {
            std::env::set_var("ATM_DAEMON_AUTOSTART", "0");
        }
        original
    }

    fn restore_autostart_env(original: Option<String>) {
        // SAFETY: test-only env mutation, callers use #[serial].
        unsafe {
            match original {
                Some(v) => std::env::set_var("ATM_DAEMON_AUTOSTART", v),
                None => std::env::remove_var("ATM_DAEMON_AUTOSTART"),
            }
        }
    }

    fn create_test_team(temp_dir: &TempDir, team_name: &str) -> std::path::PathBuf {
        let team_dir = temp_dir.path().join(".claude/teams").join(team_name);
        let inboxes_dir = team_dir.join("inboxes");
        std::fs::create_dir_all(&inboxes_dir).unwrap();

        let config = serde_json::json!({
            "name": team_name,
            "createdAt": 1739284800000i64,
            "leadAgentId": format!("team-lead@{team_name}"),
            "leadSessionId": "old-session-id-1234",
            "members": [
                {
                    "agentId": format!("team-lead@{team_name}"),
                    "name": "team-lead",
                    "agentType": "general-purpose",
                    "model": "claude-opus-4-6",
                    "joinedAt": 1739284800000i64,
                    "tmuxPaneId": "",
                    "cwd": temp_dir.path().to_str().unwrap(),
                    "subscriptions": []
                },
                {
                    "agentId": format!("publisher@{team_name}"),
                    "name": "publisher",
                    "agentType": "general-purpose",
                    "model": "claude-haiku-4-5",
                    "joinedAt": 1739284800000i64,
                    "tmuxPaneId": "%1",
                    "cwd": temp_dir.path().to_str().unwrap(),
                    "subscriptions": [],
                    "isActive": true
                }
            ]
        });

        let config_path = team_dir.join("config.json");
        std::fs::write(&config_path, serde_json::to_string_pretty(&config).unwrap()).unwrap();
        team_dir
    }

    #[test]
    #[serial]
    fn test_execute_agent_cleanup_refuses_active_without_kill() {
        let temp_dir = TempDir::new().unwrap();
        let team_dir = create_test_team(&temp_dir, "atm-dev");
        let inbox = team_dir.join("inboxes/publisher.json");
        std::fs::write(&inbox, "[]").unwrap();

        test_daemon_state::set(test_daemon_state::State {
            running: true,
            query_result: Some(agent_team_mail_core::daemon_client::SessionQueryResult {
                session_id: "sess-1".to_string(),
                process_id: 4242,
                alive: true,
                last_seen_at: None,
                runtime: Some("codex".to_string()),
                runtime_session_id: None,
                pane_id: None,
                runtime_home: None,
            }),
            query_error: None,
        });

        let result =
            execute_agent_cleanup(temp_dir.path(), "atm-dev", "publisher", false, false, 1);
        test_daemon_state::clear();

        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(
            err.contains("refusing destructive cleanup without --kill"),
            "unexpected error: {err}"
        );
    }

    #[test]
    #[serial]
    fn test_execute_agent_cleanup_refuses_without_daemon_or_force() {
        let temp_dir = TempDir::new().unwrap();
        let team_dir = create_test_team(&temp_dir, "atm-dev");
        let inbox = team_dir.join("inboxes/publisher.json");
        std::fs::write(&inbox, "[]").unwrap();

        let original_autostart = set_autostart_disabled_for_test();
        test_daemon_state::set(test_daemon_state::State {
            running: false,
            query_result: None,
            query_error: None,
        });
        let result =
            execute_agent_cleanup(temp_dir.path(), "atm-dev", "publisher", false, false, 1);
        test_daemon_state::clear();
        restore_autostart_env(original_autostart);

        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(
            err.contains("daemon is not running"),
            "unexpected error: {err}"
        );
    }

    #[test]
    #[serial]
    fn test_execute_agent_cleanup_force_removes_roster_and_mailbox() {
        let temp_dir = TempDir::new().unwrap();
        let team_dir = create_test_team(&temp_dir, "atm-dev");
        let inbox = team_dir.join("inboxes/publisher.json");
        std::fs::write(&inbox, "[]").unwrap();
        let home_env = temp_dir.path().to_string_lossy().to_string();
        let original_home = std::env::var("ATM_HOME").ok();
        let original_autostart = set_autostart_disabled_for_test();
        // SAFETY: test-only env mutation; serialized via #[serial].
        unsafe {
            std::env::set_var("ATM_HOME", &home_env);
        }

        test_daemon_state::set(test_daemon_state::State {
            running: false,
            query_result: None,
            query_error: None,
        });
        let result = execute_agent_cleanup(temp_dir.path(), "atm-dev", "publisher", true, false, 1);
        test_daemon_state::clear();
        // SAFETY: test-only cleanup.
        unsafe {
            match original_home {
                Some(v) => std::env::set_var("ATM_HOME", v),
                None => std::env::remove_var("ATM_HOME"),
            }
        }
        restore_autostart_env(original_autostart);
        assert!(result.is_ok(), "cleanup should succeed: {result:?}");

        assert!(
            !inbox.exists(),
            "mailbox should be removed during coupled teardown"
        );
        let config_path = team_dir.join("config.json");
        let cfg: TeamConfig =
            serde_json::from_str(&std::fs::read_to_string(config_path).unwrap()).unwrap();
        assert!(
            !cfg.members.iter().any(|m| m.name == "publisher"),
            "roster member should be removed together with mailbox"
        );
    }
}
