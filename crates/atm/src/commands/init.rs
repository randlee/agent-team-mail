//! `atm init` — Install Claude Code hook wiring for ATM session coordination.
//!
//! Writes ATM integration hooks into a Claude Code `settings.json` file.
//! Supports both project-local (`.claude/settings.json` in the current directory)
//! and global (`~/.claude/settings.json`) installation.
//!
//! ## Idempotency
//!
//! Running `atm init` multiple times is safe. Hooks are only appended when the
//! exact command string is not already present. All existing hooks and unrelated
//! settings are preserved through read-modify-write semantics.
//!
//! ## Atomic writes
//!
//! Settings are written to a temporary sibling file and then renamed into place
//! to avoid partial writes corrupting the target file.
//!
//! ## Hook scripts
//!
//! Python hook scripts are embedded in the binary at compile time via
//! `include_str!()` and materialized to disk during `atm init`. This means no
//! external script files are required post-install.

use agent_team_mail_core::schema::{AgentMember, TeamConfig};
use agent_team_mail_core::team_config_store::TeamConfigStore;
use anyhow::{Context, Result};
use clap::Args;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::Command;

// ---------------------------------------------------------------------------
// Embedded hook script bodies (compile-time)
// ---------------------------------------------------------------------------

const SESSION_START_PY: &str = include_str!("../../scripts/session-start.py");
const SESSION_END_PY: &str = include_str!("../../scripts/session-end.py");
const TEAMMATE_IDLE_RELAY_PY: &str = include_str!("../../scripts/teammate-idle-relay.py");
const PERMISSION_REQUEST_RELAY_PY: &str = include_str!("../../scripts/permission-request-relay.py");
const STOP_RELAY_PY: &str = include_str!("../../scripts/stop-relay.py");
const NOTIFICATION_IDLE_RELAY_PY: &str = include_str!("../../scripts/notification-idle-relay.py");
const ATM_IDENTITY_WRITE_PY: &str = include_str!("../../scripts/atm-identity-write.py");
const ATM_IDENTITY_CLEANUP_PY: &str = include_str!("../../scripts/atm-identity-cleanup.py");
const GATE_AGENT_SPAWNS_PY: &str = include_str!("../../scripts/gate-agent-spawns.py");
const ATM_HOOK_LIB_PY: &str = include_str!("../../scripts/atm_hook_lib.py");
const ATM_HOOK_RELAY_PY: &str = include_str!("../../scripts/atm-hook-relay.py");

// ---------------------------------------------------------------------------
// Hook command templates
// ---------------------------------------------------------------------------

// Hooks installed by `atm init`:
// - SessionStart: announce session ID and optionally notify daemon
// - PermissionRequest: transition daemon activity to blocked-permission
// - Stop: transition daemon activity to idle
// - Notification(idle_prompt): periodic idle heartbeat to daemon
// - PreToolUse(Bash): write PID-based identity file before `atm` commands
// - PreToolUse(Task): gate agent spawning pattern enforcement
// - PostToolUse(Bash): clean up PID identity file after `atm` commands
//
// Note: `atm init` installs the core hook commands, including SessionEnd.
// Teammate-idle relay scripts are also materialized for lifecycle parity.

/// Return the SessionStart hook command string for local or global install.
pub(crate) fn session_start_cmd(global_scripts_dir: Option<&Path>) -> String {
    let script = hook_script_path(global_scripts_dir, "session-start.py");
    format!("{} \"{script}\"", hook_python_cmd())
}

/// Return the SessionEnd hook command string for local or global install.
pub(crate) fn session_end_cmd(global_scripts_dir: Option<&Path>) -> String {
    let script = hook_script_path(global_scripts_dir, "session-end.py");
    format!("{} \"{script}\"", hook_python_cmd())
}

/// Return the PermissionRequest hook command string for local or global install.
pub(crate) fn permission_request_cmd(global_scripts_dir: Option<&Path>) -> String {
    let script = hook_script_path(global_scripts_dir, "permission-request-relay.py");
    format!("{} \"{script}\"", hook_python_cmd())
}

/// Return the Stop hook command string for local or global install.
pub(crate) fn stop_cmd(global_scripts_dir: Option<&Path>) -> String {
    let script = hook_script_path(global_scripts_dir, "stop-relay.py");
    format!("{} \"{script}\"", hook_python_cmd())
}

/// Return the Notification(idle_prompt) hook command string for local/global install.
pub(crate) fn notification_idle_prompt_cmd(global_scripts_dir: Option<&Path>) -> String {
    let script = hook_script_path(global_scripts_dir, "notification-idle-relay.py");
    format!("{} \"{script}\"", hook_python_cmd())
}

/// Return the PreToolUse(Bash) hook command string for local or global install.
pub(crate) fn pre_tool_use_bash_cmd(global_scripts_dir: Option<&Path>) -> String {
    let script = hook_script_path(global_scripts_dir, "atm-identity-write.py");
    format!("{} \"{script}\"", hook_python_cmd())
}

/// Return the PreToolUse(Task) hook command string for local or global install.
pub(crate) fn pre_tool_use_task_cmd(global_scripts_dir: Option<&Path>) -> String {
    let script = hook_script_path(global_scripts_dir, "gate-agent-spawns.py");
    format!("{} \"{script}\"", hook_python_cmd())
}

/// Return the PostToolUse(Bash) hook command string for local or global install.
pub(crate) fn post_tool_use_bash_cmd(global_scripts_dir: Option<&Path>) -> String {
    let script = hook_script_path(global_scripts_dir, "atm-identity-cleanup.py");
    format!("{} \"{script}\"", hook_python_cmd())
}

/// Return a hook script path expression:
/// - Local: uses `$CLAUDE_PROJECT_DIR` so settings remain repo-portable.
/// - Global: uses a resolved absolute per-user path for robustness.
pub(crate) fn hook_script_path(global_scripts_dir: Option<&Path>, script_name: &str) -> String {
    match global_scripts_dir {
        Some(dir) => normalize_for_bash_quoted_path(&dir.join(script_name)),
        None => format!("${{CLAUDE_PROJECT_DIR}}/.claude/scripts/{script_name}"),
    }
}

/// Resolve the Python interpreter path to embed into hook commands.
///
/// Claude executes hooks in a minimal shell environment where `python3` may not
/// be on `PATH` (for example, teammate sessions launched from tmux panes). We
/// therefore prefer an absolute interpreter path captured at install time.
fn hook_python_cmd() -> String {
    if let Some(value) = std::env::var_os("ATM_HOOK_PYTHON") {
        return normalize_for_bash_quoted_path(Path::new(&value));
    }

    if let Ok(output) = Command::new("python3")
        .args(["-c", "import sys; print(sys.executable)"])
        .output()
        && output.status.success()
    {
        let resolved = String::from_utf8_lossy(&output.stdout).trim().to_string();
        if !resolved.is_empty() {
            return normalize_for_bash_quoted_path(Path::new(&resolved));
        }
    }

    for candidate in [
        "/opt/homebrew/bin/python3",
        "/usr/local/bin/python3",
        "/usr/bin/python3",
    ] {
        if Path::new(candidate).exists() {
            return normalize_for_bash_quoted_path(Path::new(candidate));
        }
    }

    "python3".to_string()
}

/// Normalize a filesystem path for inclusion inside a double-quoted command
/// argument.
fn normalize_for_bash_quoted_path(path: &Path) -> String {
    path.to_string_lossy()
        .replace('\\', "/")
        .replace('"', "\\\"")
}

// ---------------------------------------------------------------------------
// CLI argument types
// ---------------------------------------------------------------------------

/// Install Claude Code hook wiring for ATM session coordination
#[derive(Args, Debug)]
pub struct InitArgs {
    /// Name of the ATM team to configure hooks for
    pub team: String,

    /// Install hooks into project-local `.claude/settings.json` (default is global)
    #[arg(long)]
    pub local: bool,

    /// Identity written to `.atm.toml` when it is created
    #[arg(long)]
    pub identity: Option<String>,

    /// Skip team creation step (`~/.claude/teams/<team>/config.json`)
    #[arg(long)]
    pub skip_team: bool,

    /// Show planned install actions without writing files
    #[arg(long)]
    pub dry_run: bool,

    /// Legacy compatibility flag; global install is already the default.
    #[arg(long, hide = true, conflicts_with = "local")]
    pub global: bool,
}

// ---------------------------------------------------------------------------
// Public entry point
// ---------------------------------------------------------------------------

