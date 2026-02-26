//! MCP server setup and management commands

use anyhow::{Context, Result, bail};
use clap::{Args, Subcommand, ValueEnum};
use std::path::PathBuf;

/// MCP server setup and management
#[derive(Args, Debug)]
pub struct McpArgs {
    #[command(subcommand)]
    command: McpCommands,
}

#[derive(Subcommand, Debug)]
enum McpCommands {
    /// Install atm-agent-mcp as an MCP server for a supported client
    Install(InstallArgs),
    /// Remove atm-agent-mcp MCP server configuration
    Uninstall(UninstallArgs),
    /// Show current MCP server configuration status
    Status,
}

#[derive(Debug, Clone, ValueEnum)]
enum Client {
    /// Claude Code CLI
    Claude,
    /// OpenAI Codex CLI
    Codex,
    /// Google Gemini CLI
    Gemini,
}

#[derive(Debug, Clone, ValueEnum)]
enum Scope {
    /// User-level (applies to all projects)
    Global,
    /// Project-level (applies to current directory only)
    Local,
}

/// Install atm-agent-mcp as an MCP server
#[derive(Args, Debug)]
struct InstallArgs {
    /// Target client to configure
    client: Client,

    /// Installation scope
    #[arg(default_value = "global")]
    scope: Scope,

    /// Override the atm-agent-mcp binary path (default: auto-detect from PATH)
    #[arg(long)]
    binary: Option<PathBuf>,
}

/// Remove atm-agent-mcp MCP server configuration
#[derive(Args, Debug)]
struct UninstallArgs {
    /// Target client to unconfigure
    client: Client,

    /// Installation scope to remove from
    #[arg(default_value = "global")]
    scope: Scope,
}

pub fn execute(args: McpArgs) -> Result<()> {
    match args.command {
        McpCommands::Install(install_args) => execute_install(install_args),
        McpCommands::Uninstall(uninstall_args) => execute_uninstall(uninstall_args),
        McpCommands::Status => execute_status(),
    }
}

/// Find a binary in PATH using in-process resolution (no shell dependency).
fn find_in_path(binary_name: &str) -> Option<PathBuf> {
    let path_var = std::env::var_os("PATH")?;
    for dir in std::env::split_paths(&path_var) {
        let candidate = dir.join(binary_name);
        if candidate.is_file() {
            return Some(candidate);
        }
        // On Windows, also check common extensions
        #[cfg(windows)]
        for ext in &["exe", "cmd", "bat"] {
            let with_ext = dir.join(format!("{binary_name}.{ext}"));
            if with_ext.is_file() {
                return Some(with_ext);
            }
        }
    }
    None
}

fn find_mcp_binary(override_path: Option<PathBuf>) -> Result<String> {
    if let Some(path) = override_path {
        if !path.exists() {
            bail!("Specified binary not found: {}", path.display());
        }
        return Ok(path.to_string_lossy().to_string());
    }

    if let Some(path) = find_in_path("atm-agent-mcp") {
        return Ok(path.to_string_lossy().to_string());
    }

    bail!(
        "atm-agent-mcp not found in PATH.\n\
         Install it with:\n  \
         brew install randlee/tap/agent-team-mail\n  \
         cargo install agent-team-mail-mcp"
    );
}

fn home_dir() -> Result<PathBuf> {
    crate::util::settings::get_home_dir()
}

// --- Claude Code ---
// User/Global: ~/.claude.json  (mcpServers field, user scope)
// Project/Local: .mcp.json     (project scope, committed to repo)

fn claude_config_path(scope: &Scope) -> Result<PathBuf> {
    match scope {
        Scope::Global => Ok(home_dir()?.join(".claude.json")),
        Scope::Local => Ok(PathBuf::from(".mcp.json")),
    }
}

/// Check if atm is configured in a JSON config file (Claude or Gemini).
fn is_json_atm_configured(path: &PathBuf) -> bool {
    let content = match std::fs::read_to_string(path) {
        Ok(c) => c,
        Err(_) => return false,
    };
    serde_json::from_str::<serde_json::Value>(&content)
        .ok()
        .and_then(|config| config.get("mcpServers")?.get("atm").cloned())
        .is_some()
}

/// Check cross-scope: if local requested, check if global already has it.
fn check_cross_scope_skip(client_name: &str, scope: &Scope, global_path: &Result<PathBuf>) -> bool {
    if !matches!(scope, Scope::Local) {
        return false;
    }
    if let Ok(gpath) = global_path {
        let configured = if client_name == "Codex" {
            is_codex_atm_configured(gpath)
        } else {
            is_json_atm_configured(gpath)
        };
        if configured {
            println!(
                "Project scope install skipped. atm MCP already installed globally."
            );
            println!("  Global config: {}", gpath.display());
            return true;
        }
    }
    false
}

