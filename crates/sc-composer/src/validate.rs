use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;

use crate::context::{ContextMergeReport, merge_context};
use crate::diagnostics::Diagnostic;
use crate::frontmatter;
use crate::include::{expand_includes, merge_frontmatter};
use crate::resolver::{ResolveResult, resolve_input_path};
use crate::{ComposeRequest, ComposerError};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ValidationReport {
    pub ok: bool,
    pub warnings: Vec<Diagnostic>,
    pub errors: Vec<Diagnostic>,
    pub search_trace: Vec<PathBuf>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PreparedTemplate {
    pub template_path: PathBuf,
    pub body: String,
    pub required_variables: Vec<String>,
    pub declared_variables: BTreeSet<String>,
    pub defaults: BTreeMap<String, String>,
    pub warnings: Vec<Diagnostic>,
    pub resolved_files: Vec<PathBuf>,
    pub search_trace: Vec<PathBuf>,
}

pub fn prepare_template(request: &ComposeRequest) -> Result<PreparedTemplate, ComposerError> {
    let ResolveResult {
        resolved_path,
        attempted_paths,
    } = resolve_input_path(request)?;
    let raw =
        std::fs::read_to_string(&resolved_path).map_err(|source| ComposerError::TemplateRead {
            path: resolved_path.clone(),
            source,
        })?;
    let parsed = frontmatter::parse_document(&resolved_path, &raw)?;
    let mut warnings = Vec::new();

    let include_result = expand_includes(
        &request.root,
        &resolved_path,
        &parsed.body,
        request.policy.max_include_depth,
        &request.policy.allowed_roots,
    )?;
    let (required_variables, defaults) = merge_frontmatter(
        parsed.frontmatter,
        &include_result.required_variables,
        &include_result.defaults,
    );
    let declared_variables: BTreeSet<String> =
        if required_variables.is_empty() && defaults.is_empty() {
            if frontmatter::is_template_file(&resolved_path) {
                let discovered = frontmatter::extract_template_variables(&include_result.body);
                warnings.push(frontmatter::frontmatter_missing_warning(&resolved_path));
                discovered
            } else {
                BTreeSet::new()
            }
        } else {
            let mut declared = BTreeSet::new();
            declared.extend(required_variables.iter().cloned());
            declared.extend(defaults.keys().cloned());
            declared
        };

    let required_variables = if required_variables.is_empty() && defaults.is_empty() {
        declared_variables.iter().cloned().collect()
    } else {
        required_variables
    };

    let mut resolved_files = vec![resolved_path.clone()];
    resolved_files.extend(include_result.included_files);

    Ok(PreparedTemplate {
        template_path: resolved_path,
        body: include_result.body,
        required_variables,
        declared_variables,
        defaults,
        warnings,
        resolved_files,
        search_trace: attempted_paths,
    })
}

pub fn evaluate_context(
    request: &ComposeRequest,
    prepared: &PreparedTemplate,
) -> ContextMergeReport {
    merge_context(
        &prepared.template_path,
        &prepared.resolved_files,
        &prepared.required_variables,
        &prepared.declared_variables,
        &prepared.defaults,
        &request.vars_env,
        &request.vars_input,
        request.policy.unknown_variable_policy,
    )
}

pub fn validate_request(request: &ComposeRequest) -> Result<ValidationReport, ComposerError> {
    let prepared = prepare_template(request)?;
    let merge = evaluate_context(request, &prepared);
    let mut warnings = prepared.warnings;
    warnings.extend(merge.warnings);
    Ok(ValidationReport {
        ok: merge.errors.is_empty(),
        warnings,
        errors: merge.errors,
        search_trace: prepared.search_trace,
    })
}
