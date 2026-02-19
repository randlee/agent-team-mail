//! `summary` subcommand — display saved session summary (FR-6, FR-7).
//!
//! Loads the persisted registry to find the session entry for the given
//! `agent_id`, then reads the summary file from disk and prints it.

use crate::cli::SummaryArgs;

/// Run the `summary` subcommand.
///
/// Loads the registry, looks up the agent_id, reads the summary file, and
/// prints the content (or a "no summary available" message).
///
/// # Errors
///
/// Returns an error if the registry cannot be read or parsed. Missing
/// summaries are reported as informational messages, not errors.
pub async fn run(args: SummaryArgs) -> anyhow::Result<()> {
    let sessions_dir = crate::lock::sessions_dir();

    // Scan all team directories to find the agent_id.
    let mut found_identity: Option<String> = None;
    let mut found_backend_id: Option<String> = None;
    let mut found_team: Option<String> = None;

    let entries = match std::fs::read_dir(&sessions_dir) {
        Ok(e) => e,
        Err(_) => {
            println!(
                "No sessions directory found. No summary available for agent {}.",
                args.agent_id
            );
            return Ok(());
        }
    };

    for dir_entry in entries.flatten() {
        if !dir_entry.file_type().map(|ft| ft.is_dir()).unwrap_or(false) {
            continue;
        }
        let team_name = dir_entry.file_name().to_string_lossy().to_string();
        let registry_path = dir_entry.path().join("registry.json");
        let Ok(contents) = std::fs::read_to_string(&registry_path) else {
            continue;
        };
        let Ok(root) = serde_json::from_str::<serde_json::Value>(&contents) else {
            continue;
        };
        let Some(sessions) = root.get("sessions").and_then(|v| v.as_array()) else {
            continue;
        };

        for session in sessions {
            if session.get("agent_id").and_then(|v| v.as_str()) == Some(&args.agent_id) {
                found_identity = session
                    .get("identity")
                    .and_then(|v| v.as_str())
                    .map(String::from);
                found_backend_id = session
                    .get("thread_id")
                    .and_then(|v| v.as_str())
                    .map(String::from);
                found_team = Some(team_name.clone());
                break;
            }
        }
        if found_team.is_some() {
            break;
        }
    }

    let (Some(team), Some(identity), Some(backend_id)) =
        (found_team, found_identity, found_backend_id)
    else {
        println!(
            "No session found for agent {}. No summary available.",
            args.agent_id
        );
        return Ok(());
    };

    match crate::summary::read_summary(&team, &identity, &backend_id).await {
        Some(content) => {
            println!("{content}");
        }
        None => {
            println!(
                "No summary available for agent {} (identity: {}, thread: {}).",
                args.agent_id, identity, backend_id
            );
        }
    }

    Ok(())
}
