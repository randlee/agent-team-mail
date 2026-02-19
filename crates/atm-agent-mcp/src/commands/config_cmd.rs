//! `config` subcommand — show resolved configuration.
//!
//! Loads the full resolved configuration and prints it either as JSON
//! (`--json`) or as a human-readable key=value table.

use crate::cli::ConfigArgs;
use crate::config::{resolve_config, AgentMcpConfig, ResolvedConfig, RolePreset};
use std::path::PathBuf;

/// Run the `config` subcommand.
///
/// # Errors
///
/// Returns an error if config resolution fails (e.g., unreadable TOML file or
/// home directory cannot be determined).
pub async fn run(config_path: &Option<PathBuf>, args: ConfigArgs) -> anyhow::Result<()> {
    let resolved: ResolvedConfig = resolve_config(config_path.as_deref())?;
    let cfg: &AgentMcpConfig = &resolved.agent_mcp;

    if args.json {
        let json = serde_json::to_string_pretty(cfg)?;
        println!("{json}");
    } else {
        println!("atm-agent-mcp configuration:");
        println!("  codex_bin              = {}", cfg.codex_bin);
        println!(
            "  identity               = {}",
            cfg.identity.as_deref().unwrap_or("<unset>")
        );
        println!(
            "  model                  = {}",
            cfg.model.as_deref().unwrap_or("<unset>")
        );
        println!(
            "  fast_model             = {}",
            cfg.fast_model.as_deref().unwrap_or("<unset>")
        );
        println!(
            "  reasoning_effort       = {}",
            cfg.reasoning_effort.as_deref().unwrap_or("<unset>")
        );
        println!("  sandbox                = {}", cfg.sandbox);
        println!("  approval_policy        = {}", cfg.approval_policy);
        println!("  mail_poll_interval_ms  = {}", cfg.mail_poll_interval_ms);
        println!("  request_timeout_secs   = {}", cfg.request_timeout_secs);
        println!("  max_concurrent_threads = {}", cfg.max_concurrent_threads);
        println!("  persist_threads        = {}", cfg.persist_threads);
        println!("  auto_mail              = {}", cfg.auto_mail);
        println!(
            "  base_prompt_file       = {}",
            cfg.base_prompt_file.as_deref().unwrap_or("<unset>")
        );
        println!(
            "  extra_instructions_file= {}",
            cfg.extra_instructions_file.as_deref().unwrap_or("<unset>")
        );

        if cfg.roles.is_empty() {
            println!("  roles                  = (none)");
        } else {
            println!("  roles:");
            let mut role_names: Vec<&String> = cfg.roles.keys().collect();
            role_names.sort();
            for name in role_names {
                let preset: &RolePreset = &cfg.roles[name];
                println!("    [{name}]");
                if let Some(ref m) = preset.model {
                    println!("      model            = {m}");
                }
                if let Some(ref s) = preset.sandbox {
                    println!("      sandbox          = {s}");
                }
                if let Some(ref a) = preset.approval_policy {
                    println!("      approval_policy  = {a}");
                }
                if let Some(ref r) = preset.reasoning_effort {
                    println!("      reasoning_effort = {r}");
                }
            }
        }

        // Also show ATM core config context
        println!();
        println!("atm core configuration:");
        println!("  identity    = {}", resolved.core.identity);
        println!("  team        = {}", resolved.core.default_team);
    }

    Ok(())
}