/// Execute the `atm init` command.
///
/// Resolves the target `settings.json` path, loads any existing content,
/// merges the ATM hooks idempotently, and writes the result atomically.
///
/// The `team` argument is used in informational output only and does not
/// parameterize the installed hook commands. Hook scripts resolve team
/// identity at runtime via `.atm.toml` in the project directory.
///
/// # Errors
///
/// Returns an error when the home directory cannot be resolved, the settings
/// file cannot be read or parsed, or the atomic write fails.
pub fn execute(args: InitArgs) -> Result<()> {
    let install_global = args.global || !args.local;
    let identity = args
        .identity
        .clone()
        .unwrap_or_else(|| "team-lead".to_string());
    let current_dir = std::env::current_dir().context("Cannot determine current directory")?;
    let atm_toml_path = current_dir.join(".atm.toml");
    let home_dir = crate::util::settings::get_home_dir()?;
    let config_home = home_dir.clone();
    let settings_path = resolve_settings_path(install_global)?;

    let scripts_dir = if install_global {
        crate::util::settings::config_claude_root_dir_for(&config_home).join("scripts")
    } else {
        current_dir.join(".claude").join("scripts")
    };

    let mut settings = load_settings(&settings_path)?;
    let mut dry_run_settings = if args.dry_run {
        Some(settings.clone())
    } else {
        None
    };
    let report = merge_hooks(
        dry_run_settings.as_mut().unwrap_or(&mut settings),
        if install_global {
            Some(&scripts_dir)
        } else {
            None
        },
    )?;

    let claude_runtime = RuntimeInstallReport {
        runtime: "claude",
        status: if report.all_present() {
            RuntimeInstallStatus::AlreadyConfigured
        } else if report.all_added() {
            RuntimeInstallStatus::Installed
        } else {
            RuntimeInstallStatus::Updated
        },
        detail: None,
        path: Some(settings_path.clone()),
    };

    let (atm_toml_status, team_status, runtime_reports) = if args.dry_run {
        let atm_toml_status = if atm_toml_path.exists() {
            AtmTomlStatus::AlreadyPresent
        } else {
            AtmTomlStatus::WouldCreate
        };
        let team_status = if args.skip_team {
            TeamStatus::Skipped
        } else if crate::util::settings::config_team_dir_for(&config_home, &args.team)
            .join("config.json")
            .exists()
        {
            TeamStatus::AlreadyPresent
        } else {
            TeamStatus::WouldCreate
        };
        let runtime_reports = vec![
            claude_runtime,
            plan_codex_runtime(&home_dir, &scripts_dir),
            plan_gemini_runtime(&home_dir, &scripts_dir),
        ];
        (atm_toml_status, team_status, runtime_reports)
    } else {
        let atm_toml_status = ensure_atm_toml(&atm_toml_path, &args.team, &identity)?;
        let team_status = if args.skip_team {
            TeamStatus::Skipped
        } else {
            ensure_team_config(&config_home, &args.team, &current_dir)?
        };
        // Materialize hook scripts to disk before writing settings
        materialize_scripts(&scripts_dir)?;
        write_settings_atomic(&settings_path, &settings)?;
        ensure_compose_bootstrap(&current_dir)?;
        let runtime_reports = vec![
            claude_runtime,
            configure_codex_runtime(&home_dir, &scripts_dir),
            configure_gemini_runtime(&home_dir, &scripts_dir),
        ];
        (atm_toml_status, team_status, runtime_reports)
    };

    print_report(
        &args.team,
        &settings_path,
        &report,
        install_global,
        &atm_toml_path,
        atm_toml_status,
        team_status,
        args.dry_run,
        &runtime_reports,
    );

    Ok(())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum AtmTomlStatus {
    Created,
    AlreadyPresent,
    WouldCreate,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TeamStatus {
    Created,
    AlreadyPresent,
    WouldCreate,
    Skipped,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RuntimeInstallStatus {
    Installed,
    Updated,
    AlreadyConfigured,
    SkippedNotDetected,
    Error,
}

impl RuntimeInstallStatus {
    fn as_str(self) -> &'static str {
        match self {
            RuntimeInstallStatus::Installed => "installed",
            RuntimeInstallStatus::Updated => "updated",
            RuntimeInstallStatus::AlreadyConfigured => "already-configured",
            RuntimeInstallStatus::SkippedNotDetected => "skipped-not-detected",
            RuntimeInstallStatus::Error => "error",
        }
    }
}

#[derive(Debug, Clone)]
struct RuntimeInstallReport {
    runtime: &'static str,
    status: RuntimeInstallStatus,
    detail: Option<String>,
    path: Option<PathBuf>,
}

fn configure_codex_runtime(home_dir: &Path, scripts_dir: &Path) -> RuntimeInstallReport {
    let path = home_dir.join(".codex/config.toml");
    if !runtime_detected("codex", &path) {
        return RuntimeInstallReport {
            runtime: "codex",
            status: RuntimeInstallStatus::SkippedNotDetected,
            detail: None,
            path: Some(path),
        };
    }
    let relay_script = scripts_dir.join("atm-hook-relay.py");
    match install_codex_notify_config(&path, &relay_script) {
        Ok(status) => RuntimeInstallReport {
            runtime: "codex",
            status,
            detail: None,
            path: Some(path),
        },
        Err(err) => RuntimeInstallReport {
            runtime: "codex",
            status: RuntimeInstallStatus::Error,
            detail: Some(err.to_string()),
            path: Some(path),
        },
    }
}

fn plan_codex_runtime(home_dir: &Path, scripts_dir: &Path) -> RuntimeInstallReport {
    let path = home_dir.join(".codex/config.toml");
    if !runtime_detected("codex", &path) {
        return RuntimeInstallReport {
            runtime: "codex",
            status: RuntimeInstallStatus::SkippedNotDetected,
            detail: None,
            path: Some(path),
        };
    }
    let relay_script = scripts_dir.join("atm-hook-relay.py");
    match preview_codex_notify_config(&path, &relay_script) {
        Ok(status) => RuntimeInstallReport {
            runtime: "codex",
            status,
            detail: None,
            path: Some(path),
        },
        Err(err) => RuntimeInstallReport {
            runtime: "codex",
            status: RuntimeInstallStatus::Error,
            detail: Some(err.to_string()),
            path: Some(path),
        },
    }
}

fn configure_gemini_runtime(home_dir: &Path, scripts_dir: &Path) -> RuntimeInstallReport {
    let path = home_dir.join(".gemini/settings.json");
    if !runtime_detected("gemini", &home_dir.join(".gemini")) {
        return RuntimeInstallReport {
            runtime: "gemini",
            status: RuntimeInstallStatus::SkippedNotDetected,
            detail: None,
            path: Some(path),
        };
    }
    match install_gemini_hook_config(&path, scripts_dir) {
        Ok(status) => RuntimeInstallReport {
            runtime: "gemini",
            status,
            detail: None,
            path: Some(path),
        },
        Err(err) => RuntimeInstallReport {
            runtime: "gemini",
            status: RuntimeInstallStatus::Error,
            detail: Some(err.to_string()),
            path: Some(path),
        },
    }
}

fn plan_gemini_runtime(home_dir: &Path, scripts_dir: &Path) -> RuntimeInstallReport {
    let path = home_dir.join(".gemini/settings.json");
    if !runtime_detected("gemini", &home_dir.join(".gemini")) {
        return RuntimeInstallReport {
            runtime: "gemini",
            status: RuntimeInstallStatus::SkippedNotDetected,
            detail: None,
            path: Some(path),
        };
    }
    match preview_gemini_hook_config(&path, scripts_dir) {
        Ok(status) => RuntimeInstallReport {
            runtime: "gemini",
            status,
            detail: None,
            path: Some(path),
        },
        Err(err) => RuntimeInstallReport {
            runtime: "gemini",
            status: RuntimeInstallStatus::Error,
            detail: Some(err.to_string()),
            path: Some(path),
        },
    }
}

pub(crate) fn runtime_detected(binary_name: &str, config_path: &Path) -> bool {
    find_in_path(binary_name).is_some() || config_path.exists()
}

fn find_in_path(binary_name: &str) -> Option<PathBuf> {
    let path_var = std::env::var_os("PATH")?;
    for dir in std::env::split_paths(&path_var) {
        let candidate = dir.join(binary_name);
        if is_executable(&candidate) {
            return Some(candidate);
        }
        #[cfg(windows)]
        for ext in &["exe", "cmd", "bat"] {
            let with_ext = dir.join(format!("{binary_name}.{ext}"));
            if is_executable(&with_ext) {
                return Some(with_ext);
            }
        }
    }
    None
}

fn is_executable(path: &Path) -> bool {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::metadata(path)
            .map(|m| m.is_file() && (m.permissions().mode() & 0o111 != 0))
            .unwrap_or(false)
    }
    #[cfg(not(unix))]
    {
        path.is_file()
    }
}

fn install_codex_notify_config(path: &Path, relay_script: &Path) -> Result<RuntimeInstallStatus> {
    let file_exists = path.exists();
    let mut table = if file_exists {
        let content = std::fs::read_to_string(path)
            .with_context(|| format!("Failed to read {}", path.display()))?;
        content
            .parse::<toml::Table>()
            .with_context(|| format!("Failed to parse {}", path.display()))?
    } else {
        toml::Table::new()
    };

    let desired = vec![
        toml::Value::String("python3".to_string()),
        toml::Value::String(normalize_path_for_runtime_config(relay_script)),
    ];

    if let Some(existing) = table.get("notify") {
        if let Some(existing_array) = existing.as_array() {
            if existing_array == &desired {
                return Ok(RuntimeInstallStatus::AlreadyConfigured);
            }
            anyhow::bail!(
                "Detected existing Codex notify configuration. \
                 Update {} manually so notify = [\"python3\", \"{}\"]",
                path.display(),
                relay_script.display()
            );
        }
        anyhow::bail!(
            "Codex config {} contains non-array `notify`; cannot auto-configure",
            path.display()
        );
    }

    table.insert("notify".to_string(), toml::Value::Array(desired));
    let mut serialized =
        toml::to_string_pretty(&table).context("Failed to serialize Codex TOML")?;
    if !serialized.ends_with('\n') {
        serialized.push('\n');
    }
    write_text_atomic(path, &serialized)?;
    Ok(if file_exists {
        RuntimeInstallStatus::Updated
    } else {
        RuntimeInstallStatus::Installed
    })
}

fn preview_codex_notify_config(path: &Path, relay_script: &Path) -> Result<RuntimeInstallStatus> {
    let file_exists = path.exists();
    let mut table = if file_exists {
        let content = std::fs::read_to_string(path)
            .with_context(|| format!("Failed to read {}", path.display()))?;
        content
            .parse::<toml::Table>()
            .with_context(|| format!("Failed to parse {}", path.display()))?
    } else {
        toml::Table::new()
    };

    let desired = vec![
        toml::Value::String("python3".to_string()),
        toml::Value::String(normalize_path_for_runtime_config(relay_script)),
    ];

    if let Some(existing) = table.get("notify") {
        if let Some(existing_array) = existing.as_array() {
            if existing_array == &desired {
                return Ok(RuntimeInstallStatus::AlreadyConfigured);
            }
            anyhow::bail!(
                "Detected existing Codex notify configuration. \
                 Update {} manually so notify = [\"python3\", \"{}\"]",
                path.display(),
                relay_script.display()
            );
        }
        anyhow::bail!(
            "Codex config {} contains non-array `notify`; cannot auto-configure",
            path.display()
        );
    }

    table.insert("notify".to_string(), toml::Value::Array(desired));
    Ok(if file_exists {
        RuntimeInstallStatus::Updated
    } else {
        RuntimeInstallStatus::Installed
    })
}

fn install_gemini_hook_config(path: &Path, scripts_dir: &Path) -> Result<RuntimeInstallStatus> {
    let existed = path.exists();
    let mut settings = if existed {
        let content = std::fs::read_to_string(path)
            .with_context(|| format!("Failed to read {}", path.display()))?;
        serde_json::from_str::<serde_json::Value>(&content)
            .with_context(|| format!("Failed to parse {}", path.display()))?
    } else {
        serde_json::json!({})
    };

    let mut added_any = false;
    let session_start = format!(
        "python3 \"{}\"",
        normalize_for_bash_quoted_path(&scripts_dir.join("session-start.py"))
    );
    let session_end = format!(
        "python3 \"{}\"",
        normalize_for_bash_quoted_path(&scripts_dir.join("session-end.py"))
    );
    let after_agent = format!(
        "python3 \"{}\"",
        normalize_for_bash_quoted_path(&scripts_dir.join("teammate-idle-relay.py"))
    );

    if ensure_gemini_hook_command(
        &mut settings,
        "SessionStart",
        "atm-session-start",
        &session_start,
    )? == HookStatus::Added
    {
        added_any = true;
    }
    if ensure_gemini_hook_command(&mut settings, "SessionEnd", "atm-session-end", &session_end)?
        == HookStatus::Added
    {
        added_any = true;
    }
    if ensure_gemini_hook_command(&mut settings, "AfterAgent", "atm-after-agent", &after_agent)?
        == HookStatus::Added
    {
        added_any = true;
    }

    if !added_any {
        return Ok(RuntimeInstallStatus::AlreadyConfigured);
    }

    write_settings_atomic(path, &settings)?;
    Ok(if existed {
        RuntimeInstallStatus::Updated
    } else {
        RuntimeInstallStatus::Installed
    })
}

fn preview_gemini_hook_config(path: &Path, scripts_dir: &Path) -> Result<RuntimeInstallStatus> {
    let existed = path.exists();
    let mut settings = if existed {
        let content = std::fs::read_to_string(path)
            .with_context(|| format!("Failed to read {}", path.display()))?;
        serde_json::from_str::<serde_json::Value>(&content)
            .with_context(|| format!("Failed to parse {}", path.display()))?
    } else {
        serde_json::json!({})
    };
    let mut added_any = false;
    let session_start = format!(
        "python3 \"{}\"",
        normalize_for_bash_quoted_path(&scripts_dir.join("session-start.py"))
    );
    let session_end = format!(
        "python3 \"{}\"",
        normalize_for_bash_quoted_path(&scripts_dir.join("session-end.py"))
    );
    let after_agent = format!(
        "python3 \"{}\"",
        normalize_for_bash_quoted_path(&scripts_dir.join("teammate-idle-relay.py"))
    );

    if ensure_gemini_hook_command(
        &mut settings,
        "SessionStart",
        "atm-session-start",
        &session_start,
    )? == HookStatus::Added
    {
        added_any = true;
    }
    if ensure_gemini_hook_command(&mut settings, "SessionEnd", "atm-session-end", &session_end)?
        == HookStatus::Added
    {
        added_any = true;
    }
    if ensure_gemini_hook_command(&mut settings, "AfterAgent", "atm-after-agent", &after_agent)?
        == HookStatus::Added
    {
        added_any = true;
    }

    if !added_any {
        return Ok(RuntimeInstallStatus::AlreadyConfigured);
    }
    Ok(if existed {
        RuntimeInstallStatus::Updated
    } else {
        RuntimeInstallStatus::Installed
    })
}

fn normalize_path_for_runtime_config(path: &Path) -> String {
    #[cfg(windows)]
    {
        path.to_string_lossy().replace('\\', "/")
    }
    #[cfg(not(windows))]
    {
        path.to_string_lossy().to_string()
    }
}

fn ensure_gemini_hook_command(
    settings: &mut serde_json::Value,
    category: &str,
    name: &str,
    command: &str,
) -> Result<HookStatus> {
    let root = settings
        .as_object_mut()
        .context("Gemini settings root is not a JSON object")?;
    let hooks_entry = root
        .entry("hooks".to_string())
        .or_insert_with(|| serde_json::json!({}));
    let hooks_obj = hooks_entry
        .as_object_mut()
        .context("Gemini settings `hooks` is not a JSON object")?;
    let category_entry = hooks_obj
        .entry(category.to_string())
        .or_insert_with(|| serde_json::json!([]));
    let category_array = category_entry
        .as_array_mut()
        .with_context(|| format!("Gemini hooks.{category} is not an array"))?;

    for definition in category_array.iter() {
        if let Some(inner_hooks) = definition.get("hooks").and_then(|h| h.as_array())
            && hook_command_present(inner_hooks, command)
        {
            return Ok(HookStatus::AlreadyPresent);
        }
    }

    category_array.push(serde_json::json!({
        "hooks": [
            {
                "type": "command",
                "name": name,
                "command": command
            }
        ]
    }));
    Ok(HookStatus::Added)
}

fn ensure_atm_toml(path: &Path, team: &str, identity: &str) -> Result<AtmTomlStatus> {
    if path.exists() {
        return Ok(AtmTomlStatus::AlreadyPresent);
    }
    let content = format!(
        "[core]\ndefault_team = {:?}\nidentity = {:?}\n",
        team, identity
    );
    write_text_atomic(path, &content)?;
    Ok(AtmTomlStatus::Created)
}

fn ensure_compose_bootstrap(repo_root: &Path) -> Result<()> {
    let prompts_dir = repo_root.join(".prompts");
    std::fs::create_dir_all(&prompts_dir)
        .with_context(|| format!("Failed to create {}", prompts_dir.display()))?;

    ensure_gitignore_entry(&repo_root.join(".gitignore"), ".prompts/")?;
    Ok(())
}

fn ensure_gitignore_entry(path: &Path, entry: &str) -> Result<()> {
    let mut content = if path.exists() {
        std::fs::read_to_string(path)
            .with_context(|| format!("Failed to read {}", path.display()))?
    } else {
        String::new()
    };

    if content.lines().any(|line| line.trim() == entry) {
        return Ok(());
    }

    if !content.is_empty() && !content.ends_with('\n') {
        content.push('\n');
    }
    content.push_str(entry);
    content.push('\n');
    write_text_atomic(path, &content)
}

fn ensure_team_config(config_home: &Path, team: &str, cwd: &Path) -> Result<TeamStatus> {
    let team_dir = crate::util::settings::config_team_dir_for(config_home, team);
    let inboxes_dir = team_dir.join("inboxes");
    let config_path = team_dir.join("config.json");

    if config_path.exists() {
        if !inboxes_dir.exists() {
            std::fs::create_dir_all(&inboxes_dir)
                .with_context(|| format!("Failed to create {}", inboxes_dir.display()))?;
        }
        return Ok(TeamStatus::AlreadyPresent);
    }

    std::fs::create_dir_all(&inboxes_dir)
        .with_context(|| format!("Failed to create {}", inboxes_dir.display()))?;

    let now_ms = chrono::Utc::now().timestamp_millis() as u64;
    let lead_member = AgentMember {
        agent_id: format!("team-lead@{team}"),
        name: "team-lead".to_string(),
        agent_type: "general-purpose".to_string(),
        model: "unknown".to_string(),
        prompt: None,
        color: None,
        plan_mode_required: None,
        joined_at: now_ms,
        tmux_pane_id: None,
        cwd: cwd.to_string_lossy().to_string(),
        subscriptions: Vec::new(),
        backend_type: None,
        is_active: Some(false),
        last_active: None,
        session_id: None,
        external_backend_type: None,
        external_model: None,
        unknown_fields: HashMap::new(),
    };

    let team_config = TeamConfig {
        name: team.to_string(),
        description: Some("Team initialized by atm init".to_string()),
        created_at: now_ms,
        lead_agent_id: format!("team-lead@{team}"),
        lead_session_id: String::new(),
        members: vec![lead_member],
        unknown_fields: HashMap::new(),
    };

    TeamConfigStore::open(&team_dir)
        .create_or_update(|| team_config.clone(), |config| Ok(Some(config)))
        .with_context(|| format!("Failed to initialize {}", config_path.display()))?;
    Ok(TeamStatus::Created)
}

fn write_text_atomic(path: &Path, content: &str) -> Result<()> {
    if let Some(parent) = path.parent()
        && !parent.as_os_str().is_empty()
    {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("Failed to create directory {}", parent.display()))?;
    }
    let tmp_path = path.with_extension("tmp");
    std::fs::write(&tmp_path, content.as_bytes())
        .with_context(|| format!("Failed to write temp file {}", tmp_path.display()))?;
    std::fs::rename(&tmp_path, path).with_context(|| {
        format!(
            "Failed to rename {} to {}",
            tmp_path.display(),
            path.display()
        )
    })?;
    Ok(())
}

