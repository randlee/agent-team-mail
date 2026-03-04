//! Backend-aware PID validation helpers for daemon liveness decisions.

use agent_team_mail_core::pid::is_pid_alive;
use agent_team_mail_core::schema::{AgentMember, BackendType};

#[derive(Debug, Clone)]
pub(crate) struct PidBackendValidation {
    pub pid: u32,
    pub alive: bool,
    pub backend: String,
    pub expected_rule: String,
    pub actual_process_name: Option<String>,
    pub actual_process_args: Option<String>,
    pub matches_expected: bool,
}

impl PidBackendValidation {
    pub fn expected_display(&self) -> String {
        self.expected_rule.clone()
    }

    pub fn actual_display(&self) -> String {
        let comm = self
            .actual_process_name
            .clone()
            .unwrap_or_else(|| "unknown".to_string());
        if let Some(args) = &self.actual_process_args {
            format!("comm='{}' args='{}'", comm, args)
        } else {
            comm
        }
    }

    pub fn is_alive_mismatch(&self) -> bool {
        self.alive && self.expected_rule != "-" && !self.matches_expected
    }
}

pub(crate) fn roster_process_id(member: &AgentMember) -> Option<u32> {
    member.process_id_hint().filter(|pid| *pid > 1)
}

pub(crate) fn validate_pid_backend(member: &AgentMember, pid: u32) -> PidBackendValidation {
    let rule = expected_rule(member);
    let alive = is_pid_alive(pid);
    let actual_process_name = process_name_for_pid(pid);
    let actual_process_args = process_args_for_pid(pid);
    let matches_expected = if matches!(rule, BackendRule::Unknown) {
        true
    } else if !alive {
        true
    } else {
        rule.matches(
            actual_process_name.as_deref(),
            actual_process_args.as_deref(),
        )
    };

    PidBackendValidation {
        pid,
        alive,
        backend: backend_label(member),
        expected_rule: rule.description().to_string(),
        actual_process_name,
        actual_process_args,
        matches_expected,
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum BackendRule {
    Claude,
    Codex,
    Gemini,
    Unknown,
}

impl BackendRule {
    fn description(self) -> &'static str {
        match self {
            BackendRule::Claude => "comm=claude",
            BackendRule::Codex => "comm=codex",
            BackendRule::Gemini => "comm=node && args~gemini",
            BackendRule::Unknown => "-",
        }
    }

    fn matches(self, comm: Option<&str>, args: Option<&str>) -> bool {
        let comm = comm.unwrap_or_default().to_lowercase();
        let args = args.unwrap_or_default().to_lowercase();
        match self {
            BackendRule::Claude => comm == "claude",
            BackendRule::Codex => comm == "codex",
            BackendRule::Gemini => comm == "node" && args.contains("gemini"),
            BackendRule::Unknown => true,
        }
    }
}

fn expected_rule(member: &AgentMember) -> BackendRule {
    match member.effective_backend_type() {
        Some(BackendType::ClaudeCode) => BackendRule::Claude,
        Some(BackendType::Codex) => BackendRule::Codex,
        Some(BackendType::Gemini) => BackendRule::Gemini,
        Some(BackendType::External) | Some(BackendType::Human(_)) => BackendRule::Unknown,
        None => {
            if member.name == "team-lead"
                || matches!(
                    member.agent_type.as_str(),
                    "general-purpose" | "Explore" | "Plan"
                )
            {
                BackendRule::Claude
            } else {
                BackendRule::Unknown
            }
        }
    }
}

fn backend_label(member: &AgentMember) -> String {
    match member.effective_backend_type() {
        Some(BackendType::ClaudeCode) => "claude-code".to_string(),
        Some(BackendType::Codex) => "codex".to_string(),
        Some(BackendType::Gemini) => "gemini".to_string(),
        Some(BackendType::External) => "external".to_string(),
        Some(BackendType::Human(user)) => format!("human:{user}"),
        None => "unknown".to_string(),
    }
}

fn process_name_for_pid(pid: u32) -> Option<String> {
    use sysinfo::{Pid, System};

    if pid == 0 {
        return None;
    }
    let sys = System::new_all();
    sys.process(Pid::from_u32(pid))
        .map(|p| p.name().to_string_lossy().to_lowercase())
}

fn process_args_for_pid(pid: u32) -> Option<String> {
    use sysinfo::{Pid, System};

    if pid == 0 {
        return None;
    }
    let sys = System::new_all();
    sys.process(Pid::from_u32(pid)).map(|p| {
        p.cmd()
            .iter()
            .map(|part| part.to_string_lossy())
            .collect::<Vec<_>>()
            .join(" ")
            .to_lowercase()
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    fn member(name: &str, agent_type: &str, backend: Option<BackendType>) -> AgentMember {
        AgentMember {
            agent_id: format!("{name}@atm-dev"),
            name: name.to_string(),
            agent_type: agent_type.to_string(),
            model: "unknown".to_string(),
            prompt: None,
            color: None,
            plan_mode_required: None,
            joined_at: 0,
            tmux_pane_id: None,
            cwd: ".".to_string(),
            subscriptions: Vec::new(),
            backend_type: None,
            is_active: None,
            last_active: None,
            session_id: None,
            external_backend_type: backend,
            external_model: None,
            unknown_fields: HashMap::new(),
        }
    }

    #[test]
    fn roster_process_id_reads_extension_field() {
        let mut m = member("arch-ctm", "codex", Some(BackendType::Codex));
        assert_eq!(roster_process_id(&m), None);
        m.set_process_id_hint(Some(4242));
        assert_eq!(roster_process_id(&m), Some(4242));
    }

    #[test]
    fn validate_pid_backend_detects_mismatch_for_live_process() {
        let m = member("arch-ctm", "codex", Some(BackendType::Codex));
        let res = validate_pid_backend(&m, std::process::id());
        assert!(res.alive);
        // The current process name is not expected to be exactly "codex" in tests.
        assert!(res.is_alive_mismatch());
        assert_eq!(res.backend, "codex");
    }

    #[test]
    fn gemini_rule_requires_node_and_gemini_args() {
        assert!(BackendRule::Gemini.matches(Some("node"), Some("gemini --model 2.5-pro")));
        assert!(!BackendRule::Gemini.matches(Some("node"), Some("index.js")));
        assert!(!BackendRule::Gemini.matches(Some("gemini"), Some("gemini")));
    }
}
