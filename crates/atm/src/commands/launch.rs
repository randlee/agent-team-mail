//! `atm launch` — launch a new Codex agent via the daemon.
//!
//! The command connects to the running daemon over the Unix socket and
//! requests that a new agent be spawned in a tmux pane.  The daemon
//! handles all tmux interaction; this command acts purely as a thin
//! client.
//!
//! # Examples
//!
//! ```text
//! # Launch arch-ctm with the default command
//! atm launch arch-ctm --team atm-dev
//!
//! # Launch with a custom command and initial prompt
//! atm launch arch-ctm \
//!     --command "codex --yolo" \
//!     --prompt "Review the bridge module and report findings" \
//!     --team atm-dev
//!
//! # Launch with extra environment variables and JSON output
//! atm launch arch-ctm --env MY_VAR=hello --json
//! ```

use anyhow::Result;
use clap::Args;
use std::collections::HashMap;

use agent_team_mail_core::config::{resolve_config, ConfigOverrides};
use agent_team_mail_core::daemon_client::{LaunchConfig, LaunchResult};

use crate::util::settings::get_home_dir;

/// Launch a new Codex agent via the daemon
#[derive(Args, Debug)]
pub struct LaunchArgs {
    /// Agent identity name (e.g., "arch-ctm")
    pub agent_name: String,

    /// Command to run in the tmux pane (default: "codex --yolo")
    #[arg(long, default_value = "codex --yolo")]
    pub command: String,

    /// Initial prompt to send after the agent becomes ready
    #[arg(long)]
    pub prompt: Option<String>,

    /// Override the team name (default: from config)
    #[arg(long)]
    pub team: Option<String>,

    /// Readiness timeout in seconds (default: 30)
    #[arg(long, default_value_t = 30)]
    pub timeout: u32,

    /// Output result as JSON
    #[arg(long)]
    pub json: bool,

    /// Extra environment variables for the pane (KEY=VALUE, repeatable)
    ///
    /// `ATM_IDENTITY` and `ATM_TEAM` are set automatically; you do not need
    /// to include them here.
    #[arg(long = "env", value_name = "KEY=VALUE")]
    pub env: Vec<String>,
}

/// Execute the `atm launch` command.
pub fn execute(args: LaunchArgs) -> Result<()> {
    // Platform guard — launch requires Unix (tmux + Unix socket)
    #[cfg(not(unix))]
    {
        let _ = args;
        eprintln!(
            "Error: Agent launch requires Unix platform (tmux + Unix socket)"
        );
        std::process::exit(1);
    }

    #[cfg(unix)]
    execute_unix(args)
}

#[cfg(unix)]
fn execute_unix(args: LaunchArgs) -> Result<()> {
    use agent_team_mail_core::daemon_client::launch_agent;

    // Resolve configuration for team name and identity
    let home_dir = get_home_dir()?;
    let current_dir = std::env::current_dir()?;

    let overrides = ConfigOverrides {
        team: args.team.clone(),
        ..Default::default()
    };

    let config = resolve_config(&overrides, &current_dir, &home_dir)?;

    let team_name = args
        .team
        .clone()
        .unwrap_or_else(|| config.core.default_team.clone());

    // Parse --env KEY=VALUE pairs
    let env_vars = parse_env_vars(&args.env)?;

    let launch_config = LaunchConfig {
        agent: args.agent_name.clone(),
        team: team_name.clone(),
        command: args.command.clone(),
        prompt: args.prompt.clone(),
        timeout_secs: args.timeout,
        env_vars,
    };

    // Send launch request to daemon
    let result = match launch_agent(&launch_config) {
        Ok(Some(r)) => r,
        Ok(None) => {
            if args.json {
                println!(
                    "{{\"error\": \"Daemon is not running. Start it with: atm-daemon\"}}"
                );
            } else {
                eprintln!("Error: Daemon is not running. Start it with: atm-daemon");
            }
            std::process::exit(1);
        }
        Err(e) => {
            if args.json {
                println!("{{\"error\": \"{}\"}}", e.to_string().replace('"', "\\\""));
            } else {
                eprintln!("Error: {e}");
            }
            std::process::exit(1);
        }
    };

    print_result(&result, args.json);
    Ok(())
}