// ---------------------------------------------------------------------------
// Script materialization
// ---------------------------------------------------------------------------

/// Write embedded hook scripts to `scripts_dir`, creating directories as needed.
///
/// Each script is only written when its content differs from the file already
/// on disk, making this operation idempotent. Writes are atomic (temp + rename).
///
/// # Errors
///
/// Returns an error when the directory cannot be created or a file cannot be
/// written.
fn materialize_scripts(scripts_dir: &Path) -> Result<()> {
    use std::fs;
    fs::create_dir_all(scripts_dir).with_context(|| {
        format!(
            "Failed to create scripts directory {}",
            scripts_dir.display()
        )
    })?;

    let files = [
        ("session-start.py", SESSION_START_PY),
        ("session-end.py", SESSION_END_PY),
        ("teammate-idle-relay.py", TEAMMATE_IDLE_RELAY_PY),
        ("permission-request-relay.py", PERMISSION_REQUEST_RELAY_PY),
        ("stop-relay.py", STOP_RELAY_PY),
        ("notification-idle-relay.py", NOTIFICATION_IDLE_RELAY_PY),
        ("atm-identity-write.py", ATM_IDENTITY_WRITE_PY),
        ("atm-identity-cleanup.py", ATM_IDENTITY_CLEANUP_PY),
        ("gate-agent-spawns.py", GATE_AGENT_SPAWNS_PY),
        ("atm_hook_lib.py", ATM_HOOK_LIB_PY),
        ("atm-hook-relay.py", ATM_HOOK_RELAY_PY),
    ];

    for (name, content) in &files {
        let path = scripts_dir.join(name);
        // Only write when content differs (idempotency)
        let existing = fs::read_to_string(&path).unwrap_or_default();
        if existing != *content {
            // Atomic write: temp + rename
            let tmp = path.with_extension("py.tmp");
            fs::write(&tmp, content)
                .with_context(|| format!("Failed to write temp script {}", tmp.display()))?;
            fs::rename(&tmp, &path).with_context(|| {
                format!("Failed to rename {} to {}", tmp.display(), path.display())
            })?;
        }
    }

    Ok(())
}

