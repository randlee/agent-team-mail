use anyhow::Result;
use std::collections::HashMap;
use std::path::{Path, PathBuf};

/// Runtime selector for `atm teams spawn`.
#[derive(clap::ValueEnum, Clone, Debug, PartialEq, Eq)]
pub enum RuntimeKind {
    Claude,
    Codex,
    Gemini,
    Opencode,
}

/// Launch-time options used by runtime adapters.
#[derive(Clone, Debug)]
pub struct SpawnSpec {
    pub team: String,
    pub agent: String,
    pub model: Option<String>,
    pub sandbox: Option<bool>,
    pub approval_mode: Option<String>,
    pub resume: bool,
    pub resume_session_id: Option<String>,
    pub system_prompt: Option<PathBuf>,
}

/// Runtime adapter trait for spawn command construction.
pub trait RuntimeAdapter {
    fn build_command(&self, spec: &SpawnSpec) -> Result<String>;
    fn build_env(&self, spec: &SpawnSpec, home_dir: &Path) -> Result<HashMap<String, String>>;
}

/// Gemini runtime adapter (baseline S.1 behavior).
pub struct GeminiAdapter;

impl RuntimeAdapter for GeminiAdapter {
    fn build_command(&self, spec: &SpawnSpec) -> Result<String> {
        let mut parts = vec![
            "gemini".to_string(),
            "--prompt-interactive".to_string(),
            "--sandbox".to_string(),
            spec.sandbox
                .map(|v| v.to_string())
                .unwrap_or_else(|| "false".to_string()),
        ];

        if let Some(model) = &spec.model {
            parts.push("--model".to_string());
            parts.push(shell_quote(model));
        }

        if let Some(mode) = &spec.approval_mode {
            parts.push("--approval-mode".to_string());
            parts.push(shell_quote(mode));
        }

        if spec.resume {
            parts.push("--resume".to_string());
            if let Some(session_id) = &spec.resume_session_id {
                parts.push(shell_quote(session_id));
            }
        }

        Ok(parts.join(" "))
    }

    fn build_env(&self, spec: &SpawnSpec, home_dir: &Path) -> Result<HashMap<String, String>> {
        let mut env = HashMap::new();
        let runtime_home = home_dir
            .join(".claude")
            .join("runtime")
            .join("gemini")
            .join(&spec.team)
            .join(&spec.agent)
            .join("home");
        env.insert(
            "GEMINI_CLI_HOME".to_string(),
            runtime_home.to_string_lossy().to_string(),
        );
        env.insert(
            "ATM_RUNTIME_HOME".to_string(),
            runtime_home.to_string_lossy().to_string(),
        );

        if let Some(path) = &spec.system_prompt {
            env.insert(
                "GEMINI_SYSTEM_MD".to_string(),
                path.to_string_lossy().to_string(),
            );
        }

        Ok(env)
    }
}

/// Codex runtime adapter (keeps current launch behavior).
pub struct CodexAdapter;

impl RuntimeAdapter for CodexAdapter {
    fn build_command(&self, _spec: &SpawnSpec) -> Result<String> {
        Ok("codex --yolo".to_string())
    }

    fn build_env(&self, _spec: &SpawnSpec, _home_dir: &Path) -> Result<HashMap<String, String>> {
        Ok(HashMap::new())
    }
}

/// Claude runtime adapter placeholder for CLI compatibility.
pub struct ClaudeAdapter;

impl RuntimeAdapter for ClaudeAdapter {
    fn build_command(&self, _spec: &SpawnSpec) -> Result<String> {
        Ok("claude".to_string())
    }

    fn build_env(&self, _spec: &SpawnSpec, _home_dir: &Path) -> Result<HashMap<String, String>> {
        Ok(HashMap::new())
    }
}

/// OpenCode runtime adapter placeholder for CLI compatibility.
pub struct OpenCodeAdapter;

impl RuntimeAdapter for OpenCodeAdapter {
    fn build_command(&self, _spec: &SpawnSpec) -> Result<String> {
        Ok("opencode".to_string())
    }

    fn build_env(&self, _spec: &SpawnSpec, _home_dir: &Path) -> Result<HashMap<String, String>> {
        Ok(HashMap::new())
    }
}

pub fn adapter_for_runtime(runtime: &RuntimeKind) -> Box<dyn RuntimeAdapter> {
    match runtime {
        RuntimeKind::Gemini => Box::new(GeminiAdapter),
        RuntimeKind::Claude => Box::new(ClaudeAdapter),
        RuntimeKind::Opencode => Box::new(OpenCodeAdapter),
        RuntimeKind::Codex => Box::new(CodexAdapter),
    }
}

fn shell_quote(input: &str) -> String {
    if input.is_empty() {
        return "''".to_string();
    }
    format!("'{}'", input.replace('\'', "'\"'\"'"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn base_spec() -> SpawnSpec {
        SpawnSpec {
            team: "atm-dev".to_string(),
            agent: "arch-ctm".to_string(),
            model: None,
            sandbox: None,
            approval_mode: None,
            resume: false,
            resume_session_id: None,
            system_prompt: None,
        }
    }

    #[test]
    fn gemini_build_command_uses_baseline_flags() {
        let adapter = GeminiAdapter;
        let mut spec = base_spec();
        spec.model = Some("gemini-2.5-pro".to_string());
        spec.approval_mode = Some("plan".to_string());

        let cmd = adapter.build_command(&spec).unwrap();
        assert!(cmd.contains("gemini"));
        assert!(cmd.contains("--prompt-interactive"));
        assert!(cmd.contains("--sandbox false"));
        assert!(cmd.contains("--model 'gemini-2.5-pro'"));
        assert!(cmd.contains("--approval-mode 'plan'"));
    }

    #[test]
    fn gemini_resume_uses_explicit_session_when_present() {
        let adapter = GeminiAdapter;
        let mut spec = base_spec();
        spec.resume = true;
        spec.resume_session_id = Some("session-123".to_string());

        let cmd = adapter.build_command(&spec).unwrap();
        assert!(cmd.contains("--resume 'session-123'"));
    }

    #[test]
    fn gemini_build_env_sets_runtime_home_and_system_prompt() {
        let adapter = GeminiAdapter;
        let mut spec = base_spec();
        let system_md = std::env::temp_dir().join("system.md");
        spec.system_prompt = Some(system_md.clone());

        let home_dir = std::env::temp_dir().join("tester");
        let env = adapter
            .build_env(&spec, &home_dir)
            .expect("env build should succeed");
        let runtime_home = env
            .get("GEMINI_CLI_HOME")
            .expect("GEMINI_CLI_HOME should be set");
        let runtime_home_norm = runtime_home.replace('\\', "/");
        assert!(runtime_home_norm.contains(".claude/runtime/gemini/atm-dev/arch-ctm/home"));
        assert_eq!(
            env.get("ATM_RUNTIME_HOME").map(String::as_str),
            Some(runtime_home.as_str())
        );
        assert_eq!(
            env.get("GEMINI_SYSTEM_MD").map(String::as_str),
            Some(system_md.to_string_lossy().as_ref())
        );
    }
}
