//! MCP server setup and management commands.
//!
//! Configures `atm-agent-mcp` as an MCP server for Claude Code, Codex CLI, and
//! Gemini CLI. Supports install, uninstall, and status across global (user) and
//! local (project) scopes. See `docs/requirements.md` section 4.8.

use anyhow::{Context, Result, bail};
use clap::{Args, Subcommand, ValueEnum};
use std::path::{Path, PathBuf};

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

/// Dispatch to the appropriate MCP subcommand.
///
/// # Errors
///
/// Returns an error when the selected subcommand fails due to invalid input,
/// config parse/write failures, or binary resolution issues.
pub fn execute(args: McpArgs) -> Result<()> {
    match args.command {
        McpCommands::Install(install_args) => execute_install(install_args),
        McpCommands::Uninstall(uninstall_args) => execute_uninstall(uninstall_args),
        McpCommands::Status => execute_status(),
    }
}

// ---------------------------------------------------------------------------
// Binary detection
// ---------------------------------------------------------------------------

/// Check if a file is executable (platform-appropriate).
fn is_executable(path: &Path) -> bool {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::metadata(path)
            .map(|m| m.permissions().mode() & 0o111 != 0)
            .unwrap_or(false)
    }
    #[cfg(not(unix))]
    {
        // On Windows, file existence + known extension is sufficient
        let _ = path;
        true
    }
}

/// Find a binary in PATH using in-process resolution (no shell dependency).
fn find_in_path(binary_name: &str) -> Option<PathBuf> {
    let path_var = std::env::var_os("PATH")?;
    for dir in std::env::split_paths(&path_var) {
        let candidate = dir.join(binary_name);
        if candidate.is_file() && is_executable(&candidate) {
            return Some(candidate);
        }
        // On Windows, also check common extensions. is_executable() currently
        // returns true on Windows, but we call it for forward-compatibility if
        // real Windows executable checks are added later.
        #[cfg(windows)]
        for ext in &["exe", "cmd", "bat"] {
            let with_ext = dir.join(format!("{binary_name}.{ext}"));
            if with_ext.is_file() && is_executable(&with_ext) {
                return Some(with_ext);
            }
        }
    }
    None
}