// ---------------------------------------------------------------------------
// Path resolution
// ---------------------------------------------------------------------------

/// Resolve the path to `settings.json` based on the `--global` flag.
///
/// - Global: `~/.claude/settings.json`
/// - Local:  `{cwd}/.claude/settings.json`
///
/// # Errors
///
/// Returns an error when `--global` is requested and the home directory
/// cannot be determined.
fn resolve_settings_path(global: bool) -> Result<PathBuf> {
    if global {
        let home = crate::util::settings::get_home_dir()
            .context("Cannot resolve home directory for global settings")?;
        Ok(crate::util::settings::config_claude_root_dir_for(&home).join("settings.json"))
    } else {
        let cwd = std::env::current_dir().context("Cannot determine current directory")?;
        Ok(cwd.join(".claude").join("settings.json"))
    }
}

// ---------------------------------------------------------------------------
// Settings load / write
// ---------------------------------------------------------------------------

/// Load `settings.json` as a `serde_json::Value`.
///
/// Returns an empty JSON object when the file does not exist yet.
///
/// # Errors
///
/// Returns an error when the file exists but cannot be read or parsed as JSON.
fn load_settings(path: &Path) -> Result<serde_json::Value> {
    if !path.exists() {
        return Ok(serde_json::json!({}));
    }
    let content = std::fs::read_to_string(path)
        .with_context(|| format!("Failed to read {}", path.display()))?;
    serde_json::from_str(&content)
        .with_context(|| format!("Failed to parse {} as JSON", path.display()))
}

/// Write `settings` to `path` atomically.
///
/// Writes to `{path}.tmp` first, then renames to `path`. Creates parent
/// directories as needed.
///
/// # Errors
///
/// Returns an error when parent-directory creation, JSON serialization,
/// temp-file write, or the atomic rename fails.
fn write_settings_atomic(path: &Path, settings: &serde_json::Value) -> Result<()> {
    if let Some(parent) = path.parent() {
        if !parent.as_os_str().is_empty() {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("Failed to create directory {}", parent.display()))?;
        }
    }

    let mut serialized =
        serde_json::to_string_pretty(settings).context("Failed to serialize settings as JSON")?;
    serialized.push('\n');

    let tmp_path = path.with_extension("json.tmp");
    std::fs::write(&tmp_path, serialized.as_bytes())
        .with_context(|| format!("Failed to write temp file {}", tmp_path.display()))?;

    std::fs::rename(&tmp_path, path).with_context(|| {
        format!(
            "Failed to rename {} to {}",
            tmp_path.display(),
            path.display()
        )
    })?;

    Ok(())
}

// ---------------------------------------------------------------------------
// Hook merge logic
// ---------------------------------------------------------------------------

/// Outcome of attempting to install a single hook entry.
#[derive(Debug, PartialEq, Eq)]
enum HookStatus {
    /// The hook was not present and has been added.
    Added,
    /// The hook was already present; no change was made.
    AlreadyPresent,
}

/// Summary of what changed (or was already present) during a merge.
struct MergeReport {
    session_start: HookStatus,
    session_end: HookStatus,
    permission_request: HookStatus,
    stop: HookStatus,
    notification_idle_prompt: HookStatus,
    pre_tool_use_bash: HookStatus,
    pre_tool_use_task: HookStatus,
    post_tool_use_bash: HookStatus,
}

impl MergeReport {
    fn all_present(&self) -> bool {
        self.session_start == HookStatus::AlreadyPresent
            && self.session_end == HookStatus::AlreadyPresent
            && self.permission_request == HookStatus::AlreadyPresent
            && self.stop == HookStatus::AlreadyPresent
            && self.notification_idle_prompt == HookStatus::AlreadyPresent
            && self.pre_tool_use_bash == HookStatus::AlreadyPresent
            && self.pre_tool_use_task == HookStatus::AlreadyPresent
            && self.post_tool_use_bash == HookStatus::AlreadyPresent
    }

    fn any_added(&self) -> bool {
        self.session_start == HookStatus::Added
            || self.session_end == HookStatus::Added
            || self.permission_request == HookStatus::Added
            || self.stop == HookStatus::Added
            || self.notification_idle_prompt == HookStatus::Added
            || self.pre_tool_use_bash == HookStatus::Added
            || self.pre_tool_use_task == HookStatus::Added
            || self.post_tool_use_bash == HookStatus::Added
    }

    fn all_added(&self) -> bool {
        self.session_start == HookStatus::Added
            && self.session_end == HookStatus::Added
            && self.permission_request == HookStatus::Added
            && self.stop == HookStatus::Added
            && self.notification_idle_prompt == HookStatus::Added
            && self.pre_tool_use_bash == HookStatus::Added
            && self.pre_tool_use_task == HookStatus::Added
            && self.post_tool_use_bash == HookStatus::Added
    }
}

