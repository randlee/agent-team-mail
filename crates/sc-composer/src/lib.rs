//! sc-composer: standalone composition engine for prompt/template pipelines.
//!
//! This crate intentionally stays runtime-agnostic so it can be reused by ATM
//! and non-ATM tools.

mod context;
mod frontmatter;
mod render;

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use context::merge_context;
use frontmatter::{extract_template_variables, frontmatter_missing_warning, parse_document};
use render::render_template;

/// Supported runtime profiles for default agent file resolution policy.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RuntimeKind {
    Claude,
    Codex,
    Gemini,
    Opencode,
    Custom,
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
            max_include_depth: 16,
            allowed_roots: Vec::new(),
        }
    }
}

/// Input request for compose/validate operations.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ComposeRequest {
    pub runtime: RuntimeKind,
    pub root: PathBuf,
    pub agent: Option<String>,
    pub template_path: Option<PathBuf>,
    pub vars_input: BTreeMap<String, String>,
    pub vars_env: BTreeMap<String, String>,
    pub guidance_block: Option<String>,
    pub user_prompt: Option<String>,
    pub policy: ComposePolicy,
}

/// Non-fatal note produced during resolution/validation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Diagnostic {
    pub code: String,
    pub message: String,
    pub path: Option<PathBuf>,
}

/// Result of a successful compose operation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ComposeResult {
    pub rendered_text: String,
    pub resolved_files: Vec<PathBuf>,
    pub variable_sources: BTreeMap<String, VariableSource>,
    pub warnings: Vec<Diagnostic>,
}

/// Result of validate-only operation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ValidationReport {
    pub ok: bool,
    pub warnings: Vec<Diagnostic>,
    pub errors: Vec<Diagnostic>,
}

/// Stable error surface for composition failures.
#[derive(Debug, thiserror::Error)]
pub enum ComposerError {
    #[error("template path is required for compose/validate in file mode")]
    MissingTemplatePath,
    #[error("failed to read template at {path}: {source}")]
    TemplateRead {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("invalid frontmatter in {path}: {message}")]
    FrontmatterParse { path: PathBuf, message: String },
    #[error("template parse/render failed in {path}: {message}")]
    TemplateRender { path: PathBuf, message: String },
    #[error("validation failed with {error_count} error(s)")]
    ValidationFailed {
        errors: Vec<Diagnostic>,
        warnings: Vec<Diagnostic>,
        error_count: usize,
    },
}

/// Compose a final prompt output.
pub fn compose(request: &ComposeRequest) -> Result<ComposeResult, ComposerError> {
    let template_path = resolve_template_path(request)?;
    let raw =
        std::fs::read_to_string(&template_path).map_err(|source| ComposerError::TemplateRead {
            path: template_path.clone(),
            source,
        })?;

    let parsed = parse_document(&template_path, &raw)?;
    let (required_variables, declared_variables, defaults, mut warnings) =
        effective_schema(&template_path, &parsed.frontmatter, &parsed.body);

    let merge = merge_context(
        &template_path,
        &required_variables,
        &declared_variables,
        &defaults,
        &request.vars_env,
        &request.vars_input,
        request.policy.unknown_variable_policy,
    );
    warnings.extend(merge.warnings);

    if !merge.errors.is_empty() {
        return Err(ComposerError::ValidationFailed {
            error_count: merge.errors.len(),
            errors: merge.errors,
            warnings,
        });
    }

    let rendered_text = if frontmatter::is_template_file(&template_path) {
        render_template(&template_path, &parsed.body, &merge.context)?
    } else {
        parsed.body
    };

    Ok(ComposeResult {
        rendered_text,
        resolved_files: vec![template_path],
        variable_sources: merge.variable_sources,
        warnings,
    })
}

/// Validate a compose request without producing output.
pub fn validate(request: &ComposeRequest) -> Result<ValidationReport, ComposerError> {
    let template_path = resolve_template_path(request)?;
    let raw =
        std::fs::read_to_string(&template_path).map_err(|source| ComposerError::TemplateRead {
            path: template_path.clone(),
            source,
        })?;

    let parsed = parse_document(&template_path, &raw)?;
    let (required_variables, declared_variables, defaults, mut warnings) =
        effective_schema(&template_path, &parsed.frontmatter, &parsed.body);

    let merge = merge_context(
        &template_path,
        &required_variables,
        &declared_variables,
        &defaults,
        &request.vars_env,
        &request.vars_input,
        request.policy.unknown_variable_policy,
    );
    warnings.extend(merge.warnings);

    Ok(ValidationReport {
        ok: merge.errors.is_empty(),
        warnings,
        errors: merge.errors,
    })
}

fn resolve_template_path(request: &ComposeRequest) -> Result<PathBuf, ComposerError> {
    let template_path = request
        .template_path
        .as_ref()
        .ok_or(ComposerError::MissingTemplatePath)?;
    if template_path.is_absolute() {
        Ok(template_path.clone())
    } else {
        Ok(request.root.join(template_path))
    }
}

fn effective_schema(
    template_path: &Path,
    frontmatter: &Option<frontmatter::Frontmatter>,
    body: &str,
) -> (
    Vec<String>,
    BTreeSet<String>,
    BTreeMap<String, String>,
    Vec<Diagnostic>,
) {
    if let Some(fm) = frontmatter {
        let mut declared = BTreeSet::new();
        declared.extend(fm.required_variables.iter().cloned());
        declared.extend(fm.defaults.keys().cloned());
        return (
            fm.required_variables.clone(),
            declared,
            fm.defaults.clone(),
            Vec::new(),
        );
    }

    if frontmatter::is_template_file(template_path) {
        let discovered = extract_template_variables(body);
        let required = discovered.iter().cloned().collect::<Vec<_>>();
        let warning = frontmatter_missing_warning(template_path);
        return (required, discovered, BTreeMap::new(), vec![warning]);
    }

    (Vec::new(), BTreeSet::new(), BTreeMap::new(), Vec::new())
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;
    use std::path::PathBuf;

    use tempfile::TempDir;

    use super::{
        ComposePolicy, ComposeRequest, ComposerError, RuntimeKind, UnknownVariablePolicy, compose,
        validate,
    };

    fn write_file(root: &TempDir, rel_path: &str, content: &str) -> PathBuf {
        let path = root.path().join(rel_path);
        std::fs::write(&path, content).expect("write file");
        path
    }

    fn request(root: &TempDir, rel_path: &str) -> ComposeRequest {
        ComposeRequest {
            runtime: RuntimeKind::Claude,
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
}
