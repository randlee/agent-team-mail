//! sc-composer: standalone composition engine for prompt/template pipelines.
//!
//! This crate intentionally stays runtime-agnostic so it can be reused by ATM
//! and non-ATM tools.

mod context;
mod diagnostics;
mod frontmatter;
mod include;
mod pipeline;
mod render;
mod resolver;
mod validate;

use std::collections::BTreeMap;
use std::path::PathBuf;

use agent_team_mail_core::home::get_home_dir;
use agent_team_mail_core::logging_event::LogEventV1;
use pipeline::compose_blocks;
use render::render_template;
use sc_observability::{LogConfig as SharedLogConfig, Logger as SharedLogger};
use validate::{evaluate_context, prepare_template, validate_request};

pub use diagnostics::Diagnostic;
pub use resolver::ResolveResult;
pub use validate::ValidationReport;

/// Supported runtime profiles for default agent file resolution policy.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RuntimeKind {
    Claude,
    Codex,
    Gemini,
    Opencode,
    Custom,
}

/// Request mode for composition.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ComposeMode {
    File,
    Profile,
}

/// Profile kind for profile-mode resolution.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProfileKind {
    Agent,
    Command,
    Skill,
}

/// How unknown input variables should be handled.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UnknownVariablePolicy {
    Error,
    Warn,
    Ignore,
}

/// Source of a resolved variable in the final context.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VariableSource {
    Input,
    Env,
    Default,
}

/// Composition safety and validation policy.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ComposePolicy {
    pub unknown_variable_policy: UnknownVariablePolicy,
    pub max_include_depth: usize,
    pub allowed_roots: Vec<PathBuf>,
}

impl Default for ComposePolicy {
    fn default() -> Self {
        Self {
            unknown_variable_policy: UnknownVariablePolicy::Error,
            max_include_depth: 8,
            allowed_roots: Vec::new(),
        }
    }
}

/// Input request for compose/validate operations.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ComposeRequest {
    pub runtime: RuntimeKind,
    pub mode: ComposeMode,
    pub kind: Option<ProfileKind>,
    pub root: PathBuf,
    pub agent: Option<String>,
    pub template_path: Option<PathBuf>,
    pub vars_input: BTreeMap<String, String>,
    pub vars_env: BTreeMap<String, String>,
    pub guidance_block: Option<String>,
    pub user_prompt: Option<String>,
    pub policy: ComposePolicy,
}

/// Result of a successful compose operation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ComposeResult {
    pub rendered_text: String,
    pub resolved_files: Vec<PathBuf>,
    pub search_trace: Vec<PathBuf>,
    pub variable_sources: BTreeMap<String, VariableSource>,
    pub warnings: Vec<Diagnostic>,
}

/// Stable error surface for composition failures.
#[derive(Debug, thiserror::Error)]
pub enum ComposerError {
    #[error("template path is required for compose/validate in file mode")]
    MissingTemplatePath,
    #[error("profile kind is required in profile mode")]
    MissingProfileKind,
    #[error("profile name (--agent) is required in profile mode")]
    MissingProfileName,
    #[error(
        "profile resolution failed for runtime={runtime:?} kind={kind:?} name={name}; attempted={attempted_paths:?}"
    )]
    ProfileResolutionFailed {
        runtime: RuntimeKind,
        kind: ProfileKind,
        name: String,
        attempted_paths: Box<Vec<PathBuf>>,
    },
    #[error("failed to read template at {path}: {source}")]
    TemplateRead {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("invalid frontmatter in {path}: {message}")]
    FrontmatterParse { path: PathBuf, message: String },
    #[error("include processing failed: {diagnostic:?}")]
    IncludeError { diagnostic: Box<Diagnostic> },
    #[error("template parse/render failed in {path}: {message}")]
    TemplateRender { path: PathBuf, message: String },
    #[error("validation failed with {error_count} error(s)")]
    ValidationFailed {
        errors: Box<Vec<Diagnostic>>,
        warnings: Box<Vec<Diagnostic>>,
        error_count: usize,
    },
}

