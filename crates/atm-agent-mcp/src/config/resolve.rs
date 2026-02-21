//! Config resolution for atm-agent-mcp.
//!
//! Resolves [`AgentMcpConfig`] from multiple sources with the following priority
//! (highest to lowest):
//!
//! 1. CLI flags (applied by the caller after [`resolve_config`] returns)
//! 2. Environment variables (`ATM_AGENT_MCP_*`)
//! 3. Repo-local `.atm.toml` `[plugins.atm-agent-mcp]` section
//! 4. Global `~/.config/atm/config.toml` `[plugins.atm-agent-mcp]` section
//! 5. Compiled-in defaults (via [`AgentMcpConfig::default`])

use super::types::AgentMcpConfig;
use agent_team_mail_core::config::{resolve_config as core_resolve, ConfigOverrides, CoreConfig};
use agent_team_mail_core::home::get_home_dir;
use std::path::Path;

/// Fully resolved configuration combining ATM core settings with plugin config.
#[derive(Debug, Clone)]
pub struct ResolvedConfig {
    /// Plugin-specific MCP configuration
    pub agent_mcp: AgentMcpConfig,
    /// ATM core configuration (identity, team, etc.)
    pub core: CoreConfig,
}

/// Resolve the complete configuration for atm-agent-mcp.
///
/// Loads ATM base config (`.atm.toml` + env + global config), extracts the
/// `[plugins.atm-agent-mcp]` section into [`AgentMcpConfig`], and applies
/// `ATM_AGENT_MCP_*` environment variable overrides.
///
/// # Arguments
///
/// * `config_path` – Optional explicit path to `.atm.toml`. When `None` the
///   function searches from the current working directory up to the git root,
///   then falls back to `~/.config/atm/config.toml`.
///
/// # Errors
///
/// Returns an error if the home directory cannot be determined or if an
/// explicit `config_path` cannot be read.
pub fn resolve_config(config_path: Option<&Path>) -> anyhow::Result<ResolvedConfig> {
    let home_dir = get_home_dir()?;
    let current_dir = std::env::current_dir()?;

    let mut overrides = ConfigOverrides::default();
    if let Some(path) = config_path {
        overrides.config_path = Some(path.to_path_buf());
    }

    // When an explicit config path is given, use its parent directory as the
    // search root so repo-local detection still works correctly.
    let search_dir = if let Some(path) = config_path {
        path.parent()
            .map(|p| p.to_path_buf())
            .unwrap_or_else(|| current_dir.clone())
    } else {
        current_dir.clone()
    };

    let core_config = core_resolve(&overrides, &search_dir, &home_dir)?;

    // Extract plugin config, falling back to defaults if the section is absent.
    let mut agent_mcp = if let Some(table) = core_config.plugin_config("atm-agent-mcp") {
        toml::Value::Table(table.clone())
            .try_into::<AgentMcpConfig>()
            .unwrap_or_default()
    } else {
        AgentMcpConfig::default()
    };

    apply_env_overrides(&mut agent_mcp);

    Ok(ResolvedConfig {
        agent_mcp,
        core: core_config.core,
    })
}

