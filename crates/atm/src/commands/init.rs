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
use anyhow::{Context, Result};
use clap::Args;
use std::collections::HashMap;
use std::path::{Path, PathBuf};

// ---------------------------------------------------------------------------
// Embedded hook script bodies (compile-time)
// ---------------------------------------------------------------------------

const SESSION_START_PY: &str = include_str!("../../scripts/session-start.py");
const SESSION_END_PY: &str = include_str!("../../scripts/session-end.py");
const TEAMMATE_IDLE_RELAY_PY: &str = include_str!("../../scripts/teammate-idle-relay.py");
const ATM_IDENTITY_WRITE_PY: &str = include_str!("../../scripts/atm-identity-write.py");
const ATM_IDENTITY_CLEANUP_PY: &str = include_str!("../../scripts/atm-identity-cleanup.py");
const GATE_AGENT_SPAWNS_PY: &str = include_str!("../../scripts/gate-agent-spawns.py");
const ATM_HOOK_LIB_PY: &str = include_str!("../../scripts/atm_hook_lib.py");

// ---------------------------------------------------------------------------
// Hook command templates
// ---------------------------------------------------------------------------

// Hooks installed by `atm init`:
// - SessionStart: announce session ID and optionally notify daemon
// - PreToolUse(Bash): write PID-based identity file before `atm` commands
// - PreToolUse(Task): gate agent spawning pattern enforcement
// - PostToolUse(Bash): clean up PID identity file after `atm` commands
//
// Note: `atm init` installs the core hook commands. SessionEnd and
// teammate-idle relay scripts are also materialized for lifecycle parity, even
// when not explicitly wired as hook commands by this command.

/// Return the SessionStart hook command string for local or global install.
fn session_start_cmd(global_scripts_dir: Option<&Path>) -> String {
    let script = hook_script_path(global_scripts_dir, "session-start.py");
    format!(
        "bash -c 'test -f \"${{CLAUDE_PROJECT_DIR}}/.atm.toml\" && python3 \"{script}\" || true'"
    )
}

/// Return the SessionEnd hook command string for local or global install.
fn session_end_cmd(global_scripts_dir: Option<&Path>) -> String {
    let script = hook_script_path(global_scripts_dir, "session-end.py");
    format!(
        "bash -c 'test -f \"${{CLAUDE_PROJECT_DIR}}/.atm.toml\" && python3 \"{script}\" || true'"
    )
}

/// Return the PreToolUse(Bash) hook command string for local or global install.
fn pre_tool_use_bash_cmd(global_scripts_dir: Option<&Path>) -> String {
    let script = hook_script_path(global_scripts_dir, "atm-identity-write.py");
    match global_scripts_dir {
        Some(_) => {
            format!(
                "bash -c 'test -f \"${{CLAUDE_PROJECT_DIR}}/.atm.toml\" && python3 \"{script}\" || true'"
            )
        }
        None => {
            format!("bash -c 'test -f \"{script}\" && python3 \"{script}\" || true'")
        }
    }
}

/// Return the PreToolUse(Task) hook command string for local or global install.
fn pre_tool_use_task_cmd(global_scripts_dir: Option<&Path>) -> String {
    let script = hook_script_path(global_scripts_dir, "gate-agent-spawns.py");
    match global_scripts_dir {
        Some(_) => format!(
            "bash -c 'if test -f \"${{CLAUDE_PROJECT_DIR}}/.atm.toml\"; then python3 \"{script}\"; else exit 0; fi'"
        ),
        None => {
            format!("bash -c 'if test -f \"{script}\"; then python3 \"{script}\"; else exit 0; fi'")
        }
    }
}

/// Return the PostToolUse(Bash) hook command string for local or global install.
fn post_tool_use_bash_cmd(global_scripts_dir: Option<&Path>) -> String {
    let script = hook_script_path(global_scripts_dir, "atm-identity-cleanup.py");
    match global_scripts_dir {
        Some(_) => {
            format!(
                "bash -c 'test -f \"${{CLAUDE_PROJECT_DIR}}/.atm.toml\" && python3 \"{script}\" || true'"
            )
        }
        None => {
            format!("bash -c 'test -f \"{script}\" && python3 \"{script}\" || true'")
        }
    }
}