/// Merge ATM hooks into `settings` and return a report.
///
/// Uses idempotency checks by matching on the exact command string so that
/// re-running `atm init` never duplicates entries.
///
/// # Errors
///
/// Returns an error when the JSON structure is malformed in a way that
/// prevents safe merging (e.g. `hooks` key exists but is not an object).
fn merge_hooks(
    settings: &mut serde_json::Value,
    global_scripts_dir: Option<&Path>,
) -> Result<MergeReport> {
    // Ensure `hooks` key is a JSON object.
    {
        let obj = settings
            .as_object_mut()
            .context("settings.json root is not a JSON object")?;
        let hooks_entry = obj.entry("hooks").or_insert_with(|| serde_json::json!({}));
        if !hooks_entry.is_object() {
            anyhow::bail!(
                "settings.json `hooks` field exists but is not a JSON object; refusing to overwrite"
            );
        }
    }

    // Migrate existing SessionStart/SessionEnd entries to Claude's current
    // nested hook schema when present.
    normalize_catch_all_hook_category_if_present(settings, "SessionStart")?;
    normalize_catch_all_hook_category_if_present(settings, "SessionEnd")?;
    normalize_catch_all_hook_category_if_present(settings, "PermissionRequest")?;
    normalize_catch_all_hook_category_if_present(settings, "Stop")?;

    let ss_cmd = session_start_cmd(global_scripts_dir);
    let se_cmd = session_end_cmd(global_scripts_dir);
    let pr_cmd = permission_request_cmd(global_scripts_dir);
    let stop = stop_cmd(global_scripts_dir);
    let notify_idle = notification_idle_prompt_cmd(global_scripts_dir);
    let ptu_bash = pre_tool_use_bash_cmd(global_scripts_dir);
    let ptu_task = pre_tool_use_task_cmd(global_scripts_dir);
    let post_bash = post_tool_use_bash_cmd(global_scripts_dir);

    let session_start = merge_session_hook(settings, "SessionStart", &ss_cmd)?;
    let session_end = merge_session_hook(settings, "SessionEnd", &se_cmd)?;
    let permission_request = merge_session_hook(settings, "PermissionRequest", &pr_cmd)?;
    let stop = merge_session_hook(settings, "Stop", &stop)?;
    let notification_idle_prompt =
        merge_matcher_hook(settings, "Notification", "idle_prompt", &notify_idle)?;
    let pre_tool_use_bash = merge_matcher_hook(settings, "PreToolUse", "Bash", &ptu_bash)?;
    let pre_tool_use_task = merge_matcher_hook(settings, "PreToolUse", "Task", &ptu_task)?;
    let post_tool_use_bash = merge_matcher_hook(settings, "PostToolUse", "Bash", &post_bash)?;

    Ok(MergeReport {
        session_start,
        session_end,
        permission_request,
        stop,
        notification_idle_prompt,
        pre_tool_use_bash,
        pre_tool_use_task,
        post_tool_use_bash,
    })
}

/// Merge a single hook entry into a `hooks.SessionStart` or `hooks.SessionEnd` array.
///
/// Writes new entries in the nested hook schema (no `matcher` field):
/// `{ "hooks": [{ "type": "command", "command": "..." }] }`.
///
/// Detects existing entries in both legacy catch-all format (with `matcher: ""`)
/// and the new nested format (without `matcher`), so re-running `atm init` after
/// a migration is idempotent.
fn merge_session_hook(
    settings: &mut serde_json::Value,
    category: &str,
    command: &str,
) -> Result<HookStatus> {
    let array = get_or_create_hook_array(settings, category)?;

    // Check for presence in either format (legacy catch-all or new nested).
    for entry in array.iter() {
        if let Some(hooks) = entry.get("hooks").and_then(|h| h.as_array()) {
            if hook_command_present(hooks, command) {
                return Ok(HookStatus::AlreadyPresent);
            }
        }
    }

    let new_entry = serde_json::json!({
        "hooks": [{
            "type": "command",
            "command": command
        }]
    });
    array.push(new_entry);
    Ok(HookStatus::Added)
}

/// Merge a single hook entry into a `PreToolUse` or `PostToolUse` matcher object.
///
/// The `PreToolUse`/`PostToolUse` arrays hold matcher objects of the form:
/// ```json
/// { "matcher": "Bash", "hooks": [ { "type": "command", "command": "..." } ] }
/// ```
/// This function finds the object whose `matcher` equals `matcher_name`,
/// appends the hook entry when not already present, or creates a new matcher
/// object if none exists.
fn merge_matcher_hook(
    settings: &mut serde_json::Value,
    hook_category: &str,
    matcher_name: &str,
    command: &str,
) -> Result<HookStatus> {
    let new_hook_entry = command_hook_entry(command);

    let category_array = get_or_create_hook_array(settings, hook_category)?;

    // Find existing matcher object index.
    let existing_idx = category_array.iter().position(|entry| {
        entry
            .get("matcher")
            .and_then(|m| m.as_str())
            .map(|m| m == matcher_name)
            .unwrap_or(false)
    });

    if let Some(idx) = existing_idx {
        // Matcher object exists — check and possibly add to its `hooks` array.
        let matcher_obj = &mut category_array[idx];

        // Ensure `hooks` sub-array exists.
        if matcher_obj.get("hooks").is_none() {
            matcher_obj
                .as_object_mut()
                .context("matcher entry is not an object")?
                .insert("hooks".to_string(), serde_json::json!([]));
        }

        let hooks_array = matcher_obj
            .get_mut("hooks")
            .and_then(|h| h.as_array_mut())
            .context("matcher `hooks` field is not an array")?;

        if hook_command_present(hooks_array, command) {
            return Ok(HookStatus::AlreadyPresent);
        }

        hooks_array.push(new_hook_entry);
        Ok(HookStatus::Added)
    } else {
        // No matcher object for this name — append a new one.
        let new_matcher = serde_json::json!({
            "matcher": matcher_name,
            "hooks": [new_hook_entry]
        });
        category_array.push(new_matcher);
        Ok(HookStatus::Added)
    }
}

/// Return a mutable reference to `settings["hooks"][category]` as a JSON array,
/// creating missing intermediate objects and the array itself if absent.
///
/// # Errors
///
/// Returns an error when `hooks[category]` already exists but is not an array.
fn get_or_create_hook_array<'a>(
    settings: &'a mut serde_json::Value,
    category: &str,
) -> Result<&'a mut Vec<serde_json::Value>> {
    let hooks = settings
        .as_object_mut()
        .context("settings root is not an object")?
        .get_mut("hooks")
        .and_then(|h| h.as_object_mut())
        .context("settings `hooks` is not an object")?;

    let entry = hooks
        .entry(category.to_string())
        .or_insert_with(|| serde_json::json!([]));

    entry
        .as_array_mut()
        .with_context(|| format!("hooks.{category} is not an array"))
}

fn command_hook_entry(command: &str) -> serde_json::Value {
    serde_json::json!({
        "type": "command",
        "command": command
    })
}

fn catch_all_hook_entry(command: &str) -> serde_json::Value {
    serde_json::json!({
        "matcher": "",
        "hooks": [command_hook_entry(command)]
    })
}

fn normalize_catch_all_hook_entries(array: &mut [serde_json::Value]) -> Result<()> {
    for entry in array.iter_mut() {
        let Some(obj) = entry.as_object_mut() else {
            continue;
        };

        if let Some(command) = obj
            .get("command")
            .and_then(|c| c.as_str())
            .map(|s| s.to_string())
        {
            *entry = catch_all_hook_entry(&command);
            continue;
        }

        if let Some(hooks) = obj.get_mut("hooks") {
            hooks
                .as_array_mut()
                .context("catch-all hook `hooks` field is not an array")?;
        }
    }

    Ok(())
}

fn normalize_catch_all_hook_category_if_present(
    settings: &mut serde_json::Value,
    category: &str,
) -> Result<()> {
    let hooks = settings
        .as_object_mut()
        .context("settings root is not an object")?
        .get_mut("hooks")
        .and_then(|h| h.as_object_mut())
        .context("settings `hooks` is not an object")?;

    let Some(existing) = hooks.get_mut(category) else {
        return Ok(());
    };

    let array = existing
        .as_array_mut()
        .with_context(|| format!("hooks.{category} is not an array"))?;
    normalize_catch_all_hook_entries(array)
}

/// Return `true` when any catch-all hook entry contains a nested command equal to `cmd`.
pub(crate) fn catch_all_hook_command_present(array: &[serde_json::Value], cmd: &str) -> bool {
    array.iter().any(|entry| {
        entry
            .get("hooks")
            .and_then(|h| h.as_array())
            .map(|hooks| hook_command_present(hooks, cmd))
            .unwrap_or(false)
    })
}