/// Apply `ATM_AGENT_MCP_*` environment variable overrides to `cfg`.
///
/// Empty string values are treated as "not set" and do not override existing
/// configuration.
fn apply_env_overrides(cfg: &mut AgentMcpConfig) {
    if let Ok(v) = std::env::var("ATM_AGENT_MCP_IDENTITY") {
        if !v.is_empty() {
            cfg.identity = Some(v);
        }
    }
    if let Ok(v) = std::env::var("ATM_AGENT_MCP_MODEL") {
        if !v.is_empty() {
            cfg.model = Some(v);
        }
    }
    if let Ok(v) = std::env::var("ATM_AGENT_MCP_SANDBOX") {
        if !v.is_empty() {
            cfg.sandbox = v;
        }
    }
    if let Ok(v) = std::env::var("ATM_AGENT_MCP_APPROVAL_POLICY") {
        if !v.is_empty() {
            cfg.approval_policy = v;
        }
    }
    if let Ok(v) = std::env::var("ATM_AGENT_MCP_CODEX_BIN") {
        if !v.is_empty() {
            cfg.codex_bin = v;
        }
    }
    if let Ok(v) = std::env::var("ATM_AGENT_MCP_MAIL_POLL_INTERVAL_MS") {
        if let Ok(ms) = v.parse::<u64>() {
            cfg.mail_poll_interval_ms = ms;
        }
    }
    if let Ok(v) = std::env::var("ATM_AGENT_MCP_FAST_MODEL") {
        if !v.is_empty() {
            cfg.fast_model = Some(v);
        }
    }
    if let Ok(v) = std::env::var("ATM_AGENT_MCP_REASONING_EFFORT") {
        if !v.is_empty() {
            cfg.reasoning_effort = Some(v);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serial_test::serial;
    use std::env;

    // ─── Default value tests ────────────────────────────────────────────────

    #[test]
    fn test_default_config_is_complete() {
        let cfg = AgentMcpConfig::default();
        // Verify the struct can be created and key fields exist
        assert!(!cfg.codex_bin.is_empty());
        assert!(!cfg.sandbox.is_empty());
        assert!(!cfg.approval_policy.is_empty());
    }

    #[test]
    fn test_default_codex_bin() {
        let cfg = AgentMcpConfig::default();
        assert_eq!(cfg.codex_bin, "codex");
    }

    #[test]
    fn test_default_sandbox() {
        let cfg = AgentMcpConfig::default();
        assert_eq!(cfg.sandbox, "workspace-write");
    }

    #[test]
    fn test_default_approval_policy() {
        let cfg = AgentMcpConfig::default();
        assert_eq!(cfg.approval_policy, "on-failure");
    }

    #[test]
    fn test_default_mail_poll_interval_ms() {
        let cfg = AgentMcpConfig::default();
        assert_eq!(cfg.mail_poll_interval_ms, 5000);
    }

    #[test]
    fn test_default_request_timeout_secs() {
        let cfg = AgentMcpConfig::default();
        assert_eq!(cfg.request_timeout_secs, 300);
    }

    #[test]
    fn test_default_max_concurrent_threads() {
        let cfg = AgentMcpConfig::default();
        assert_eq!(cfg.max_concurrent_threads, 10);
    }

    #[test]
    fn test_default_persist_threads() {
        let cfg = AgentMcpConfig::default();
        assert!(cfg.persist_threads);
    }

    #[test]
    fn test_default_auto_mail() {
        let cfg = AgentMcpConfig::default();
        assert!(cfg.auto_mail);
    }

    #[test]
    fn test_default_optional_fields_are_none() {
        let cfg = AgentMcpConfig::default();
        assert!(cfg.identity.is_none());
        assert!(cfg.model.is_none());
        assert!(cfg.fast_model.is_none());
        assert!(cfg.reasoning_effort.is_none());
        assert!(cfg.base_prompt_file.is_none());
        assert!(cfg.extra_instructions_file.is_none());
    }

    #[test]
    fn test_default_roles_is_empty() {
        let cfg = AgentMcpConfig::default();
        assert!(cfg.roles.is_empty());
    }

    // ─── TOML deserialization tests ─────────────────────────────────────────

    #[test]
    fn test_toml_full_deserialization() {
        let toml_str = r#"
codex_bin = "/usr/local/bin/codex"
identity = "arch-ctm"
model = "o3"
fast_model = "o4-mini"
reasoning_effort = "high"
sandbox = "network-disabled"
approval_policy = "never"
mail_poll_interval_ms = 3000
request_timeout_secs = 600
max_concurrent_threads = 5
persist_threads = false
auto_mail = false
base_prompt_file = "/tmp/base.md"
extra_instructions_file = "/tmp/extra.md"
"#;
        let cfg: AgentMcpConfig = toml::from_str(toml_str).unwrap();
        assert_eq!(cfg.codex_bin, "/usr/local/bin/codex");
        assert_eq!(cfg.identity, Some("arch-ctm".to_string()));
        assert_eq!(cfg.model, Some("o3".to_string()));
        assert_eq!(cfg.fast_model, Some("o4-mini".to_string()));
        assert_eq!(cfg.reasoning_effort, Some("high".to_string()));
        assert_eq!(cfg.sandbox, "network-disabled");
        assert_eq!(cfg.approval_policy, "never");
        assert_eq!(cfg.mail_poll_interval_ms, 3000);
        assert_eq!(cfg.request_timeout_secs, 600);
        assert_eq!(cfg.max_concurrent_threads, 5);
        assert!(!cfg.persist_threads);
        assert!(!cfg.auto_mail);
        assert_eq!(cfg.base_prompt_file, Some("/tmp/base.md".to_string()));
        assert_eq!(cfg.extra_instructions_file, Some("/tmp/extra.md".to_string()));
    }

    #[test]
    fn test_toml_partial_uses_defaults() {
        // Only override a few fields; the rest should use defaults
        let toml_str = r#"
identity = "dev-agent"
model = "claude-3-5-sonnet"
"#;
        let cfg: AgentMcpConfig = toml::from_str(toml_str).unwrap();
        assert_eq!(cfg.identity, Some("dev-agent".to_string()));
        assert_eq!(cfg.model, Some("claude-3-5-sonnet".to_string()));
        // Defaults preserved
        assert_eq!(cfg.codex_bin, "codex");
        assert_eq!(cfg.sandbox, "workspace-write");
        assert_eq!(cfg.approval_policy, "on-failure");
        assert_eq!(cfg.mail_poll_interval_ms, 5000);
        assert_eq!(cfg.request_timeout_secs, 300);
        assert_eq!(cfg.max_concurrent_threads, 10);
        assert!(cfg.persist_threads);
        assert!(cfg.auto_mail);
    }

    #[test]
    fn test_toml_empty_section_uses_all_defaults() {
        let cfg: AgentMcpConfig = toml::from_str("").unwrap();
        assert_eq!(cfg.codex_bin, "codex");
        assert_eq!(cfg.sandbox, "workspace-write");
        assert_eq!(cfg.approval_policy, "on-failure");
        assert_eq!(cfg.mail_poll_interval_ms, 5000);
    }

    #[test]
    fn test_role_presets_deserialization() {
        let toml_str = r#"
[roles.architect]
model = "o3"
reasoning_effort = "high"
approval_policy = "never"

[roles.qa]
model = "claude-3-5-haiku"
sandbox = "network-disabled"
"#;
        let cfg: AgentMcpConfig = toml::from_str(toml_str).unwrap();
        assert_eq!(cfg.roles.len(), 2);

        let arch = cfg.roles.get("architect").unwrap();
        assert_eq!(arch.model, Some("o3".to_string()));
        assert_eq!(arch.reasoning_effort, Some("high".to_string()));
        assert_eq!(arch.approval_policy, Some("never".to_string()));
        assert!(arch.sandbox.is_none());

        let qa = cfg.roles.get("qa").unwrap();
        assert_eq!(qa.model, Some("claude-3-5-haiku".to_string()));
        assert_eq!(qa.sandbox, Some("network-disabled".to_string()));
        assert!(qa.reasoning_effort.is_none());
    }

    #[test]
    fn test_role_preset_all_none_fields() {
        let toml_str = "[roles.empty]\n";
        let cfg: AgentMcpConfig = toml::from_str(toml_str).unwrap();
        let preset = cfg.roles.get("empty").unwrap();
        assert!(preset.model.is_none());
        assert!(preset.sandbox.is_none());
        assert!(preset.approval_policy.is_none());
        assert!(preset.reasoning_effort.is_none());
    }

    #[test]
    fn test_json_round_trip() {
        let original = AgentMcpConfig {
            codex_bin: "my-codex".to_string(),
            identity: Some("test-id".to_string()),
            model: Some("gpt-4o".to_string()),
            fast_model: None,
            reasoning_effort: None,
            sandbox: "workspace-write".to_string(),
            approval_policy: "on-failure".to_string(),
            mail_poll_interval_ms: 2000,
            request_timeout_secs: 120,
            max_concurrent_threads: 4,
            persist_threads: false,
            auto_mail: true,
            max_mail_messages: 10,
            max_mail_message_length: 4096,
            per_thread_auto_mail: std::collections::HashMap::new(),
            base_prompt_file: None,
            extra_instructions_file: None,
            roles: std::collections::HashMap::new(),
            transport: None,
        };

        let json = serde_json::to_string_pretty(&original).unwrap();
        let restored: AgentMcpConfig = serde_json::from_str(&json).unwrap();

        assert_eq!(original.codex_bin, restored.codex_bin);
        assert_eq!(original.identity, restored.identity);
        assert_eq!(original.model, restored.model);
        assert_eq!(original.sandbox, restored.sandbox);
        assert_eq!(original.approval_policy, restored.approval_policy);
        assert_eq!(original.mail_poll_interval_ms, restored.mail_poll_interval_ms);
        assert_eq!(original.request_timeout_secs, restored.request_timeout_secs);
        assert_eq!(original.max_concurrent_threads, restored.max_concurrent_threads);
        assert_eq!(original.persist_threads, restored.persist_threads);
        assert_eq!(original.auto_mail, restored.auto_mail);
    }

    // ─── Environment variable override tests ────────────────────────────────

    #[test]
    #[serial]
    fn test_env_identity_override() {
        unsafe {
            env::remove_var("ATM_AGENT_MCP_IDENTITY");
        }
        let mut cfg = AgentMcpConfig::default();
        unsafe {
            env::set_var("ATM_AGENT_MCP_IDENTITY", "env-agent");
        }
        apply_env_overrides(&mut cfg);
        assert_eq!(cfg.identity, Some("env-agent".to_string()));
        unsafe {
            env::remove_var("ATM_AGENT_MCP_IDENTITY");
        }
    }

    #[test]
    #[serial]
    fn test_env_model_override() {
        unsafe {
            env::remove_var("ATM_AGENT_MCP_MODEL");
        }
        let mut cfg = AgentMcpConfig::default();
        unsafe {
            env::set_var("ATM_AGENT_MCP_MODEL", "o3-mini");
        }
        apply_env_overrides(&mut cfg);
        assert_eq!(cfg.model, Some("o3-mini".to_string()));
        unsafe {
            env::remove_var("ATM_AGENT_MCP_MODEL");
        }
    }

    #[test]
    #[serial]
    fn test_env_sandbox_override() {
        unsafe {
            env::remove_var("ATM_AGENT_MCP_SANDBOX");
        }
        let mut cfg = AgentMcpConfig::default();
        unsafe {
            env::set_var("ATM_AGENT_MCP_SANDBOX", "network-disabled");
        }
        apply_env_overrides(&mut cfg);
        assert_eq!(cfg.sandbox, "network-disabled");
        unsafe {
            env::remove_var("ATM_AGENT_MCP_SANDBOX");
        }
    }

    #[test]
    #[serial]
    fn test_env_mail_poll_interval_numeric_parse() {
        unsafe {
            env::remove_var("ATM_AGENT_MCP_MAIL_POLL_INTERVAL_MS");
        }
        let mut cfg = AgentMcpConfig::default();
        unsafe {
            env::set_var("ATM_AGENT_MCP_MAIL_POLL_INTERVAL_MS", "1500");
        }
        apply_env_overrides(&mut cfg);
        assert_eq!(cfg.mail_poll_interval_ms, 1500);
        unsafe {
            env::remove_var("ATM_AGENT_MCP_MAIL_POLL_INTERVAL_MS");
        }
    }

    #[test]
    #[serial]
    fn test_env_codex_bin_override() {
        unsafe {
            env::remove_var("ATM_AGENT_MCP_CODEX_BIN");
        }
        let mut cfg = AgentMcpConfig::default();
        unsafe {
            env::set_var("ATM_AGENT_MCP_CODEX_BIN", "/opt/bin/codex");
        }
        apply_env_overrides(&mut cfg);
        assert_eq!(cfg.codex_bin, "/opt/bin/codex");
        unsafe {
            env::remove_var("ATM_AGENT_MCP_CODEX_BIN");
        }
    }

    #[test]
    #[serial]
    fn test_empty_env_does_not_override_identity() {
        unsafe {
            env::set_var("ATM_AGENT_MCP_IDENTITY", "");
        }
        let cfg = AgentMcpConfig {
            identity: Some("preset-identity".to_string()),
            ..Default::default()
        };
        let mut cfg = cfg;
        apply_env_overrides(&mut cfg);
        // Empty string must not override the existing value
        assert_eq!(cfg.identity, Some("preset-identity".to_string()));
        unsafe {
            env::remove_var("ATM_AGENT_MCP_IDENTITY");
        }
    }

    #[test]
    #[serial]
    fn test_empty_env_does_not_override_sandbox() {
        unsafe {
            env::set_var("ATM_AGENT_MCP_SANDBOX", "");
        }
        let mut cfg = AgentMcpConfig::default();
        // sandbox starts at default "workspace-write"
        apply_env_overrides(&mut cfg);
        assert_eq!(cfg.sandbox, "workspace-write");
        unsafe {
            env::remove_var("ATM_AGENT_MCP_SANDBOX");
        }
    }

    #[test]
    #[serial]
    fn test_invalid_numeric_env_does_not_crash() {
        unsafe {
            env::set_var("ATM_AGENT_MCP_MAIL_POLL_INTERVAL_MS", "not-a-number");
        }
        let mut cfg = AgentMcpConfig::default();
        apply_env_overrides(&mut cfg); // must not panic
        // Value unchanged because parse failed
        assert_eq!(cfg.mail_poll_interval_ms, 5000);
        unsafe {
            env::remove_var("ATM_AGENT_MCP_MAIL_POLL_INTERVAL_MS");
        }
    }

    #[test]
    #[serial]
    fn test_env_fast_model_override() {
        unsafe {
            env::remove_var("ATM_AGENT_MCP_FAST_MODEL");
        }
        let mut cfg = AgentMcpConfig::default();
        unsafe {
            env::set_var("ATM_AGENT_MCP_FAST_MODEL", "o4-mini");
        }
        apply_env_overrides(&mut cfg);
        assert_eq!(cfg.fast_model, Some("o4-mini".to_string()));
        unsafe {
            env::remove_var("ATM_AGENT_MCP_FAST_MODEL");
        }
    }

    #[test]
    #[serial]
    fn test_env_reasoning_effort_override() {
        unsafe {
            env::remove_var("ATM_AGENT_MCP_REASONING_EFFORT");
        }
        let mut cfg = AgentMcpConfig::default();
        unsafe {
            env::set_var("ATM_AGENT_MCP_REASONING_EFFORT", "medium");
        }
        apply_env_overrides(&mut cfg);
        assert_eq!(cfg.reasoning_effort, Some("medium".to_string()));
        unsafe {
            env::remove_var("ATM_AGENT_MCP_REASONING_EFFORT");
        }
    }

    #[test]
    #[serial]
    fn test_env_approval_policy_override() {
        unsafe {
            env::remove_var("ATM_AGENT_MCP_APPROVAL_POLICY");
        }
        let mut cfg = AgentMcpConfig::default();
        unsafe {
            env::set_var("ATM_AGENT_MCP_APPROVAL_POLICY", "never");
        }
        apply_env_overrides(&mut cfg);
        assert_eq!(cfg.approval_policy, "never");
        unsafe {
            env::remove_var("ATM_AGENT_MCP_APPROVAL_POLICY");
        }
    }
}