/// Return a hook script path expression:
/// - Local: uses `$CLAUDE_PROJECT_DIR` so settings remain repo-portable.
/// - Global: uses a resolved absolute per-user path for robustness.
fn hook_script_path(global_scripts_dir: Option<&Path>, script_name: &str) -> String {
    match global_scripts_dir {
        Some(dir) => normalize_for_bash_quoted_path(&dir.join(script_name)),
        None => format!("${{CLAUDE_PROJECT_DIR}}/.claude/scripts/{script_name}"),
    }
}

/// Normalize a filesystem path for inclusion inside a double-quoted `bash -c`
/// command string.
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
    let settings_path = resolve_settings_path(install_global)?;

    let atm_toml_status = ensure_atm_toml(&atm_toml_path, &args.team, &identity)?;
    let team_status = if args.skip_team {
        TeamStatus::Skipped
    } else {
        ensure_team_config(&home_dir, &args.team, &current_dir)?
    };

    // Materialize hook scripts to disk before writing settings
    let scripts_dir = if install_global {
        home_dir.join(".claude").join("scripts")
    } else {
        current_dir.join(".claude").join("scripts")
    };
    materialize_scripts(&scripts_dir)?;

    let mut settings = load_settings(&settings_path)?;

    let report = merge_hooks(
        &mut settings,
        if install_global {
            Some(&scripts_dir)
        } else {
            None
        },
    )?;

    write_settings_atomic(&settings_path, &settings)?;

    print_report(
        &args.team,
        &settings_path,
        &report,
        install_global,
        &atm_toml_path,
        atm_toml_status,
        team_status,
    );

    Ok(())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum AtmTomlStatus {
    Created,
    AlreadyPresent,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TeamStatus {
    Created,
    AlreadyPresent,
    Skipped,
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

fn ensure_team_config(home_dir: &Path, team: &str, cwd: &Path) -> Result<TeamStatus> {
    let team_dir = home_dir.join(".claude/teams").join(team);
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

    write_json_atomic(&config_path, &team_config)?;
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

fn write_json_atomic(path: &Path, value: &impl serde::Serialize) -> Result<()> {
    let mut serialized =
        serde_json::to_string_pretty(value).context("Failed to serialize JSON value")?;
    serialized.push('\n');
    write_text_atomic(path, &serialized)
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
        ("atm-identity-write.py", ATM_IDENTITY_WRITE_PY),
        ("atm-identity-cleanup.py", ATM_IDENTITY_CLEANUP_PY),
        ("gate-agent-spawns.py", GATE_AGENT_SPAWNS_PY),
        ("atm_hook_lib.py", ATM_HOOK_LIB_PY),
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
        Ok(home.join(".claude").join("settings.json"))
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
    pre_tool_use_bash: HookStatus,
    pre_tool_use_task: HookStatus,
    post_tool_use_bash: HookStatus,
}

impl MergeReport {
    fn all_present(&self) -> bool {
        self.session_start == HookStatus::AlreadyPresent
            && self.session_end == HookStatus::AlreadyPresent
            && self.pre_tool_use_bash == HookStatus::AlreadyPresent
            && self.pre_tool_use_task == HookStatus::AlreadyPresent
            && self.post_tool_use_bash == HookStatus::AlreadyPresent
    }

    fn any_added(&self) -> bool {
        self.session_start == HookStatus::Added
            || self.session_end == HookStatus::Added
            || self.pre_tool_use_bash == HookStatus::Added
            || self.pre_tool_use_task == HookStatus::Added
            || self.post_tool_use_bash == HookStatus::Added
    }

    fn all_added(&self) -> bool {
        self.session_start == HookStatus::Added
            && self.session_end == HookStatus::Added
            && self.pre_tool_use_bash == HookStatus::Added
            && self.pre_tool_use_task == HookStatus::Added
            && self.post_tool_use_bash == HookStatus::Added
    }
}

/// Merge all four ATM hooks into `settings` and return a report.
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
    // catch-all matcher schema when present.
    normalize_catch_all_hook_category_if_present(settings, "SessionStart")?;
    normalize_catch_all_hook_category_if_present(settings, "SessionEnd")?;

    let ss_cmd = session_start_cmd(global_scripts_dir);
    let se_cmd = session_end_cmd(global_scripts_dir);
    let ptu_bash = pre_tool_use_bash_cmd(global_scripts_dir);
    let ptu_task = pre_tool_use_task_cmd(global_scripts_dir);
    let post_bash = post_tool_use_bash_cmd(global_scripts_dir);

    let session_start = merge_session_hook(settings, "SessionStart", &ss_cmd)?;
    let session_end = merge_session_hook(settings, "SessionEnd", &se_cmd)?;
    let pre_tool_use_bash = merge_matcher_hook(settings, "PreToolUse", "Bash", &ptu_bash)?;
    let pre_tool_use_task = merge_matcher_hook(settings, "PreToolUse", "Task", &ptu_task)?;
    let post_tool_use_bash = merge_matcher_hook(settings, "PostToolUse", "Bash", &post_bash)?;

    Ok(MergeReport {
        session_start,
        session_end,
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
            let should_normalize_matcher = match obj.get("matcher") {
                None => true,
                Some(serde_json::Value::Null) => true,
                Some(serde_json::Value::String(m)) => m.is_empty(),
                Some(serde_json::Value::Object(m)) => m.is_empty(),
                _ => false,
            };

            if should_normalize_matcher {
                obj.insert("matcher".to_string(), serde_json::json!(""));
            }
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

#[allow(dead_code)] // used in tests; production path uses merge_session_hook
fn catch_all_hook_command_present(array: &[serde_json::Value], cmd: &str) -> bool {
    array.iter().any(|entry| {
        entry
            .get("hooks")
            .and_then(|h| h.as_array())
            .map(|hooks| hook_command_present(hooks, cmd))
            .unwrap_or(false)
    })
}

/// Return `true` when any entry in `array` has a `command` field equal to `cmd`.
fn hook_command_present(array: &[serde_json::Value], cmd: &str) -> bool {
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

fn print_report(
    team: &str,
    settings_path: &Path,
    report: &MergeReport,
    install_global: bool,
    atm_toml_path: &Path,
    atm_toml_status: AtmTomlStatus,
    team_status: TeamStatus,
) {
    match atm_toml_status {
        AtmTomlStatus::Created => {
            println!("Created .atm.toml at {}", atm_toml_path.display());
        }
        AtmTomlStatus::AlreadyPresent => {
            println!(".atm.toml already present at {}", atm_toml_path.display());
        }
    }

    match team_status {
        TeamStatus::Created => println!("Created team '{}'", team),
        TeamStatus::AlreadyPresent => println!("Team '{}' already exists", team),
        TeamStatus::Skipped => println!("Skipped team creation (--skip-team)"),
    }

    if report.all_present() {
        println!(
            "ATM hooks already configured for team '{}' in {}",
            team,
            settings_path.display()
        );
        println!("  \u{2713} SessionStart hook present");
        println!("  \u{2713} PreToolUse(Bash) hook present");
        println!("  \u{2713} PreToolUse(Task) hook present");
        println!("  \u{2713} PostToolUse(Bash) hook present");
    } else if report.all_added() {
        println!(
            "Installed ATM hooks for team '{}' in {}",
            team,
            settings_path.display()
        );
        print_hook_line("SessionStart hook", &report.session_start);
        print_hook_line("PreToolUse(Bash) hook", &report.pre_tool_use_bash);
        print_hook_line("PreToolUse(Task) hook", &report.pre_tool_use_task);
        print_hook_line("PostToolUse(Bash) hook", &report.post_tool_use_bash);
    } else if report.any_added() {
        println!(
            "Updated ATM hooks for team '{}' in {}",
            team,
            settings_path.display()
        );
        print_hook_line("SessionStart hook", &report.session_start);
        print_hook_line("PreToolUse(Bash) hook", &report.pre_tool_use_bash);
        print_hook_line("PreToolUse(Task) hook", &report.pre_tool_use_task);
        print_hook_line("PostToolUse(Bash) hook", &report.post_tool_use_bash);
    }

    println!(
        "Hook scope: {}",
        if install_global { "global" } else { "local" }
    );
}

fn print_hook_line(label: &str, status: &HookStatus) {
    match status {
        HookStatus::Added => println!("  + {label} added"),
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
    use tempfile::TempDir;

    // -----------------------------------------------------------------------
    // Helper: build a settings.json path inside a TempDir
    // -----------------------------------------------------------------------
    fn temp_settings(dir: &TempDir) -> PathBuf {
        dir.path().join(".claude").join("settings.json")
    }

    // -----------------------------------------------------------------------
    // Fresh file test
    // -----------------------------------------------------------------------

    /// Installing into a nonexistent settings.json creates the file with
    /// all four ATM hooks correctly structured.
    #[test]
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
            session_start
                .iter()
                .all(|e| e.get("matcher").and_then(|m| m.as_str()) == Some("")),
            "SessionStart entries must include empty-string matcher"
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
            session_start
                .iter()
                .all(|e| e.get("matcher").and_then(|m| m.as_str()) == Some("")),
            "all SessionStart entries must have empty-string matcher after migration"
        );
        assert!(
            session_start.iter().all(|e| e.get("hooks").is_some()),
            "legacy bare SessionStart entries must be wrapped after migration"
        );

        let session_end = settings["hooks"]["SessionEnd"]
            .as_array()
            .expect("SessionEnd array");
        assert!(
            session_end
                .iter()
                .all(|e| e.get("matcher").and_then(|m| m.as_str()) == Some("")),
            "all SessionEnd entries must have empty-string matcher after migration"
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

    /// Global path resolution must use `ATM_HOME` (cross-platform pattern).
    #[test]
    #[serial]
    fn test_resolve_settings_path_global_uses_atm_home() {
        let dir = TempDir::new().expect("tempdir");
        let old_home = env::var("ATM_HOME").ok();

        // SAFETY: single-threaded test manipulating env var.
        unsafe {
            env::set_var("ATM_HOME", dir.path());
        }

        let path = resolve_settings_path(true).expect("resolve global");
        assert_eq!(path, dir.path().join(".claude").join("settings.json"));

        unsafe {
            match old_home {
                Some(v) => env::set_var("ATM_HOME", v),
                None => env::remove_var("ATM_HOME"),
            }
        }
    }

    /// Global hook commands must use resolved absolute script paths, not
    /// `${HOME}` expansion.
    #[test]
    fn test_global_hook_commands_use_absolute_script_paths() {
        let dir = TempDir::new().expect("tempdir");
        let scripts_dir = dir.path().join("scripts with spaces");
        let expected_session =
            normalize_for_bash_quoted_path(&scripts_dir.join("session-start.py"));
        let expected_write =
            normalize_for_bash_quoted_path(&scripts_dir.join("atm-identity-write.py"));
        let expected_gate =
            normalize_for_bash_quoted_path(&scripts_dir.join("gate-agent-spawns.py"));
        let expected_cleanup =
            normalize_for_bash_quoted_path(&scripts_dir.join("atm-identity-cleanup.py"));

        let session = session_start_cmd(Some(&scripts_dir));
        let write = pre_tool_use_bash_cmd(Some(&scripts_dir));
        let gate = pre_tool_use_task_cmd(Some(&scripts_dir));
        let cleanup = post_tool_use_bash_cmd(Some(&scripts_dir));

        assert!(session.contains(&expected_session));
        assert!(write.contains(&expected_write));
        assert!(gate.contains(&expected_gate));
        assert!(cleanup.contains(&expected_cleanup));

        assert!(!session.contains("${HOME}"));
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
        let old_home = env::var("ATM_HOME").ok();

        // SAFETY: serialized test controls process env and cwd.
        unsafe {
            env::set_var("ATM_HOME", dir.path());
        }
        env::set_current_dir(&repo_dir).expect("set cwd");

        let result = execute(InitArgs {
            team: "atm-dev".to_string(),
            local: false,
            identity: Some("team-lead".to_string()),
            skip_team: false,
            global: false,
        });

        env::set_current_dir(original_dir).expect("restore cwd");
        // SAFETY: serialized test cleanup.
        unsafe {
            match old_home {
                Some(v) => env::set_var("ATM_HOME", v),
                None => env::remove_var("ATM_HOME"),
            }
        }

        assert!(result.is_ok());
        assert!(
            repo_dir.join(".atm.toml").exists(),
            ".atm.toml should be created in repo"
        );
        assert!(
            dir.path()
                .join(".claude/teams/atm-dev/config.json")
                .exists(),
            "team config should be created under ATM_HOME"
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
            "atm-identity-write.py",
            "atm-identity-cleanup.py",
            "gate-agent-spawns.py",
            "atm_hook_lib.py",
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
            pre_tool_use_bash: HookStatus::AlreadyPresent,
            pre_tool_use_task: HookStatus::AlreadyPresent,
            post_tool_use_bash: HookStatus::AlreadyPresent,
        };
        assert!(!report.all_added());
        assert!(report.any_added());
        assert!(!report.all_present());
    }
}
