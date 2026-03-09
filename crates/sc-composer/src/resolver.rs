use std::path::PathBuf;

use crate::{ComposeMode, ComposeRequest, ComposerError, ProfileKind, RuntimeKind};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolveResult {
    pub resolved_path: PathBuf,
    pub attempted_paths: Vec<PathBuf>,
}

pub fn resolve_input_path(request: &ComposeRequest) -> Result<ResolveResult, ComposerError> {
    if let Some(template_path) = &request.template_path {
        let resolved = absolutize(&request.root, template_path);
        return Ok(ResolveResult {
            resolved_path: resolved.clone(),
            attempted_paths: vec![resolved],
        });
    }

    match request.mode {
        ComposeMode::File => Err(ComposerError::MissingTemplatePath),
        ComposeMode::Profile => resolve_profile_path(request),
    }
}

fn resolve_profile_path(request: &ComposeRequest) -> Result<ResolveResult, ComposerError> {
    let kind = request.kind.ok_or(ComposerError::MissingProfileKind)?;
    let name = request
        .agent
        .as_ref()
        .ok_or(ComposerError::MissingProfileName)?;

    let base_dirs = candidate_base_dirs(request.runtime, kind);
    let mut attempted_paths = Vec::new();
    for base in base_dirs {
        for suffix in probe_suffixes(kind, name) {
            let candidate = request.root.join(&base).join(suffix);
            attempted_paths.push(candidate.clone());
            if candidate.exists() {
                return Ok(ResolveResult {
                    resolved_path: candidate,
                    attempted_paths,
                });
            }
        }
    }

    Err(ComposerError::ProfileResolutionFailed {
        runtime: request.runtime,
        kind,
        name: name.clone(),
        attempted_paths: Box::new(attempted_paths),
    })
}

fn candidate_base_dirs(runtime: RuntimeKind, kind: ProfileKind) -> Vec<PathBuf> {
    let mut dirs = Vec::new();
    let runtime_dir = match runtime {
        RuntimeKind::Claude => Some(".claude"),
        RuntimeKind::Codex => Some(".codex"),
        RuntimeKind::Gemini => Some(".gemini"),
        RuntimeKind::Opencode => Some(".opencode"),
        RuntimeKind::Custom => None,
    };

    if let Some(rt) = runtime_dir {
        dirs.push(PathBuf::from(rt).join(kind_dir(kind)));
    }
    dirs.push(PathBuf::from(shared_dir(kind)));
    let claude_fallback = PathBuf::from(".claude").join(kind_dir(kind));
    if !dirs.contains(&claude_fallback) {
        dirs.push(claude_fallback);
    }

    dirs
}

fn kind_dir(kind: ProfileKind) -> &'static str {
    match kind {
        ProfileKind::Agent => "agents",
        ProfileKind::Command => "commands",
        ProfileKind::Skill => "skills",
    }
}

fn shared_dir(kind: ProfileKind) -> &'static str {
    match kind {
        ProfileKind::Agent => ".agents",
        ProfileKind::Command => ".commands",
        ProfileKind::Skill => ".skills",
    }
}

fn probe_suffixes(kind: ProfileKind, name: &str) -> Vec<PathBuf> {
    match kind {
        ProfileKind::Agent | ProfileKind::Command => vec![
            PathBuf::from(format!("{name}.md.j2")),
            PathBuf::from(format!("{name}.md")),
            PathBuf::from(format!("{name}.j2")),
        ],
        ProfileKind::Skill => vec![
            PathBuf::from(name).join("SKILL.md.j2"),
            PathBuf::from(name).join("SKILL.md"),
            PathBuf::from(name).join("SKILL.j2"),
        ],
    }
}

fn absolutize(root: &std::path::Path, candidate: &std::path::Path) -> PathBuf {
    if candidate.is_absolute() {
        candidate.to_path_buf()
    } else {
        root.join(candidate)
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use tempfile::TempDir;

    use crate::{ComposePolicy, ComposeRequest, ProfileKind};

    use super::*;

    fn profile_request(root: &TempDir, runtime: RuntimeKind, kind: ProfileKind) -> ComposeRequest {
        ComposeRequest {
            runtime,
            mode: ComposeMode::Profile,
            kind: Some(kind),
            root: root.path().to_path_buf(),
            agent: Some("test-profile".to_string()),
            template_path: None,
            vars_input: BTreeMap::new(),
            vars_env: BTreeMap::new(),
            guidance_block: None,
            user_prompt: None,
            policy: ComposePolicy::default(),
        }
    }

    #[test]
    fn resolve_profile_prefers_runtime_then_shared_then_claude_fallback() {
        let tmp = TempDir::new().expect("tempdir");
        std::fs::create_dir_all(tmp.path().join(".agents")).expect("mkdir");
        std::fs::create_dir_all(tmp.path().join(".claude/agents")).expect("mkdir");
        std::fs::write(tmp.path().join(".agents/test-profile.md"), "shared-profile")
            .expect("write");
        std::fs::write(
            tmp.path().join(".claude/agents/test-profile.md"),
            "claude-fallback",
        )
        .expect("write");

        let resolved = resolve_input_path(&profile_request(
            &tmp,
            RuntimeKind::Codex,
            ProfileKind::Agent,
        ))
        .expect("resolve");
        assert_eq!(
            resolved.resolved_path,
            tmp.path().join(".agents/test-profile.md")
        );
        assert!(
            resolved
                .attempted_paths
                .iter()
                .any(|p| p.ends_with(".codex/agents/test-profile.md"))
        );
    }

    #[test]
    fn explicit_template_path_bypasses_profile_resolution() {
        let tmp = TempDir::new().expect("tempdir");
        let request = ComposeRequest {
            runtime: RuntimeKind::Codex,
            mode: ComposeMode::Profile,
            kind: Some(ProfileKind::Agent),
            root: tmp.path().to_path_buf(),
            agent: Some("missing".to_string()),
            template_path: Some(PathBuf::from("templates/direct.md.j2")),
            vars_input: BTreeMap::new(),
            vars_env: BTreeMap::new(),
            guidance_block: None,
            user_prompt: None,
            policy: ComposePolicy::default(),
        };

        let resolved = resolve_input_path(&request).expect("resolve");
        assert_eq!(
            resolved.resolved_path,
            tmp.path().join("templates/direct.md.j2")
        );
        assert_eq!(resolved.attempted_paths.len(), 1);
    }
}