/// Resolve the `atm-agent-mcp` binary path from `--binary` override or PATH.
///
/// # Errors
///
/// Returns an error when the override path is invalid/non-executable (Unix),
/// or when `atm-agent-mcp` cannot be found on `PATH`.
fn find_mcp_binary(override_path: Option<PathBuf>) -> Result<String> {
    if let Some(path) = override_path {
        if !path.is_file() {
            bail!("Specified binary is not a file: {}", path.display());
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let perms = std::fs::metadata(&path)
                .with_context(|| format!("Cannot read metadata for {}", path.display()))?
                .permissions();
            if perms.mode() & 0o111 == 0 {
                bail!("Specified binary is not executable: {}", path.display());
            }
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
         cargo install agent-team-mail  (includes atm-agent-mcp binary)"
    );
}

// ---------------------------------------------------------------------------
// Config path helpers
// ---------------------------------------------------------------------------

fn home_dir() -> Result<PathBuf> {
    crate::util::settings::get_home_dir()
}

// Claude Code:
//   User/Global: ~/.claude.json  (mcpServers field, user scope)
//   Project/Local: .mcp.json     (project scope, committed to repo)
fn claude_config_path(scope: &Scope) -> Result<PathBuf> {
    match scope {
        Scope::Global => Ok(home_dir()?.join(".claude.json")),
        Scope::Local => Ok(PathBuf::from(".mcp.json")),
    }
}

// Codex CLI: ~/.codex/config.toml (global only)
fn codex_config_path() -> Result<PathBuf> {
    Ok(home_dir()?.join(".codex/config.toml"))
}

// Gemini CLI:
//   User/Global: ~/.gemini/settings.json
//   Project/Local: .gemini/settings.json
fn gemini_config_path(scope: &Scope) -> Result<PathBuf> {
    match scope {
        Scope::Global => Ok(home_dir()?.join(".gemini/settings.json")),
        Scope::Local => Ok(PathBuf::from(".gemini/settings.json")),
    }
}

// ---------------------------------------------------------------------------
// Config detection (used by status + cross-scope dedup)
// ---------------------------------------------------------------------------

/// Check if atm is configured in a JSON config file (Claude or Gemini).
fn is_json_atm_configured(path: &Path) -> bool {
    let content = match std::fs::read_to_string(path) {
        Ok(c) => c,
        Err(_) => return false,
    };
    serde_json::from_str::<serde_json::Value>(&content)
        .ok()
        .and_then(|config| config.get("mcpServers")?.get("atm").cloned())
        .is_some()
}

/// Check if atm is configured in Codex TOML config.
fn is_codex_atm_configured(path: &Path) -> bool {
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

/// Check if atm is configured for a given client at the specified path.
fn is_atm_configured(client: &Client, path: &Path) -> bool {
    match client {
        Client::Codex => is_codex_atm_configured(path),
        Client::Claude | Client::Gemini => is_json_atm_configured(path),
    }
}

/// Cross-scope check: if local scope requested, check if global already has it.
fn check_cross_scope_skip(client: &Client, scope: &Scope, global_path: Option<&Path>) -> bool {
    if !matches!(scope, Scope::Local) {
        return false;
    }
    if let Some(gpath) = global_path {
        if is_atm_configured(client, gpath) {
            println!("Project scope install skipped. atm MCP already installed globally.");
            println!("  Global config: {}", gpath.display());
            return true;
        }
    }
    false
}

// ---------------------------------------------------------------------------
// JSON write helper
// ---------------------------------------------------------------------------

/// Write JSON with trailing newline (important for git-tracked files like .mcp.json).
/// Note: serde_json::to_string_pretty reformats the entire file. Format-preserving
/// JSON merge is deferred to a future version if UX complaints arise.
///
/// # Errors
///
/// Returns an error when parent-directory creation, JSON serialization, or
/// file write fails.
fn write_json(path: &Path, config: &serde_json::Value) -> Result<()> {
    if let Some(parent) = path.parent() {
        if !parent.as_os_str().is_empty() {
            std::fs::create_dir_all(parent)?;
        }
    }
    let mut formatted = serde_json::to_string_pretty(config)?;
    formatted.push('\n');
    std::fs::write(path, formatted.as_bytes())
        .with_context(|| format!("Failed to write {}", path.display()))
}

/// Compare an existing JSON MCP server entry against the expected binary/args.
///
/// `require_stdio_type` is true for Claude Code (requires `"type":"stdio"`),
/// and false for Gemini CLI (extra keys like `"type"` are tolerated).
fn json_server_entry_matches(
    existing: &serde_json::Value,
    binary: &str,
    require_stdio_type: bool,
) -> bool {
    let command_matches = existing.get("command").and_then(|v| v.as_str()) == Some(binary);
    let args_match = existing
        .get("args")
        .and_then(|v| v.as_array())
        .map(|a| a.len() == 1 && a.first().and_then(|v| v.as_str()) == Some("serve"))
        .unwrap_or(false);
    if !command_matches || !args_match {
        return false;
    }
    if require_stdio_type {
        existing.get("type").and_then(|v| v.as_str()) == Some("stdio")
    } else {
        true
    }
}

// ---------------------------------------------------------------------------
// Install: Claude Code
// ---------------------------------------------------------------------------

fn install_claude(binary: &str, scope: &Scope) -> Result<()> {
    let global_path = claude_config_path(&Scope::Global).ok();
    if check_cross_scope_skip(&Client::Claude, scope, global_path.as_deref()) {
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

    // Claude Code requires "type": "stdio" per spec 4.8.1
    let server_entry = serde_json::json!({
        "type": "stdio",
        "command": binary,
        "args": ["serve"]
    });

    // Idempotency check
    if let Some(existing) = config.get("mcpServers").and_then(|s| s.get("atm")) {
        if json_server_entry_matches(existing, binary, true) {
            println!("atm MCP server already configured for Claude Code ({}).", scope_label(scope));
            println!("  Config: {}", path.display());
            println!("  No changes made.");
            return Ok(());
        }
        let old_cmd = existing.get("command").and_then(|v| v.as_str()).unwrap_or("(unknown)");
        println!("Updating existing atm MCP server entry for Claude Code ({}).", scope_label(scope));
        println!("  Old binary: {old_cmd}");
        println!("  New binary: {binary}");
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

    write_json(&path, &config)?;

    println!("Installed atm MCP server for Claude Code ({})", scope_label(scope));
    println!("  Config: {}", path.display());
    println!("  Binary: {binary}");
    println!("\nRestart Claude Code to pick up the new configuration.");

    Ok(())
}

// ---------------------------------------------------------------------------
// Install: Codex
// ---------------------------------------------------------------------------

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

    // Single parse for both idempotency check and merge
    let mut table: toml::Table = if content.is_empty() {
        toml::Table::new()
    } else {
        content
            .parse::<toml::Table>()
            .with_context(|| format!("Failed to parse {} as TOML", path.display()))?
    };

    // Idempotency check on the parsed table
    if let Some(mcp_servers) = table.get("mcp_servers").and_then(|v| v.as_table()) {
        if let Some(atm_entry) = mcp_servers.get("atm").and_then(|v| v.as_table()) {
            let existing_cmd = atm_entry.get("command").and_then(|v| v.as_str());
            let existing_args = atm_entry.get("args").and_then(|v| v.as_array());
            let args_match = existing_args
                .map(|a| {
                    a.len() == 1 && a.first().and_then(|v| v.as_str()) == Some("serve")
                })
                .unwrap_or(false);

            if existing_cmd == Some(binary) && args_match {
                println!("atm MCP server already configured for Codex (global).");
                println!("  Config: {}", path.display());
                println!("  No changes made.");
                return Ok(());
            }
            let old = existing_cmd.unwrap_or("(unknown)");
            println!("Updating existing atm MCP server entry for Codex (global).");
            println!("  Old binary: {old}");
            println!("  New binary: {binary}");
        }
    }

    // Merge: update/add [mcp_servers.atm]
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

// ---------------------------------------------------------------------------
// Install: Gemini
// ---------------------------------------------------------------------------

fn install_gemini(binary: &str, scope: &Scope) -> Result<()> {
    let global_path = gemini_config_path(&Scope::Global).ok();
    if check_cross_scope_skip(&Client::Gemini, scope, global_path.as_deref()) {
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

    // Gemini CLI does not use a "type" field in its mcpServers entries,
    // unlike Claude Code which requires "type": "stdio". This matches the
    // Gemini CLI docs and the spec table in section 4.8.1.
    let server_entry = serde_json::json!({
        "command": binary,
        "args": ["serve"]
    });

    // Idempotency check
    if let Some(existing) = config.get("mcpServers").and_then(|s| s.get("atm")) {
        if json_server_entry_matches(existing, binary, false) {
            println!(
                "atm MCP server already configured for Gemini CLI ({}).",
                scope_label(scope)
            );
            println!("  Config: {}", path.display());
            println!("  No changes made.");
            return Ok(());
        }
        let old_cmd = existing.get("command").and_then(|v| v.as_str()).unwrap_or("(unknown)");
        println!(
            "Updating existing atm MCP server entry for Gemini CLI ({}).",
            scope_label(scope)
        );
        println!("  Old binary: {old_cmd}");
        println!("  New binary: {binary}");
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

    write_json(&path, &config)?;

    println!(
        "Installed atm MCP server for Gemini CLI ({})",
        scope_label(scope)
    );
    println!("  Config: {}", path.display());
    println!("  Binary: {binary}");
    println!("\nRestart Gemini CLI to pick up the new configuration.");

    Ok(())
}

// ---------------------------------------------------------------------------
// Uninstall
// ---------------------------------------------------------------------------

fn uninstall_claude(scope: &Scope) -> Result<()> {
    let path = claude_config_path(scope)?;
    uninstall_json_client("Claude Code", &path, scope)
}

fn uninstall_gemini(scope: &Scope) -> Result<()> {
    let path = gemini_config_path(scope)?;
    uninstall_json_client("Gemini CLI", &path, scope)
}

/// Generic JSON uninstall for Claude Code and Gemini.
///
/// # Errors
///
/// Returns an error when JSON read/parse/write fails.
fn uninstall_json_client(client_name: &str, path: &Path, scope: &Scope) -> Result<()> {
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

    write_json(path, &config)?;

    println!("Removed atm MCP server from {} ({}).", client_name, scope_label(scope));
    println!("  Config: {}", path.display());
    println!("\nRestart {} to pick up the change.", client_name);

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

// ---------------------------------------------------------------------------
// Install / Uninstall dispatch
// ---------------------------------------------------------------------------

fn execute_install(args: InstallArgs) -> Result<()> {
    let binary = find_mcp_binary(args.binary)?;

    match args.client {
        Client::Claude => install_claude(&binary, &args.scope),
        Client::Codex => install_codex(&binary, &args.scope),
        Client::Gemini => install_gemini(&binary, &args.scope),
    }
}

fn execute_uninstall(args: UninstallArgs) -> Result<()> {
    match args.client {
        Client::Claude => uninstall_claude(&args.scope),
        Client::Codex => uninstall_codex(&args.scope),
        Client::Gemini => uninstall_gemini(&args.scope),
    }
}

// ---------------------------------------------------------------------------
// Status
// ---------------------------------------------------------------------------

/// Enum variant used for status dispatch (avoids stringly-typed comparison).
enum StatusClient {
    Claude,
    Codex,
    Gemini,
}

fn execute_status() -> Result<()> {
    // Single PATH scan (QA-002: avoid double scan race window)
    let binary_result = find_mcp_binary(None);
    let binary_available = binary_result.is_ok();
    let binary_path = binary_result.unwrap_or_else(|_| "(not found)".to_string());

    println!("ATM MCP Server Status");
    println!("=====================");
    println!();
    println!("Binary: {binary_path}");
    println!(
        "Available: {}",
        if binary_available { "yes" } else { "no" }
    );
    println!();

    print_client_status("Claude Code", StatusClient::Claude, &[
        ("User   ", claude_config_path(&Scope::Global).ok()),
        ("Project", claude_config_path(&Scope::Local).ok()),
    ]);

    print_client_status("Codex", StatusClient::Codex, &[
        ("Global ", codex_config_path().ok()),
    ]);

    print_client_status("Gemini CLI", StatusClient::Gemini, &[
        ("User   ", gemini_config_path(&Scope::Global).ok()),
        ("Project", gemini_config_path(&Scope::Local).ok()),
    ]);

    if !binary_available {
        println!();
        println!("Install atm-agent-mcp with:");
        println!("  brew install randlee/tap/agent-team-mail");
        println!("  cargo install agent-team-mail  (includes atm-agent-mcp binary)");
    }

    Ok(())
}

fn print_client_status(name: &str, client: StatusClient, configs: &[(&str, Option<PathBuf>)]) {
    println!("{name}:");
    for (scope, path) in configs {
        if let Some(p) = path {
            let installed = match client {
                StatusClient::Codex => is_codex_atm_configured(p),
                StatusClient::Claude | StatusClient::Gemini => is_json_atm_configured(p),
            };
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

fn scope_label(scope: &Scope) -> &'static str {
    match scope {
        Scope::Global => "global",
        Scope::Local => "local",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serial_test::serial;
    use std::env;
    use tempfile::TempDir;

    #[test]
    fn test_json_server_entry_matches_claude_requires_stdio_type() {
        let v = serde_json::json!({
            "type": "stdio",
            "command": "/bin/atm-agent-mcp",
            "args": ["serve"]
        });
        assert!(json_server_entry_matches(&v, "/bin/atm-agent-mcp", true));
    }

    #[test]
    fn test_json_server_entry_matches_gemini_allows_extra_type_field() {
        let v = serde_json::json!({
            "type": "stdio",
            "command": "/bin/atm-agent-mcp",
            "args": ["serve"]
        });
        assert!(json_server_entry_matches(&v, "/bin/atm-agent-mcp", false));
    }

    #[test]
    fn test_json_server_entry_matches_requires_serve_arg() {
        let v = serde_json::json!({
            "command": "/bin/atm-agent-mcp",
            "args": ["other"]
        });
        assert!(!json_server_entry_matches(&v, "/bin/atm-agent-mcp", false));
    }

    #[test]
    fn test_is_json_atm_configured_true_and_false() {
        let temp = TempDir::new().expect("tempdir");
        let good = temp.path().join("good.json");
        let bad = temp.path().join("bad.json");

        std::fs::write(
            &good,
            r#"{"mcpServers":{"atm":{"command":"atm-agent-mcp","args":["serve"]}}}"#,
        )
        .expect("write good");
        std::fs::write(&bad, r#"{"mcpServers":{"other":{"command":"x"}}}"#).expect("write bad");

        assert!(is_json_atm_configured(&good));
        assert!(!is_json_atm_configured(&bad));
    }

    #[test]
    fn test_is_codex_atm_configured_true_and_false() {
        let temp = TempDir::new().expect("tempdir");
        let good = temp.path().join("good.toml");
        let bad = temp.path().join("bad.toml");

        std::fs::write(
            &good,
            r#"[mcp_servers.atm]
command = "atm-agent-mcp"
args = ["serve"]
"#,
        )
        .expect("write good");
        std::fs::write(
            &bad,
            r#"[mcp_servers.other]
command = "x"
"#,
        )
        .expect("write bad");

        assert!(is_codex_atm_configured(&good));
        assert!(!is_codex_atm_configured(&bad));
    }

    #[test]
    fn test_scope_label_values() {
        assert_eq!(scope_label(&Scope::Global), "global");
        assert_eq!(scope_label(&Scope::Local), "local");
    }

    #[test]
    fn test_write_json_creates_parent_and_trailing_newline() {
        let temp = TempDir::new().expect("tempdir");
        let path = temp.path().join("nested/config.json");
        let value = serde_json::json!({"mcpServers":{"atm":{"command":"x","args":["serve"]}}});

        write_json(&path, &value).expect("write_json");
        let content = std::fs::read_to_string(&path).expect("read");
        assert!(content.ends_with('\n'));
        assert!(path.exists());
    }

    #[test]
    fn test_find_mcp_binary_override_missing_file_errors() {
        let missing = PathBuf::from("/definitely/not/present/atm-agent-mcp");
        let err = find_mcp_binary(Some(missing)).expect_err("expected error");
        assert!(err.to_string().contains("not a file"));
    }

    #[test]
    fn test_check_cross_scope_skip_true_when_global_configured() {
        let temp = TempDir::new().expect("tempdir");
        let global = temp.path().join("global.json");
        std::fs::write(
            &global,
            r#"{"mcpServers":{"atm":{"command":"atm-agent-mcp","args":["serve"]}}}"#,
        )
        .expect("write global");

        assert!(check_cross_scope_skip(
            &Client::Claude,
            &Scope::Local,
            Some(global.as_path())
        ));
    }

    #[test]
    #[serial]
    fn test_install_and_uninstall_claude_global_round_trip() {
        let temp = TempDir::new().expect("tempdir");
        let old_home = env::var("ATM_HOME").ok();
        unsafe { env::set_var("ATM_HOME", temp.path()) };

        install_claude("/bin/atm-agent-mcp", &Scope::Global).expect("install");
        let path = claude_config_path(&Scope::Global).expect("path");
        assert!(is_json_atm_configured(&path));
        uninstall_claude(&Scope::Global).expect("uninstall");
        assert!(!is_json_atm_configured(&path));

        unsafe {
            match old_home {
                Some(v) => env::set_var("ATM_HOME", v),
                None => env::remove_var("ATM_HOME"),
            }
        }
    }

    #[test]
    fn test_install_codex_local_scope_errors() {
        let err = install_codex("/bin/atm-agent-mcp", &Scope::Local).expect_err("expected error");
        assert!(err.to_string().contains("only supports global"));
    }

    #[test]
    #[serial]
    fn test_install_and_uninstall_codex_global_round_trip() {
        let temp = TempDir::new().expect("tempdir");
        let old_home = env::var("ATM_HOME").ok();
        unsafe { env::set_var("ATM_HOME", temp.path()) };

        install_codex("/bin/atm-agent-mcp", &Scope::Global).expect("install");
        let path = codex_config_path().expect("path");
        assert!(is_codex_atm_configured(&path));
        uninstall_codex(&Scope::Global).expect("uninstall");
        assert!(!is_codex_atm_configured(&path));

        unsafe {
            match old_home {
                Some(v) => env::set_var("ATM_HOME", v),
                None => env::remove_var("ATM_HOME"),
            }
        }
    }

    #[test]
    #[serial]
    fn test_install_and_uninstall_gemini_local_round_trip() {
        let temp = TempDir::new().expect("tempdir");
        let old_cwd = env::current_dir().expect("cwd");
        env::set_current_dir(temp.path()).expect("set cwd");

        install_gemini("/bin/atm-agent-mcp", &Scope::Local).expect("install");
        let path = gemini_config_path(&Scope::Local).expect("path");
        assert!(is_json_atm_configured(&path));
        uninstall_gemini(&Scope::Local).expect("uninstall");
        assert!(!is_json_atm_configured(&path));

        env::set_current_dir(old_cwd).expect("restore cwd");
    }

    #[test]
    #[serial]
    fn test_install_gemini_idempotent_with_extra_type_field() {
        let temp = TempDir::new().expect("tempdir");
        let original_atm_home = env::var("ATM_HOME").ok();
        let original_cwd = env::current_dir().expect("cwd");
        unsafe {
            env::set_var("ATM_HOME", temp.path());
        }
        env::set_current_dir(temp.path()).expect("set cwd");

        let path = temp.path().join(".gemini/settings.json");
        std::fs::create_dir_all(path.parent().expect("parent")).expect("mkdir");
        let initial = serde_json::json!({
            "mcpServers": {
                "atm": {
                    "type": "stdio",
                    "command": "/bin/atm-agent-mcp",
                    "args": ["serve"]
                }
            }
        });
        let mut formatted = serde_json::to_string_pretty(&initial).expect("serialize");
        formatted.push('\n');
        std::fs::write(&path, formatted.as_bytes()).expect("write");

        install_gemini("/bin/atm-agent-mcp", &Scope::Global).expect("install");

        let after = std::fs::read_to_string(&path).expect("read after");
        let parsed: serde_json::Value = serde_json::from_str(&after).expect("parse after");
        assert_eq!(
            parsed
                .get("mcpServers")
                .and_then(|s| s.get("atm"))
                .and_then(|atm| atm.get("type"))
                .and_then(|v| v.as_str()),
            Some("stdio")
        );

        env::set_current_dir(original_cwd).expect("restore cwd");
        unsafe {
            match original_atm_home {
                Some(v) => env::set_var("ATM_HOME", v),
                None => env::remove_var("ATM_HOME"),
            }
        }
    }

    #[test]
    #[serial]
    fn test_find_in_path_prefers_executable_file() {
        let temp = TempDir::new().expect("tempdir");
        let old_path = env::var_os("PATH");
        unsafe {
            env::set_var("PATH", temp.path());
        }
        let bin = temp.path().join("atm-agent-mcp");
        std::fs::write(&bin, b"#!/bin/sh\nexit 0\n").expect("write bin");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mut perms = std::fs::metadata(&bin).expect("metadata").permissions();
            perms.set_mode(0o755);
            std::fs::set_permissions(&bin, perms).expect("chmod");
        }

        let found = find_in_path("atm-agent-mcp");
        assert_eq!(found.as_deref(), Some(bin.as_path()));

        unsafe {
            match old_path {
                Some(v) => env::set_var("PATH", v),
                None => env::remove_var("PATH"),
            }
        }
    }
}
