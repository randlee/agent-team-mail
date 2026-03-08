//! sc-composer: standalone composition engine for prompt/template pipelines.
//!
//! This crate intentionally stays runtime-agnostic so it can be reused by ATM
//! and non-ATM tools.

use std::collections::BTreeMap;
use std::path::PathBuf;

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
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ComposerError {
    #[error("not implemented: {0}")]
    NotImplemented(&'static str),
}

/// Compose a final prompt output.
///
/// Placeholder API surface for upcoming implementation.
pub fn compose(_request: &ComposeRequest) -> Result<ComposeResult, ComposerError> {
    Err(ComposerError::NotImplemented("compose"))
}

/// Validate a compose request without producing output.
///
/// Placeholder API surface for upcoming implementation.
pub fn validate(_request: &ComposeRequest) -> Result<ValidationReport, ComposerError> {
    Err(ComposerError::NotImplemented("validate"))
}