fn emit_observability(action: &str, outcome: &str, fields: serde_json::Value) {
    let Some(home_dir) = get_home_dir().ok() else {
        return;
    };
    let mut cfg = SharedLogConfig::from_home(&home_dir);
    cfg.log_path = home_dir
        .join(".config")
        .join("sc-composer")
        .join("logs")
        .join("sc-composer.log.jsonl");
    cfg.spool_dir = home_dir
        .join(".config")
        .join("sc-composer")
        .join("logs")
        .join("spool");
    let logger = SharedLogger::new(cfg);
    let mut event = LogEventV1::builder("sc-composer", action, "sc_composer::lib")
        .level("info")
        .build();
    event.outcome = Some(outcome.to_string());
    event.fields = json_to_map(fields);
    if let Some(team) = env_nonempty("ATM_TEAM") {
        event.team = Some(team);
    }
    if let Some(agent) = env_nonempty("ATM_IDENTITY") {
        event.agent = Some(agent);
    }
    if let Some(runtime) = env_nonempty("ATM_RUNTIME") {
        event.runtime = Some(runtime);
    }
    if let Some(session_id) = env_nonempty("ATM_SESSION_ID") {
        event.session_id = Some(session_id);
    }
    let _ = logger.emit(&event);
}

fn env_nonempty(key: &str) -> Option<String> {
    std::env::var(key).ok().and_then(|value| {
        let trimmed = value.trim();
        if trimmed.is_empty() {
            None
        } else {
            Some(trimmed.to_string())
        }
    })
}

fn json_to_map(value: serde_json::Value) -> serde_json::Map<String, serde_json::Value> {
    match value {
        serde_json::Value::Object(map) => map,
        other => {
            let mut map = serde_json::Map::new();
            map.insert("value".to_string(), other);
            map
        }
    }
}

/// Compose a final prompt output.
pub fn compose(request: &ComposeRequest) -> Result<ComposeResult, ComposerError> {
    let result = (|| {
        let prepared = prepare_template(request)?;
        let merge = evaluate_context(request, &prepared);

        let mut warnings = prepared.warnings;
        warnings.extend(merge.warnings);
        if !merge.errors.is_empty() {
            return Err(ComposerError::ValidationFailed {
                error_count: merge.errors.len(),
                errors: Box::new(merge.errors),
                warnings: Box::new(warnings),
            });
        }

        let profile_body = if frontmatter::is_template_file(&prepared.template_path) {
            render_template(&prepared.template_path, &prepared.body, &merge.context)?
        } else {
            prepared.body
        };
        let rendered_text = compose_blocks(
            &profile_body,
            request.guidance_block.as_deref(),
            request.user_prompt.as_deref(),
        );

        Ok(ComposeResult {
            rendered_text,
            resolved_files: prepared.resolved_files,
            search_trace: prepared.search_trace,
            variable_sources: merge.variable_sources,
            warnings,
        })
    })();

    match &result {
        Ok(composed) => emit_observability(
            "compose",
            "ok",
            serde_json::json!({
                "mode": format!("{:?}", request.mode),
                "runtime": format!("{:?}", request.runtime),
                "resolved_files": composed.resolved_files.len(),
                "warnings": composed.warnings.len(),
            }),
        ),
        Err(err) => emit_observability(
            "compose",
            "err",
            serde_json::json!({
                "mode": format!("{:?}", request.mode),
                "runtime": format!("{:?}", request.runtime),
                "error": err.to_string(),
            }),
        ),
    }

    result
}

/// Validate a compose request without producing output.
pub fn validate(request: &ComposeRequest) -> Result<ValidationReport, ComposerError> {
    let result = validate_request(request);
    match &result {
        Ok(report) => emit_observability(
            "validate",
            "ok",
            serde_json::json!({
                "mode": format!("{:?}", request.mode),
                "runtime": format!("{:?}", request.runtime),
                "errors": report.errors.len(),
                "warnings": report.warnings.len(),
            }),
        ),
        Err(err) => emit_observability(
            "validate",
            "err",
            serde_json::json!({
                "mode": format!("{:?}", request.mode),
                "runtime": format!("{:?}", request.runtime),
                "error": err.to_string(),
            }),
        ),
    }
    result
}