fn install_claude(binary: &str, scope: &Scope) -> Result<()> {
    // Cross-scope deduplication
    if check_cross_scope_skip("Claude", scope, &claude_config_path(&Scope::Global)) {
        return Ok(());
    }

    let path = claude_config_path(scope)?;

    let mut config: serde_json::Value = if path.exists() {
        let content = std::fs::read_to_string(&path)
            .with_context(|| format!("Failed to read {}", path.display()))?;
        serde_json::from_str(&content)
            .with_context(|| format!("Failed to parse {}", path.display()))?
    } else {
        serde_json::json!({})
    };

    let server_entry = serde_json::json!({
        "type": "stdio",
        "command": binary,
        "args": ["serve"]
    });

    // Check idempotency — if already configured with same settings, no-op
    if let Some(existing) = config.get("mcpServers").and_then(|s| s.get("atm")) {
        if existing == &server_entry {
            println!("atm MCP server already configured for Claude Code ({}).", scope_label(scope));
            println!("  Config: {}", path.display());
            println!("  No changes made.");
            return Ok(());
        }
        // Different config exists — will be updated
        println!("Updating existing atm MCP server entry for Claude Code ({}).", scope_label(scope));
    }

    let servers = config
        .as_object_mut()
        .context("Config is not a JSON object")?
        .entry("mcpServers")
        .or_insert_with(|| serde_json::json!({}));

    servers
        .as_object_mut()
        .context("mcpServers is not an object")?
        .insert("atm".to_string(), server_entry);

    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }

    let formatted = serde_json::to_string_pretty(&config)?;
    std::fs::write(&path, formatted.as_bytes())
        .with_context(|| format!("Failed to write {}", path.display()))?;

    println!("Installed atm MCP server for Claude Code ({})", scope_label(scope));
    println!("  Config: {}", path.display());
    println!("  Binary: {binary}");
    println!("\nRestart Claude Code to pick up the new configuration.");

    Ok(())
}

fn uninstall_claude(scope: &Scope) -> Result<()> {
    let path = claude_config_path(scope)?;
    uninstall_json_client("Claude Code", &path, scope)
}

/// Generic JSON uninstall for Claude Code and Gemini.
fn uninstall_json_client(client_name: &str, path: &PathBuf, scope: &Scope) -> Result<()> {
    if !path.exists() {
        println!("atm MCP server not present for {} ({}).", client_name, scope_label(scope));
        println!("  Config: {} (file does not exist)", path.display());
        return Ok(());
    }

    let content = std::fs::read_to_string(path)
        .with_context(|| format!("Failed to read {}", path.display()))?;
    let mut config: serde_json::Value = serde_json::from_str(&content)
        .with_context(|| format!("Failed to parse {}", path.display()))?;

    let removed = config
        .get_mut("mcpServers")
        .and_then(|s| s.as_object_mut())
        .map(|servers| servers.remove("atm").is_some())
        .unwrap_or(false);

    if !removed {
        println!("atm MCP server not present for {} ({}).", client_name, scope_label(scope));
        println!("  Config: {}", path.display());
        return Ok(());
    }

    let formatted = serde_json::to_string_pretty(&config)?;
    std::fs::write(path, formatted.as_bytes())
        .with_context(|| format!("Failed to write {}", path.display()))?;

    println!("Removed atm MCP server from {} ({}).", client_name, scope_label(scope));
    println!("  Config: {}", path.display());
    println!("\nRestart {} to pick up the change.", client_name);

    Ok(())
}

// --- Codex ---

fn codex_config_path() -> Result<PathBuf> {
    Ok(home_dir()?.join(".codex/config.toml"))
}

fn is_codex_atm_configured(path: &PathBuf) -> bool {
    let content = match std::fs::read_to_string(path) {
        Ok(c) => c,
        Err(_) => return false,
    };
    content
        .parse::<toml::Table>()
        .ok()
        .and_then(|t| t.get("mcp_servers")?.as_table()?.get("atm").cloned())
        .is_some()
}