/// Return `true` when any entry in `array` has a `command` field equal to `cmd`.
pub(crate) fn hook_command_present(array: &[serde_json::Value], cmd: &str) -> bool {
    array.iter().any(|entry| {
        entry
            .get("command")
            .and_then(|c| c.as_str())
            .map(|c| c == cmd)
            .unwrap_or(false)
    })
}

// ---------------------------------------------------------------------------
// Output
// ---------------------------------------------------------------------------

#[allow(clippy::too_many_arguments)]
fn print_report(
    team: &str,
    settings_path: &Path,
    report: &MergeReport,
    install_global: bool,
    atm_toml_path: &Path,
    atm_toml_status: AtmTomlStatus,
    team_status: TeamStatus,
    dry_run: bool,
    runtime_reports: &[RuntimeInstallReport],
) {
    match atm_toml_status {
        AtmTomlStatus::Created => {
            println!("Created .atm.toml at {}", atm_toml_path.display());
        }
        AtmTomlStatus::AlreadyPresent => {
            println!(".atm.toml already present at {}", atm_toml_path.display());
        }
        AtmTomlStatus::WouldCreate => {
            println!("Would create .atm.toml at {}", atm_toml_path.display());
        }
    }

    match team_status {
        TeamStatus::Created => println!("Created team '{}'", team),
        TeamStatus::AlreadyPresent => println!("Team '{}' already exists", team),
        TeamStatus::WouldCreate => println!("Would create team '{}'", team),
        TeamStatus::Skipped => println!("Skipped team creation (--skip-team)"),
    }

    if report.all_present() {
        println!(
            "ATM hooks already configured for team '{}' in {}",
            team,
            settings_path.display()
        );
        println!("  \u{2713} SessionStart hook present");
        println!("  \u{2713} SessionEnd hook present");
        println!("  \u{2713} PermissionRequest hook present");
        println!("  \u{2713} Stop hook present");
        println!("  \u{2713} Notification(idle_prompt) hook present");
        println!("  \u{2713} PreToolUse(Bash) hook present");
        println!("  \u{2713} PreToolUse(Task) hook present");
        println!("  \u{2713} PostToolUse(Bash) hook present");
    } else if report.all_added() {
        if dry_run {
            println!(
                "Would install ATM hooks for team '{}' in {}",
                team,
                settings_path.display()
            );
        } else {
            println!(
                "Installed ATM hooks for team '{}' in {}",
                team,
                settings_path.display()
            );
        }
        print_hook_line("SessionStart hook", &report.session_start, dry_run);
        print_hook_line("SessionEnd hook", &report.session_end, dry_run);
        print_hook_line(
            "PermissionRequest hook",
            &report.permission_request,
            dry_run,
        );
        print_hook_line("Stop hook", &report.stop, dry_run);
        print_hook_line(
            "Notification(idle_prompt) hook",
            &report.notification_idle_prompt,
            dry_run,
        );
        print_hook_line("PreToolUse(Bash) hook", &report.pre_tool_use_bash, dry_run);
        print_hook_line("PreToolUse(Task) hook", &report.pre_tool_use_task, dry_run);
        print_hook_line(
            "PostToolUse(Bash) hook",
            &report.post_tool_use_bash,
            dry_run,
        );
    } else if report.any_added() {
        if dry_run {
            println!(
                "Would update ATM hooks for team '{}' in {}",
                team,
                settings_path.display()
            );
        } else {
            println!(
                "Updated ATM hooks for team '{}' in {}",
                team,
                settings_path.display()
            );
        }
        print_hook_line("SessionStart hook", &report.session_start, dry_run);
        print_hook_line("SessionEnd hook", &report.session_end, dry_run);
        print_hook_line(
            "PermissionRequest hook",
            &report.permission_request,
            dry_run,
        );
        print_hook_line("Stop hook", &report.stop, dry_run);
        print_hook_line(
            "Notification(idle_prompt) hook",
            &report.notification_idle_prompt,
            dry_run,
        );
        print_hook_line("PreToolUse(Bash) hook", &report.pre_tool_use_bash, dry_run);
        print_hook_line("PreToolUse(Task) hook", &report.pre_tool_use_task, dry_run);
        print_hook_line(
            "PostToolUse(Bash) hook",
            &report.post_tool_use_bash,
            dry_run,
        );
    }

    println!(
        "Hook scope: {}",
        if install_global { "global" } else { "local" }
    );
    if dry_run {
        println!("Dry run: no files were written.");
    }
    println!("Runtime installs:");
    for runtime in runtime_reports {
        let status = if dry_run {
            match runtime.status {
                RuntimeInstallStatus::Installed => "would-install",
                RuntimeInstallStatus::Updated => "would-update",
                RuntimeInstallStatus::AlreadyConfigured => "already-configured",
                RuntimeInstallStatus::SkippedNotDetected => "skipped-not-detected",
                RuntimeInstallStatus::Error => "error",
            }
        } else {
            runtime.status.as_str()
        };
        match (&runtime.detail, &runtime.path) {
            (Some(detail), Some(path)) => {
                println!("  - {}: {} ({})", runtime.runtime, status, path.display());
                println!("    remediation: {detail}");
            }
            (Some(detail), None) => {
                println!("  - {}: {}", runtime.runtime, status);
                println!("    remediation: {detail}");
            }
            (None, Some(path)) => {
                println!("  - {}: {} ({})", runtime.runtime, status, path.display());
            }
            (None, None) => {
                println!("  - {}: {}", runtime.runtime, status);
            }
        }
    }
}