/// Print the launch result in human-readable or JSON format.
fn print_result(result: &LaunchResult, json: bool) {
    if json {
        let output = serde_json::json!({
            "agent": result.agent,
            "pane_id": result.pane_id,
            "state": result.state,
            "warning": result.warning,
        });
        println!(
            "{}",
            serde_json::to_string_pretty(&output).unwrap_or_default()
        );
    } else {
        println!("Launched agent: {}", result.agent);
        println!("  pane:  {}", result.pane_id);
        println!("  state: {}", result.state);
        if let Some(ref warning) = result.warning {
            eprintln!("Warning: {warning}");
        }
    }
}

/// Parse a list of `"KEY=VALUE"` strings into a [`HashMap`].
///
/// Returns an error if any entry does not contain `=` or has an empty key.
fn parse_env_vars(pairs: &[String]) -> Result<HashMap<String, String>> {
    let mut map = HashMap::new();
    for pair in pairs {
        let eq_pos = pair.find('=').ok_or_else(|| {
            anyhow::anyhow!(
                "Invalid --env value '{pair}': expected KEY=VALUE format"
            )
        })?;
        let key = &pair[..eq_pos];
        let value = &pair[eq_pos + 1..];
        if key.is_empty() {
            anyhow::bail!("Invalid --env value '{pair}': key must not be empty");
        }
        map.insert(key.to_string(), value.to_string());
    }
    Ok(map)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_env_vars_empty() {
        let result = parse_env_vars(&[]).unwrap();
        assert!(result.is_empty());
    }

    #[test]
    fn test_parse_env_vars_single() {
        let result = parse_env_vars(&["MY_VAR=hello".to_string()]).unwrap();
        assert_eq!(result.get("MY_VAR").map(String::as_str), Some("hello"));
    }

    #[test]
    fn test_parse_env_vars_multiple() {
        let pairs = vec![
            "FOO=bar".to_string(),
            "BAZ=qux".to_string(),
        ];
        let result = parse_env_vars(&pairs).unwrap();
        assert_eq!(result.get("FOO").map(String::as_str), Some("bar"));
        assert_eq!(result.get("BAZ").map(String::as_str), Some("qux"));
    }

    #[test]
    fn test_parse_env_vars_value_with_equals() {
        // Value may contain '=' — only the first one splits key/value.
        let result = parse_env_vars(&["URL=http://example.com?a=1&b=2".to_string()]).unwrap();
        assert_eq!(
            result.get("URL").map(String::as_str),
            Some("http://example.com?a=1&b=2")
        );
    }

    #[test]
    fn test_parse_env_vars_empty_value() {
        let result = parse_env_vars(&["EMPTY=".to_string()]).unwrap();
        assert_eq!(result.get("EMPTY").map(String::as_str), Some(""));
    }

    #[test]
    fn test_parse_env_vars_missing_equals() {
        let result = parse_env_vars(&["NOEQUALSSIGN".to_string()]);
        assert!(result.is_err());
    }

    #[test]
    fn test_parse_env_vars_empty_key() {
        let result = parse_env_vars(&["=value".to_string()]);
        assert!(result.is_err());
    }

    #[test]
    fn test_launch_config_builds_correctly() {
        let config = LaunchConfig {
            agent: "arch-ctm".to_string(),
            team: "atm-dev".to_string(),
            command: "codex --yolo".to_string(),
            prompt: Some("Review the bridge".to_string()),
            timeout_secs: 30,
            env_vars: HashMap::new(),
        };

        assert_eq!(config.agent, "arch-ctm");
        assert_eq!(config.team, "atm-dev");
        assert_eq!(config.command, "codex --yolo");
        assert_eq!(config.prompt.as_deref(), Some("Review the bridge"));
        assert_eq!(config.timeout_secs, 30);
    }

    #[test]
    fn test_launch_result_with_warning() {
        let result = LaunchResult {
            agent: "arch-ctm".to_string(),
            pane_id: "%42".to_string(),
            state: "launching".to_string(),
            warning: Some("Timeout reached".to_string()),
        };

        // Human-readable output should not panic
        print_result(&result, false);
    }

    #[test]
    fn test_launch_result_json_output() {
        let result = LaunchResult {
            agent: "worker-1".to_string(),
            pane_id: "%7".to_string(),
            state: "idle".to_string(),
            warning: None,
        };

        // JSON output: capture via to_value
        let json = serde_json::json!({
            "agent": result.agent,
            "pane_id": result.pane_id,
            "state": result.state,
            "warning": result.warning,
        });

        assert_eq!(json["agent"].as_str().unwrap(), "worker-1");
        assert_eq!(json["pane_id"].as_str().unwrap(), "%7");
        assert_eq!(json["state"].as_str().unwrap(), "idle");
        assert!(json["warning"].is_null());
    }
}