fn install_codex(binary: &str, scope: &Scope) -> Result<()> {
    if matches!(scope, Scope::Local) {
        bail!("Codex CLI only supports global MCP configuration (~/.codex/config.toml)");
    }

    let path = codex_config_path()?;

    let content = if path.exists() {
        std::fs::read_to_string(&path)
            .with_context(|| format!("Failed to read {}", path.display()))?
    } else {
        String::new()
    };

    // Parse existing TOML to check for idempotency
    if !content.is_empty() {
        if let Ok(table) = content.parse::<toml::Table>() {
            if let Some(mcp_servers) = table.get("mcp_servers").and_then(|v| v.as_table()) {
                if let Some(atm_entry) = mcp_servers.get("atm").and_then(|v| v.as_table()) {
                    let existing_cmd = atm_entry.get("command").and_then(|v| v.as_str());
                    let existing_args = atm_entry.get("args").and_then(|v| v.as_array());
                    let args_match = existing_args
                        .map(|a| {
                            a.len() == 1
                                && (a.first().and_then(|v| v.as_str()) == Some("serve"))
                        })
                        .unwrap_or(false);

                    if existing_cmd == Some(binary) && args_match {
                        println!("atm MCP server already configured for Codex (global).");
                        println!("  Config: {}", path.display());
                        println!("  No changes made.");
                        return Ok(());
                    }
                    println!("Updating existing atm MCP server entry for Codex (global).");
                }
            }
        }
    }

    // Parse-and-merge: parse existing TOML, update/add [mcp_servers.atm], re-serialize
    let mut table: toml::Table = if content.is_empty() {
        toml::Table::new()
    } else {
        content
            .parse::<toml::Table>()
            .with_context(|| format!("Failed to parse {} as TOML", path.display()))?
    };

    let mcp_servers = table
        .entry("mcp_servers")
        .or_insert_with(|| toml::Value::Table(toml::Table::new()));

    let mcp_table = mcp_servers
        .as_table_mut()
        .context("mcp_servers is not a TOML table")?;

    let mut atm_table = toml::Table::new();
    atm_table.insert(
        "command".to_string(),
        toml::Value::String(binary.to_string()),
    );
    atm_table.insert(
        "args".to_string(),
        toml::Value::Array(vec![toml::Value::String("serve".to_string())]),
    );

    mcp_table.insert("atm".to_string(), toml::Value::Table(atm_table));

    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }

    let serialized = toml::to_string_pretty(&table).context("Failed to serialize TOML")?;
    std::fs::write(&path, serialized.as_bytes())
        .with_context(|| format!("Failed to write {}", path.display()))?;

    println!("Installed atm MCP server for Codex (global)");
    println!("  Config: {}", path.display());
    println!("  Binary: {binary}");
    println!("\nRestart Codex to pick up the new configuration.");

    Ok(())
}

fn uninstall_codex(scope: &Scope) -> Result<()> {
    if matches!(scope, Scope::Local) {
        bail!("Codex CLI only supports global MCP configuration (~/.codex/config.toml)");
    }

    let path = codex_config_path()?;

    if !path.exists() {
        println!("atm MCP server not present for Codex (global).");
        println!("  Config: {} (file does not exist)", path.display());
        return Ok(());
    }

    let content = std::fs::read_to_string(&path)
        .with_context(|| format!("Failed to read {}", path.display()))?;

    let mut table: toml::Table = content
        .parse::<toml::Table>()
        .with_context(|| format!("Failed to parse {} as TOML", path.display()))?;

    let removed = table
        .get_mut("mcp_servers")
        .and_then(|v| v.as_table_mut())
        .map(|servers| servers.remove("atm").is_some())
        .unwrap_or(false);

    if !removed {
        println!("atm MCP server not present for Codex (global).");
        println!("  Config: {}", path.display());
        return Ok(());
    }

    let serialized = toml::to_string_pretty(&table).context("Failed to serialize TOML")?;
    std::fs::write(&path, serialized.as_bytes())
        .with_context(|| format!("Failed to write {}", path.display()))?;

    println!("Removed atm MCP server from Codex (global).");
    println!("  Config: {}", path.display());
    println!("\nRestart Codex to pick up the change.");

    Ok(())
}

// --- Gemini ---

fn gemini_config_path(scope: &Scope) -> Result<PathBuf> {
    match scope {
        Scope::Global => Ok(home_dir()?.join(".gemini/settings.json")),
        Scope::Local => Ok(PathBuf::from(".gemini/settings.json")),
    }
}