/// Resolve the input template/profile path and return full probe trace.
pub fn resolve(request: &ComposeRequest) -> Result<ResolveResult, ComposerError> {
    let result = resolver::resolve_input_path(request);
    match &result {
        Ok(resolved) => emit_observability(
            "resolve",
            "ok",
            serde_json::json!({
                "mode": format!("{:?}", request.mode),
                "runtime": format!("{:?}", request.runtime),
                "attempted_paths": resolved.attempted_paths.len(),
            }),
        ),
        Err(err) => emit_observability(
            "resolve",
            "err",
            serde_json::json!({
                "mode": format!("{:?}", request.mode),
                "runtime": format!("{:?}", request.runtime),
                "error": err.to_string(),
            }),
        ),
    }
    result
}

/// Discover template variables in Jinja content.
pub fn discover_template_variables(content: &str) -> Vec<String> {
    frontmatter::extract_template_variables(content)
        .into_iter()
        .collect()
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;
    use std::path::PathBuf;

    use agent_team_mail_core::logging_event::LogEventV1;
    use serial_test::serial;
    use tempfile::TempDir;

    use super::{
        ComposeMode, ComposePolicy, ComposeRequest, ComposerError, ProfileKind, RuntimeKind,
        UnknownVariablePolicy, compose, emit_observability, validate,
    };

    fn write_file(root: &TempDir, rel_path: &str, content: &str) -> PathBuf {
        let path = root.path().join(rel_path);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).expect("mkdir");
        }
        std::fs::write(&path, content).expect("write file");
        path
    }

    fn request(root: &TempDir, rel_path: &str) -> ComposeRequest {
        ComposeRequest {
            runtime: RuntimeKind::Claude,
            mode: ComposeMode::File,
            kind: None,
            root: root.path().to_path_buf(),
            agent: None,
            template_path: Some(PathBuf::from(rel_path)),
            vars_input: BTreeMap::new(),
            vars_env: BTreeMap::new(),
            guidance_block: None,
            user_prompt: None,
            policy: ComposePolicy::default(),
        }
    }

    #[test]
    fn compose_plain_text_passthrough() {
        let tmp = TempDir::new().expect("tempdir");
        write_file(&tmp, "plain.txt", "hello world");

        let result = compose(&request(&tmp, "plain.txt")).expect("compose");
        assert_eq!(result.rendered_text, "hello world");
    }

    #[test]
    fn compose_template_substitutes_vars() {
        let tmp = TempDir::new().expect("tempdir");
        write_file(&tmp, "template.md.j2", "hello {{ name }}");

        let mut req = request(&tmp, "template.md.j2");
        req.vars_input
            .insert("name".to_string(), "alex".to_string());
        let result = compose(&req).expect("compose");
        assert_eq!(result.rendered_text, "hello alex");
    }

    #[test]
    fn compose_missing_required_var_returns_missing_var_diagnostic() {
        let tmp = TempDir::new().expect("tempdir");
        write_file(
            &tmp,
            "template.md.j2",
            "---\nrequired_variables:\n  - name\n---\nhello {{ name }}",
        );

        let err = compose(&request(&tmp, "template.md.j2")).expect_err("missing var must fail");
        match err {
            ComposerError::ValidationFailed { errors, .. } => {
                assert!(
                    errors.iter().any(|d| d.code == "MISSING_VAR"),
                    "expected MISSING_VAR, got: {errors:?}"
                );
            }
            other => panic!("unexpected error type: {other}"),
        }
    }

    #[test]
    fn compose_missing_required_var_from_include_reports_include_chain() {
        let tmp = TempDir::new().expect("tempdir");
        write_file(
            &tmp,
            "base.md.j2",
            "---\nrequired_variables: []\n---\n@<partials/need_name.md.j2>\n",
        );
        write_file(
            &tmp,
            "partials/need_name.md.j2",
            "---\nrequired_variables:\n  - name\n---\nhello {{ name }}",
        );

        let err = compose(&request(&tmp, "base.md.j2")).expect_err("missing var must fail");
        match err {
            ComposerError::ValidationFailed { errors, .. } => {
                let missing = errors
                    .iter()
                    .find(|d| d.code == "MISSING_VAR")
                    .expect("expected MISSING_VAR");
                let diagnostic_path = missing
                    .path
                    .as_ref()
                    .expect("missing diagnostic path should be present");
                assert!(
                    diagnostic_path.ends_with("partials/need_name.md.j2"),
                    "diagnostic path should point to declaring include file: {missing:?}"
                );
                assert!(
                    missing.include_chain.len() >= 2,
                    "include chain should include root + include path: {missing:?}"
                );
            }
            other => panic!("unexpected error type: {other}"),
        }
    }

    #[test]
    fn compose_unknown_var_policy_error_warn_ignore() {
        let tmp = TempDir::new().expect("tempdir");
        write_file(
            &tmp,
            "template.md.j2",
            "---\nrequired_variables:\n  - name\n---\nhello {{ name }}",
        );

        let mut req = request(&tmp, "template.md.j2");
        req.vars_input
            .insert("name".to_string(), "alex".to_string());
        req.vars_input.insert("extra".to_string(), "x".to_string());
        req.policy.unknown_variable_policy = UnknownVariablePolicy::Error;

        let err = compose(&req).expect_err("unknown must fail for Error policy");
        match err {
            ComposerError::ValidationFailed { errors, .. } => {
                assert!(errors.iter().any(|d| d.code == "UNKNOWN_VAR"));
            }
            other => panic!("unexpected error type: {other}"),
        }

        req.policy.unknown_variable_policy = UnknownVariablePolicy::Warn;
        let ok = compose(&req).expect("warn policy should pass");
        assert!(ok.warnings.iter().any(|d| d.code == "UNKNOWN_VAR"));

        req.policy.unknown_variable_policy = UnknownVariablePolicy::Ignore;
        let ok = compose(&req).expect("ignore policy should pass");
        assert!(!ok.warnings.iter().any(|d| d.code == "UNKNOWN_VAR"));
    }

    #[test]
    fn compose_frontmatter_defaults_are_applied() {
        let tmp = TempDir::new().expect("tempdir");
        write_file(
            &tmp,
            "template.md.j2",
            "---\ndefaults:\n  role: engineer\nrequired_variables:\n  - role\n---\nrole={{ role }}",
        );
        let result = compose(&request(&tmp, "template.md.j2")).expect("compose");
        assert_eq!(result.rendered_text, "role=engineer");
    }

    #[test]
    fn validate_reports_missing_vars_without_rendering() {
        let tmp = TempDir::new().expect("tempdir");
        write_file(
            &tmp,
            "template.md.j2",
            "---\nrequired_variables:\n  - name\n---\nhello {{ name }}",
        );

        let report = validate(&request(&tmp, "template.md.j2")).expect("validate");
        assert!(!report.ok);
        assert!(report.errors.iter().any(|d| d.code == "MISSING_VAR"));
    }

    #[test]
    fn compose_profile_mode_resolves_and_applies_pipeline_order() {
        let tmp = TempDir::new().expect("tempdir");
        write_file(&tmp, ".codex/agents/rust-dev.md.j2", "role={{ role }}");

        let mut req = ComposeRequest {
            runtime: RuntimeKind::Codex,
            mode: ComposeMode::Profile,
            kind: Some(ProfileKind::Agent),
            root: tmp.path().to_path_buf(),
            agent: Some("rust-dev".to_string()),
            template_path: None,
            vars_input: BTreeMap::from([("role".to_string(), "coder".to_string())]),
            vars_env: BTreeMap::new(),
            guidance_block: Some("guidance".to_string()),
            user_prompt: Some("prompt".to_string()),
            policy: ComposePolicy::default(),
        };

        let result = compose(&req).expect("compose");
        assert_eq!(result.rendered_text, "role=coder\n\nguidance\n\nprompt");
        assert!(!result.search_trace.is_empty());

        req.template_path = Some(PathBuf::from("explicit.md.j2"));
        write_file(&tmp, "explicit.md.j2", "explicit");
        let explicit = compose(&req).expect("compose");
        assert_eq!(explicit.rendered_text, "explicit\n\nguidance\n\nprompt");
    }

    #[test]
    fn validate_profile_resolution_failure_reports_search_trace() {
        let tmp = TempDir::new().expect("tempdir");
        let req = ComposeRequest {
            runtime: RuntimeKind::Gemini,
            mode: ComposeMode::Profile,
            kind: Some(ProfileKind::Agent),
            root: tmp.path().to_path_buf(),
            agent: Some("missing-agent".to_string()),
            template_path: None,
            vars_input: BTreeMap::new(),
            vars_env: BTreeMap::new(),
            guidance_block: None,
            user_prompt: None,
            policy: ComposePolicy::default(),
        };

        let err = validate(&req).expect_err("missing profile should fail");
        match err {
            ComposerError::ProfileResolutionFailed {
                attempted_paths, ..
            } => {
                assert!(!attempted_paths.is_empty());
            }
            other => panic!("unexpected error: {other}"),
        }
    }

    #[test]
    fn compose_expands_includes_and_merges_include_frontmatter() {
        let tmp = TempDir::new().expect("tempdir");
        write_file(
            &tmp,
            "base.md.j2",
            "---\nrequired_variables:\n  - name\n---\n@<partials/greet.md.j2>\n",
        );
        write_file(
            &tmp,
            "partials/greet.md.j2",
            "---\ndefaults:\n  salutation: Hello\nrequired_variables:\n  - salutation\n---\n{{ salutation }} {{ name }}",
        );
        let mut req = request(&tmp, "base.md.j2");
        req.vars_input.insert("name".to_string(), "Kai".to_string());
        let result = compose(&req).expect("compose");
        assert_eq!(result.rendered_text.trim(), "Hello Kai");
    }

    #[test]
    #[serial]
    fn emit_observability_includes_env_correlation_fields() {
        let tmp = TempDir::new().expect("tempdir");
        // SAFETY: test-scoped env overrides.
        unsafe {
            std::env::set_var("ATM_HOME", tmp.path());
            std::env::set_var("ATM_TEAM", "atm-dev");
            std::env::set_var("ATM_IDENTITY", "arch-ctm");
            std::env::set_var("ATM_RUNTIME", "codex");
            std::env::set_var("ATM_SESSION_ID", "sess-123");
        }

        emit_observability("compose", "ok", serde_json::json!({"mode": "file"}));
        let log_path = tmp
            .path()
            .join(".config/sc-composer/logs/sc-composer.log.jsonl");
        let lines: Vec<String> = std::fs::read_to_string(&log_path)
            .expect("log file should exist")
            .lines()
            .filter(|l| !l.is_empty())
            .map(str::to_string)
            .collect();
        let event: LogEventV1 =
            serde_json::from_str(lines.last().expect("at least one log line should exist"))
                .expect("parse event");
        assert_eq!(event.team.as_deref(), Some("atm-dev"));
        assert_eq!(event.agent.as_deref(), Some("arch-ctm"));
        assert_eq!(event.runtime.as_deref(), Some("codex"));
        assert_eq!(event.session_id.as_deref(), Some("sess-123"));
        assert_eq!(event.outcome.as_deref(), Some("ok"));

        // SAFETY: cleanup after test.
        unsafe {
            std::env::remove_var("ATM_HOME");
            std::env::remove_var("ATM_TEAM");
            std::env::remove_var("ATM_IDENTITY");
            std::env::remove_var("ATM_RUNTIME");
            std::env::remove_var("ATM_SESSION_ID");
        }
    }
}
