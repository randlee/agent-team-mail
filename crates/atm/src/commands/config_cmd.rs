//! Config command implementation

use anyhow::Result;
use atm_core::config::{resolve_config, ConfigOverrides};
use clap::Args;
use serde_json::json;

use crate::util::settings::get_home_dir;

/// Show effective configuration
#[derive(Args, Debug)]
pub struct ConfigArgs {
    /// Output as JSON
    #[arg(long)]
    json: bool,
}

/// Execute the config command
pub fn execute(args: ConfigArgs) -> Result<()> {
    let home_dir = get_home_dir()?;
    let current_dir = std::env::current_dir()?;

    // Resolve configuration
    let overrides = ConfigOverrides::default();
    let config = resolve_config(&overrides, &current_dir, &home_dir)?;

    // Check config file paths
    let global_config_path = home_dir.join(".config/atm/config.toml");
    let repo_config_path = current_dir.join(".atm.toml");

    let global_exists = global_config_path.exists();
    let repo_exists = repo_config_path.exists();

    // Determine source for each config value
    // For now, we'll use heuristics based on whether files exist
    let default_team_source = if repo_exists {
        "repo_config"
    } else if global_exists {
        "global_config"
    } else {
        "default"
    };

    let identity_source = if repo_exists {
        "repo_config"
    } else if global_exists {
        "global_config"
    } else {
        "default"
    };

    // Output results
    if args.json {
        let output = json!({
            "defaultTeam": {
                "value": config.core.default_team,
                "source": default_team_source,
            },
            "identity": {
                "value": config.core.identity,
                "source": identity_source,
            },
            "claudeRoot": format!("{}/.claude/", home_dir.display()),
            "configFiles": {
                "global": {
                    "path": global_config_path.display().to_string(),
                    "exists": global_exists,
                },
                "repo": {
                    "path": repo_config_path.display().to_string(),
                    "exists": repo_exists,
                }
            }
        });
        println!("{}", serde_json::to_string_pretty(&output)?);
    } else {
        let default_team = &config.core.default_team;
        let default_team_src = format_source(default_team_source);
        let identity = &config.core.identity;
        let identity_src = format_source(identity_source);
        let home_display = home_dir.display();
        println!("Configuration:");
        println!("  default_team: {default_team} (from {default_team_src})");
        println!("  identity: {identity} (from {identity_src})");
        println!("  claude_root: {home_display}/.claude/");
        println!();
        println!("Config files:");
        let global_display = global_config_path.display();
        let global_status = if global_exists { "(found)" } else { "(not found)" };
        println!("  Global: {global_display} {global_status}");
        let repo_display = repo_config_path.display();
        let repo_status = if repo_exists { "(found)" } else { "(not found)" };
        println!("  Repo: {repo_display} {repo_status}");
    }

    Ok(())
}

/// Format source name for display
fn format_source(source: &str) -> String {
    match source {
        "repo_config" => "repo .atm.toml".to_string(),
        "global_config" => "~/.config/atm/config.toml".to_string(),
        "default" => "default".to_string(),
        _ => source.to_string(),
    }
}