fn install_gemini(binary: &str, scope: &Scope) -> Result<()> {
    // Cross-scope deduplication
    if check_cross_scope_skip("Gemini", scope, &gemini_config_path(&Scope::Global)) {
        return Ok(());
    }

    let path = gemini_config_path(scope)?;

    let mut config: serde_json::Value = if path.exists() {
        let content = std::fs::read_to_string(&path)
            .with_context(|| format!("Failed to read {}", path.display()))?;
        serde_json::from_str(&content)
            .with_context(|| format!("Failed to parse {}", path.display()))?
    } else {
        serde_json::json!({})
    };

    let server_entry = serde_json::json!({
        "command": binary,
        "args": ["serve"]
    });

    // Check idempotency
    if let Some(existing) = config.get("mcpServers").and_then(|s| s.get("atm")) {
        if existing == &server_entry {
            println!(
                "atm MCP server already configured for Gemini CLI ({}).",
                scope_label(scope)
            );
            println!("  Config: {}", path.display());
            println!("  No changes made.");
            return Ok(());
        }
        println!(
            "Updating existing atm MCP server entry for Gemini CLI ({}).",
            scope_label(scope)
        );
    }

    let servers = config
        .as_object_mut()
        .context("Config is not a JSON object")?
        .entry("mcpServers")
        .or_insert_with(|| serde_json::json!({}));

    servers
        .as_object_mut()
        .context("mcpServers is not an object")?
        .insert("atm".to_string(), server_entry);

    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }

    let formatted = serde_json::to_string_pretty(&config)?;
    std::fs::write(&path, formatted.as_bytes())
        .with_context(|| format!("Failed to write {}", path.display()))?;

    println!(
        "Installed atm MCP server for Gemini CLI ({})",
        scope_label(scope)
    );
    println!("  Config: {}", path.display());
    println!("  Binary: {binary}");
    println!("\nRestart Gemini CLI to pick up the new configuration.");

    Ok(())
}

fn uninstall_gemini(scope: &Scope) -> Result<()> {
    let path = gemini_config_path(scope)?;
    uninstall_json_client("Gemini CLI", &path, scope)
}

// --- Install dispatch ---

fn execute_install(args: InstallArgs) -> Result<()> {
    let binary = find_mcp_binary(args.binary)?;

    match args.client {
        Client::Claude => install_claude(&binary, &args.scope),
        Client::Codex => install_codex(&binary, &args.scope),
        Client::Gemini => install_gemini(&binary, &args.scope),
    }
}

// --- Uninstall dispatch ---

fn execute_uninstall(args: UninstallArgs) -> Result<()> {
    match args.client {
        Client::Claude => uninstall_claude(&args.scope),
        Client::Codex => uninstall_codex(&args.scope),
        Client::Gemini => uninstall_gemini(&args.scope),
    }
}

// --- Status ---

fn execute_status() -> Result<()> {
    let binary_available = find_mcp_binary(None).is_ok();
    let binary_path = find_mcp_binary(None).unwrap_or_else(|_| "(not found)".to_string());

    println!("ATM MCP Server Status");
    println!("=====================");
    println!();
    println!("Binary: {binary_path}");
    println!(
        "Available: {}",
        if binary_available { "yes" } else { "no" }
    );
    println!();

    // Check Claude Code
    print_client_status("Claude Code", &[
        ("User   ", claude_config_path(&Scope::Global).ok()),
        ("Project", claude_config_path(&Scope::Local).ok()),
    ]);

    // Check Codex
    print_client_status("Codex", &[
        ("Global ", codex_config_path().ok()),
    ]);

    // Check Gemini
    print_client_status("Gemini CLI", &[
        ("User   ", gemini_config_path(&Scope::Global).ok()),
        ("Project", gemini_config_path(&Scope::Local).ok()),
    ]);

    if !binary_available {
        println!();
        println!("Install atm-agent-mcp with:");
        println!("  brew install randlee/tap/agent-team-mail");
        println!("  cargo install agent-team-mail-mcp");
    }

    Ok(())
}

fn print_client_status(name: &str, configs: &[(&str, Option<PathBuf>)]) {
    println!("{name}:");
    for (scope, path) in configs {
        if let Some(p) = path {
            let installed = check_atm_configured(p, name);
            let status = if installed {
                "configured"
            } else {
                "not configured"
            };
            println!("  {scope} {status:16} {}", p.display());
        }
    }
    println!();
}

fn check_atm_configured(path: &PathBuf, client_name: &str) -> bool {
    if client_name == "Codex" {
        is_codex_atm_configured(path)
    } else {
        is_json_atm_configured(path)
    }
}

fn scope_label(scope: &Scope) -> &'static str {
    match scope {
        Scope::Global => "global",
        Scope::Local => "local",
    }
}
