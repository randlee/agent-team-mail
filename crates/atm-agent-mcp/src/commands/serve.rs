//! `serve` subcommand — start the MCP proxy server.
//!
//! Reads MCP JSON-RPC messages from stdin, proxies to a lazily spawned `codex
//! mcp-server` child process, and writes responses to stdout. See [`crate::proxy`]
//! for the core proxy logic and [`crate::framing`] for framing details.

use crate::cli::ServeArgs;
use crate::config::resolve_config;
use crate::proxy::{ProxyServer, ResumeContext};
use std::path::PathBuf;

/// Run the `serve` subcommand.
///
/// Resolves configuration, applies CLI overrides, then enters the proxy loop
/// which reads from stdin and writes to stdout until EOF.
///
/// # Errors
///
/// Returns an error if configuration resolution fails or the proxy loop
/// encounters an unrecoverable I/O error.
pub async fn run(config_path: &Option<PathBuf>, args: ServeArgs) -> anyhow::Result<()> {
    // Resolve configuration from file/env/defaults
    let resolved = resolve_config(config_path.as_deref())?;
    let mut config = resolved.agent_mcp;

    // Apply CLI argument overrides
    if let Some(ref identity) = args.identity {
        config.identity = Some(identity.clone());
    }
    if let Some(ref model) = args.model {
        config.model = Some(model.clone());
    }
    if args.fast {
        if let Some(ref fast_model) = config.fast_model.clone() {
            config.model = Some(fast_model.clone());
        }
    }
    if let Some(ref sandbox) = args.sandbox {
        config.sandbox = sandbox.clone();
    }
    if let Some(ref policy) = args.approval_policy {
        config.approval_policy = policy.clone();
    }
    if let Some(timeout_secs) = args.timeout {
        config.request_timeout_secs = timeout_secs;
    }

    // Set up upstream I/O (stdin for reading, stdout for writing)
    let upstream_in = tokio::io::stdin();
    let upstream_out = tokio::io::stdout();

    // Use the ATM core team name for session registration and lock files.
    let team = resolved.core.default_team.clone();

    // FR-6: Determine resume context from --resume flag.
    let resume_context = if let Some(ref resume_arg) = args.resume {
        let registry_path = crate::lock::sessions_dir()
            .join(&team)
            .join("registry.json");
        load_resume_context(&registry_path, resume_arg.clone(), &team).await?
    } else {
        None
    };

    let mut proxy = ProxyServer::new_with_resume(config, team, resume_context);
    proxy.run(upstream_in, upstream_out).await
}

/// Load resume context from the persisted registry (FR-6.1, FR-6.2).
///
/// If `resume_arg` is `Some(agent_id)`, looks up that specific session.
/// If `resume_arg` is `None`, finds the most recent session by `last_active`.
///
/// Returns `None` if no matching session is found (not an error).
async fn load_resume_context(
    registry_path: &std::path::Path,
    resume_arg: Option<String>,
    team: &str,
) -> anyhow::Result<Option<ResumeContext>> {
    // Read and parse the persisted registry.
    let contents = match tokio::fs::read_to_string(registry_path).await {
        Ok(c) => c,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            tracing::info!("no persisted registry found; cannot resume");
            return Ok(None);
        }
        Err(e) => {
            tracing::warn!("failed to read registry for resume: {e}");
            return Ok(None);
        }
    };

    let root: serde_json::Value = match serde_json::from_str(&contents) {
        Ok(v) => v,
        Err(e) => {
            tracing::warn!("failed to parse registry for resume: {e}");
            return Ok(None);
        }
    };

    let sessions = match root.get("sessions").and_then(|v| v.as_array()) {
        Some(s) => s,
        None => {
            tracing::info!("registry has no sessions array; cannot resume");
            return Ok(None);
        }
    };

    // Find the matching session entry.
    let entry = if let Some(ref target_id) = resume_arg {
        // FR-6.2: specific agent_id
        sessions
            .iter()
            .find(|s| s.get("agent_id").and_then(|v| v.as_str()) == Some(target_id.as_str()))
    } else {
        // FR-6.1: most recent by last_active
        sessions.iter().max_by(|a, b| {
            let a_ts = a
                .get("last_active")
                .and_then(|v| v.as_str())
                .unwrap_or("");
            let b_ts = b
                .get("last_active")
                .and_then(|v| v.as_str())
                .unwrap_or("");
            a_ts.cmp(b_ts)
        })
    };

    let Some(entry) = entry else {
        tracing::info!("no matching session found for resume");
        return Ok(None);
    };

    let agent_id = entry
        .get("agent_id")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    let identity = entry
        .get("identity")
        .and_then(|v| v.as_str())
        .unwrap_or("unknown")
        .to_string();
    let backend_id = match entry.get("thread_id").and_then(|v| v.as_str()) {
        Some(tid) => tid.to_string(),
        None => {
            tracing::info!(
                agent_id = %agent_id,
                "session has no thread_id; cannot resume with summary"
            );
            return Ok(Some(ResumeContext {
                agent_id,
                identity,
                backend_id: String::new(),
                summary: None,
            }));
        }
    };

    // FR-6.3: Load summary from disk (may not exist after crash/SIGKILL).
    let summary = crate::summary::read_summary(team, &identity, &backend_id).await;
    if summary.is_none() {
        tracing::warn!(
            agent_id = %agent_id,
            identity = %identity,
            "no summary file found for resume; continuing without context (FR-6.3)"
        );
    }

    Ok(Some(ResumeContext {
        agent_id,
        identity,
        backend_id,
        summary,
    }))
}