fn print_hook_line(label: &str, status: &HookStatus, dry_run: bool) {
    match status {
        HookStatus::Added => {
            if dry_run {
                println!("  + {label} would be added");
            } else {
                println!("  + {label} added");
            }
        }
        HookStatus::AlreadyPresent => println!("  \u{2713} {label} present"),
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use serial_test::serial;
    use std::env;
    use std::ffi::{OsStr, OsString};
    use tempfile::TempDir;

    fn entry_uses_session_hook_schema(entry: &serde_json::Value) -> bool {
        if entry.get("hooks").and_then(|h| h.as_array()).is_none() {
            return false;
        }
        entry
            .get("matcher")
            .and_then(|m| m.as_str())
            .map(|m| m.is_empty())
            .unwrap_or(true)
    }

    // -----------------------------------------------------------------------
    // Helper: build a settings.json path inside a TempDir
    // -----------------------------------------------------------------------
    fn temp_settings(dir: &TempDir) -> PathBuf {
        dir.path().join(".claude").join("settings.json")
    }

    struct EnvVarGuard {
        key: &'static str,
        original: Option<OsString>,
    }

    impl EnvVarGuard {
        fn set_path(key: &'static str, value: &Path) -> Self {
            let guard = Self {
                key,
                original: env::vars_os().find_map(|(current_key, current_value)| {
                    (current_key == OsStr::new(key)).then_some(current_value)
                }),
            };

            // SAFETY: serialized tests own process env mutations.
            unsafe {
                env::set_var(key, value);
            }

            guard
        }
    }

    impl Drop for EnvVarGuard {
        fn drop(&mut self) {
            // SAFETY: serialized tests own process env mutations.
            unsafe {
                match &self.original {
                    Some(value) => env::set_var(self.key, value),
                    None => env::remove_var(self.key),
                }
            }
        }
    }

    // -----------------------------------------------------------------------
    // Fresh file test
    // -----------------------------------------------------------------------

    /// Installing into a nonexistent settings.json creates the file with
    /// core ATM hooks correctly structured.
    #[test]
    #[serial]
    fn test_fresh_file_install() {
        let dir = TempDir::new().expect("tempdir");
        let path = temp_settings(&dir);

        // File must not exist yet
        assert!(!path.exists());

        let mut settings = load_settings(&path).expect("load");
        let report = merge_hooks(&mut settings, None).expect("merge");
        write_settings_atomic(&path, &settings).expect("write");

        assert!(path.exists());
        assert_eq!(report.session_start, HookStatus::Added);
        assert_eq!(report.permission_request, HookStatus::Added);
        assert_eq!(report.stop, HookStatus::Added);
        assert_eq!(report.notification_idle_prompt, HookStatus::Added);
        assert_eq!(report.pre_tool_use_bash, HookStatus::Added);
        assert_eq!(report.pre_tool_use_task, HookStatus::Added);
        assert_eq!(report.post_tool_use_bash, HookStatus::Added);

        // Verify structural correctness of written file
        let content = std::fs::read_to_string(&path).expect("read");
        assert!(content.ends_with('\n'), "file must end with newline");

        let parsed: serde_json::Value = serde_json::from_str(&content).expect("parse");
        let session_start = parsed["hooks"]["SessionStart"]
            .as_array()
            .expect("SessionStart array");
        assert!(
            catch_all_hook_command_present(session_start, &session_start_cmd(None)),
            "SessionStart hook missing"
        );
        assert!(
            session_start.iter().all(entry_uses_session_hook_schema),
            "SessionStart entries must be nested hooks and only use empty-string matcher when present"
        );

        let pre_tool_use = parsed["hooks"]["PreToolUse"]
            .as_array()
            .expect("PreToolUse array");
        let bash_matcher = pre_tool_use
            .iter()
            .find(|e| e.get("matcher").and_then(|m| m.as_str()) == Some("Bash"))
            .expect("Bash matcher");
        let bash_hooks = bash_matcher["hooks"].as_array().expect("hooks array");
        assert!(hook_command_present(
            bash_hooks,
            &pre_tool_use_bash_cmd(None)
        ));

        let task_matcher = pre_tool_use
            .iter()
            .find(|e| e.get("matcher").and_then(|m| m.as_str()) == Some("Task"))
            .expect("Task matcher");
        let task_hooks = task_matcher["hooks"].as_array().expect("hooks array");
        assert!(hook_command_present(
            task_hooks,
            &pre_tool_use_task_cmd(None)
        ));

        let post_tool_use = parsed["hooks"]["PostToolUse"]
            .as_array()
            .expect("PostToolUse array");
        let post_bash = post_tool_use
            .iter()
            .find(|e| e.get("matcher").and_then(|m| m.as_str()) == Some("Bash"))
            .expect("PostToolUse Bash matcher");
        let post_hooks = post_bash["hooks"].as_array().expect("hooks array");
        assert!(hook_command_present(
            post_hooks,
            &post_tool_use_bash_cmd(None)
        ));
    }

    // -----------------------------------------------------------------------
    // Idempotency test
    // -----------------------------------------------------------------------

    /// Running install twice on the same settings.json must not duplicate hooks.
    #[test]
    #[serial]
    fn test_idempotent_double_install() {
        let dir = TempDir::new().expect("tempdir");
        let path = temp_settings(&dir);

        // First install
        let mut settings = load_settings(&path).expect("load 1");
        merge_hooks(&mut settings, None).expect("merge 1");
        write_settings_atomic(&path, &settings).expect("write 1");

        // Second install on the freshly written file
        let mut settings2 = load_settings(&path).expect("load 2");
        let report = merge_hooks(&mut settings2, None).expect("merge 2");
        write_settings_atomic(&path, &settings2).expect("write 2");

        // All hooks must be reported as already present
        assert_eq!(report.session_start, HookStatus::AlreadyPresent);
        assert_eq!(report.permission_request, HookStatus::AlreadyPresent);
        assert_eq!(report.stop, HookStatus::AlreadyPresent);
        assert_eq!(report.notification_idle_prompt, HookStatus::AlreadyPresent);
        assert_eq!(report.pre_tool_use_bash, HookStatus::AlreadyPresent);
        assert_eq!(report.pre_tool_use_task, HookStatus::AlreadyPresent);
        assert_eq!(report.post_tool_use_bash, HookStatus::AlreadyPresent);

        // Verify no duplicate entries in the file
        let content = std::fs::read_to_string(&path).expect("read");
        let parsed: serde_json::Value = serde_json::from_str(&content).expect("parse");

        let session_start_count = parsed["hooks"]["SessionStart"]
            .as_array()
            .expect("array")
            .iter()
            .filter(|e| {
                e.get("hooks")
                    .and_then(|h| h.as_array())
                    .map(|hooks| hook_command_present(hooks, &session_start_cmd(None)))
                    .unwrap_or(false)
            })
            .count();
        assert_eq!(session_start_count, 1, "SessionStart hook duplicated");

        let pre_tool_use = parsed["hooks"]["PreToolUse"].as_array().expect("array");
        let bash_matcher_count = pre_tool_use
            .iter()
            .filter(|e| e.get("matcher").and_then(|m| m.as_str()) == Some("Bash"))
            .count();
        assert_eq!(bash_matcher_count, 1, "Bash matcher duplicated");

        let bash_hooks = pre_tool_use
            .iter()
            .find(|e| e.get("matcher").and_then(|m| m.as_str()) == Some("Bash"))
            .unwrap()["hooks"]
            .as_array()
            .expect("hooks");
        let bash_hook_count = bash_hooks
            .iter()
            .filter(|e| {
                e.get("command")
                    .and_then(|c| c.as_str())
                    .map(|c| c == pre_tool_use_bash_cmd(None))
                    .unwrap_or(false)
            })
            .count();
        assert_eq!(bash_hook_count, 1, "PreToolUse(Bash) hook duplicated");
    }

    // -----------------------------------------------------------------------
    // Merge / preservation test
    // -----------------------------------------------------------------------

    /// Pre-existing non-ATM hooks must be preserved after install.
    #[test]
    #[serial]
    fn test_preserves_existing_non_atm_hooks() {
        let dir = TempDir::new().expect("tempdir");
        let path = temp_settings(&dir);

        // Write a settings.json with a pre-existing PreToolUse hook for another tool
        let initial = serde_json::json!({
            "someOtherSetting": "value",
            "hooks": {
                "PreToolUse": [
                    {
                        "matcher": "Bash",
                        "hooks": [
                            {
                                "type": "command",
                                "command": "echo 'existing-hook'"
                            }
                        ]
                    }
                ]
            }
        });
        std::fs::create_dir_all(path.parent().unwrap()).expect("mkdir");
        let mut content = serde_json::to_string_pretty(&initial).expect("serialize");
        content.push('\n');
        std::fs::write(&path, content.as_bytes()).expect("write initial");

        // Install ATM hooks
        let mut settings = load_settings(&path).expect("load");
        merge_hooks(&mut settings, None).expect("merge");
        write_settings_atomic(&path, &settings).expect("write");

        let result_content = std::fs::read_to_string(&path).expect("read");
        let parsed: serde_json::Value = serde_json::from_str(&result_content).expect("parse");

        // The unrelated setting must still be present
        assert_eq!(
            parsed.get("someOtherSetting").and_then(|v| v.as_str()),
            Some("value"),
            "someOtherSetting was lost"
        );

        // The pre-existing non-ATM hook must still be present in the Bash matcher
        let pre_tool_use = parsed["hooks"]["PreToolUse"].as_array().expect("array");
        let bash_hooks = pre_tool_use
            .iter()
            .find(|e| e.get("matcher").and_then(|m| m.as_str()) == Some("Bash"))
            .expect("Bash matcher")["hooks"]
            .as_array()
            .expect("hooks");

        let existing_preserved = bash_hooks.iter().any(|e| {
            e.get("command")
                .and_then(|c| c.as_str())
                .map(|c| c == "echo 'existing-hook'")
                .unwrap_or(false)
        });
        assert!(existing_preserved, "pre-existing non-ATM hook was removed");

        // ATM hook must also be present
        assert!(
            hook_command_present(bash_hooks, &pre_tool_use_bash_cmd(None)),
            "ATM PreToolUse(Bash) hook missing after merge"
        );
    }

    #[test]
    #[serial]
    fn test_migrates_legacy_catch_all_hook_entries_to_matcher_schema() {
        let mut settings = serde_json::json!({
            "hooks": {
                "SessionStart": [
                    {
                        "hooks": [
                            {
                                "type": "command",
                                "command": session_start_cmd(None)
                            }
                        ]
                    },
                    {
                        "type": "command",
                        "command": "python3 ~/.claude/scripts/legacy-session-start.py"
                    }
                ],
                "SessionEnd": [
                    {
                        "hooks": [
                            {
                                "type": "command",
                                "command": "python3 ~/.claude/scripts/session-end.py"
                            }
                        ]
                    }
                ]
            }
        });

        let report = merge_hooks(&mut settings, None).expect("merge");
        assert_eq!(report.session_start, HookStatus::AlreadyPresent);

        let session_start = settings["hooks"]["SessionStart"]
            .as_array()
            .expect("SessionStart array");
        assert!(
            catch_all_hook_command_present(session_start, &session_start_cmd(None)),
            "SessionStart ATM command must still be present after migration"
        );
        assert!(
            session_start.iter().all(entry_uses_session_hook_schema),
            "all SessionStart entries must be nested hooks and only use empty-string matcher when present"
        );
        assert!(
            session_start.iter().all(|e| e.get("hooks").is_some()),
            "legacy bare SessionStart entries must be wrapped after migration"
        );

        let session_end = settings["hooks"]["SessionEnd"]
            .as_array()
            .expect("SessionEnd array");
        assert!(
            session_end.iter().all(entry_uses_session_hook_schema),
            "all SessionEnd entries must be nested hooks and only use empty-string matcher when present"
        );
        assert!(
            session_end.iter().all(|e| e.get("hooks").is_some()),
            "legacy SessionEnd entries must remain wrapped after migration"
        );
    }

    #[test]
    fn test_normalize_catch_all_hook_entries_preserves_non_catch_all_matcher() {
        let mut entries = vec![serde_json::json!({
            "matcher": "Bash",
            "hooks": [
                {
                    "type": "command",
                    "command": "echo existing"
                }
            ]
        })];

        normalize_catch_all_hook_entries(&mut entries).expect("normalize");

        assert_eq!(
            entries[0].get("matcher").and_then(|m| m.as_str()),
            Some("Bash"),
            "non-catch-all matcher must not be overwritten"
        );
    }

    // -----------------------------------------------------------------------
    // Path resolution tests
    // -----------------------------------------------------------------------

    /// Local path resolution should yield `{cwd}/.claude/settings.json`.
    #[test]
    #[serial]
    fn test_resolve_settings_path_local() {
        let path = resolve_settings_path(false).expect("resolve local");
        let cwd = env::current_dir().expect("cwd");
        assert_eq!(path, cwd.join(".claude").join("settings.json"));
    }

    /// Global path resolution must honor `ATM_HOME` when present.
    #[test]
    #[serial]
    fn test_resolve_settings_path_global_uses_os_home() {
        let dir = TempDir::new().expect("tempdir");
        let _atm_home_guard = EnvVarGuard::set_path("ATM_HOME", dir.path());

        let path = resolve_settings_path(true).expect("resolve global");
        assert_eq!(path, dir.path().join(".claude").join("settings.json"));
    }

    /// Global hook commands must use resolved absolute script paths, not
    /// `${HOME}` expansion.
    #[test]
    fn test_global_hook_commands_use_absolute_script_paths() {
        let dir = TempDir::new().expect("tempdir");
        let scripts_dir = dir.path().join("scripts with spaces");
        let expected_session =
            normalize_for_bash_quoted_path(&scripts_dir.join("session-start.py"));
        let expected_permission =
            normalize_for_bash_quoted_path(&scripts_dir.join("permission-request-relay.py"));
        let expected_stop = normalize_for_bash_quoted_path(&scripts_dir.join("stop-relay.py"));
        let expected_notification =
            normalize_for_bash_quoted_path(&scripts_dir.join("notification-idle-relay.py"));
        let expected_write =
            normalize_for_bash_quoted_path(&scripts_dir.join("atm-identity-write.py"));
        let expected_gate =
            normalize_for_bash_quoted_path(&scripts_dir.join("gate-agent-spawns.py"));
        let expected_cleanup =
            normalize_for_bash_quoted_path(&scripts_dir.join("atm-identity-cleanup.py"));

        let session = session_start_cmd(Some(&scripts_dir));
        let permission = permission_request_cmd(Some(&scripts_dir));
        let stop = stop_cmd(Some(&scripts_dir));
        let notification = notification_idle_prompt_cmd(Some(&scripts_dir));
        let write = pre_tool_use_bash_cmd(Some(&scripts_dir));
        let gate = pre_tool_use_task_cmd(Some(&scripts_dir));
        let cleanup = post_tool_use_bash_cmd(Some(&scripts_dir));

        assert!(session.contains(&expected_session));
        assert!(permission.contains(&expected_permission));
        assert!(stop.contains(&expected_stop));
        assert!(notification.contains(&expected_notification));
        assert!(write.contains(&expected_write));
        assert!(gate.contains(&expected_gate));
        assert!(cleanup.contains(&expected_cleanup));

        assert!(!session.contains("${HOME}"));
        assert!(!permission.contains("${HOME}"));
        assert!(!stop.contains("${HOME}"));
        assert!(!notification.contains("${HOME}"));
        assert!(!write.contains("${HOME}"));
        assert!(!gate.contains("${HOME}"));
        assert!(!cleanup.contains("${HOME}"));
    }

    // -----------------------------------------------------------------------
    // Execute bootstrap test
    // -----------------------------------------------------------------------

    #[test]
    #[serial]
    fn test_execute_creates_atm_toml_team_and_global_hooks() {
        let dir = TempDir::new().expect("tempdir");
        let repo_dir = dir.path().join("repo");
        std::fs::create_dir_all(&repo_dir).expect("create repo");
        let original_dir = env::current_dir().expect("original cwd");
        let _atm_home_guard = EnvVarGuard::set_path("ATM_HOME", dir.path());

        env::set_current_dir(&repo_dir).expect("set cwd");

        let result = execute(InitArgs {
            team: "atm-dev".to_string(),
            local: false,
            identity: Some("team-lead".to_string()),
            skip_team: false,
            dry_run: false,
            global: false,
        });

        env::set_current_dir(original_dir).expect("restore cwd");

        assert!(result.is_ok());
        assert!(
            repo_dir.join(".atm.toml").exists(),
            ".atm.toml should be created in repo"
        );
        assert!(
            dir.path()
                .join(".claude/teams/atm-dev/config.json")
                .exists(),
            "team config should be created under canonical HOME"
        );
        assert!(
            dir.path().join(".claude/settings.json").exists(),
            "global settings should be created by default"
        );
    }

    // -----------------------------------------------------------------------
    // Script materialization test
    // -----------------------------------------------------------------------

    /// `materialize_scripts` must create all embedded script files in the
    /// target dir.
    #[test]
    fn test_materialize_scripts_creates_all_files() {
        let dir = TempDir::new().expect("tempdir");
        let scripts_dir = dir.path().join("scripts");

        materialize_scripts(&scripts_dir).expect("materialize");

        for name in &[
            "session-start.py",
            "session-end.py",
            "teammate-idle-relay.py",
            "permission-request-relay.py",
            "stop-relay.py",
            "notification-idle-relay.py",
            "atm-identity-write.py",
            "atm-identity-cleanup.py",
            "gate-agent-spawns.py",
            "atm_hook_lib.py",
            "atm-hook-relay.py",
        ] {
            assert!(
                scripts_dir.join(name).exists(),
                "{name} was not materialized"
            );
        }
    }

    /// `materialize_scripts` must be idempotent: running twice must not fail
    /// and must produce identical content.
    #[test]
    fn test_materialize_scripts_idempotent() {
        let dir = TempDir::new().expect("tempdir");
        let scripts_dir = dir.path().join("scripts");

        materialize_scripts(&scripts_dir).expect("materialize first");
        materialize_scripts(&scripts_dir).expect("materialize second");

        let content = std::fs::read_to_string(scripts_dir.join("session-start.py"))
            .expect("read session-start.py");
        assert_eq!(content, SESSION_START_PY);
    }

    #[test]
    fn test_session_start_script_supports_env_fallback_without_atm_toml() {
        assert!(
            SESSION_START_PY.contains("ATM_TEAM"),
            "session-start.py must read ATM_TEAM as fallback context"
        );
        assert!(
            SESSION_START_PY.contains("ATM_IDENTITY"),
            "session-start.py must read ATM_IDENTITY as fallback context"
        );
        assert!(
            SESSION_START_PY
                .contains("if atm_config is None and not default_team and not identity"),
            "session-start.py must fail-open only when both repo and env context are absent"
        );
    }

    // -----------------------------------------------------------------------
    // Atomic write test
    // -----------------------------------------------------------------------

    /// `write_settings_atomic` must create parent directories and end with `\n`.
    #[test]
    fn test_write_settings_atomic_creates_parents_and_ends_with_newline() {
        let dir = TempDir::new().expect("tempdir");
        let nested = dir.path().join("a").join("b").join("settings.json");
        let value = serde_json::json!({"hooks": {}});

        write_settings_atomic(&nested, &value).expect("write");
        let content = std::fs::read_to_string(&nested).expect("read");
        assert!(content.ends_with('\n'));
        assert!(nested.exists());
    }

    // -----------------------------------------------------------------------
    // Hook presence helper
    // -----------------------------------------------------------------------

    #[test]
    fn test_hook_command_present_true_and_false() {
        let array = vec![
            serde_json::json!({"type": "command", "command": "echo hello"}),
            serde_json::json!({"type": "command", "command": "echo world"}),
        ];
        assert!(hook_command_present(&array, "echo hello"));
        assert!(!hook_command_present(&array, "echo other"));
    }

    // -----------------------------------------------------------------------
    // print_report variant tests
    // -----------------------------------------------------------------------

    /// Verify MergeReport correctly identifies all_added state.
    #[test]
    fn test_merge_report_all_added() {
        let report = MergeReport {
            session_start: HookStatus::Added,
            session_end: HookStatus::Added,
            permission_request: HookStatus::Added,
            stop: HookStatus::Added,
            notification_idle_prompt: HookStatus::Added,
            pre_tool_use_bash: HookStatus::Added,
            pre_tool_use_task: HookStatus::Added,
            post_tool_use_bash: HookStatus::Added,
        };
        assert!(report.all_added());
        assert!(report.any_added());
        assert!(!report.all_present());
    }

    /// Verify MergeReport correctly identifies partial update state.
    #[test]
    fn test_merge_report_partial_update() {
        let report = MergeReport {
            session_start: HookStatus::Added,
            session_end: HookStatus::AlreadyPresent,
            permission_request: HookStatus::AlreadyPresent,
            stop: HookStatus::AlreadyPresent,
            notification_idle_prompt: HookStatus::AlreadyPresent,
            pre_tool_use_bash: HookStatus::AlreadyPresent,
            pre_tool_use_task: HookStatus::AlreadyPresent,
            post_tool_use_bash: HookStatus::AlreadyPresent,
        };
        assert!(!report.all_added());
        assert!(report.any_added());
        assert!(!report.all_present());
    }
}
