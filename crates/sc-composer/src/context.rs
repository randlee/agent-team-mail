use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use crate::diagnostics::Diagnostic;
use crate::{UnknownVariablePolicy, VariableSource};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ContextMergeReport {
    pub context: BTreeMap<String, String>,
    pub variable_sources: BTreeMap<String, VariableSource>,
    pub warnings: Vec<Diagnostic>,
    pub errors: Vec<Diagnostic>,
}

#[expect(
    clippy::too_many_arguments,
    reason = "context merge needs explicit policy/config inputs for deterministic diagnostics"
)]
pub fn merge_context(
    template_path: &Path,
    include_chain: &[std::path::PathBuf],
    required_variables: &[String],
    required_variable_sources: &BTreeMap<String, std::path::PathBuf>,
    declared_variables: &BTreeSet<String>,
    defaults: &BTreeMap<String, String>,
    vars_env: &BTreeMap<String, String>,
    vars_input: &BTreeMap<String, String>,
    unknown_policy: UnknownVariablePolicy,
) -> ContextMergeReport {
    let mut context = BTreeMap::new();
    let mut variable_sources = BTreeMap::new();
    let mut warnings = Vec::new();
    let mut errors = Vec::new();

    for (key, value) in defaults {
        context.insert(key.clone(), value.clone());
        variable_sources.insert(key.clone(), VariableSource::Default);
    }

    for (key, value) in vars_env {
        context.insert(key.clone(), value.clone());
        variable_sources.insert(key.clone(), VariableSource::Env);
    }

    for (key, value) in vars_input {
        context.insert(key.clone(), value.clone());
        variable_sources.insert(key.clone(), VariableSource::Input);
    }

    for required in required_variables {
        if !context.contains_key(required) {
            errors.push(Diagnostic {
                code: "MISSING_VAR".to_string(),
                message: format!("Required variable '{required}' is missing"),
                path: Some(
                    required_variable_sources
                        .get(required)
                        .cloned()
                        .unwrap_or_else(|| template_path.to_path_buf()),
                ),
                line: None,
                column: None,
                include_chain: include_chain.to_vec(),
            });
        }
    }

    if !declared_variables.is_empty() {
        for key in vars_input.keys() {
            if declared_variables.contains(key) {
                continue;
            }

            let diagnostic = Diagnostic {
                code: "UNKNOWN_VAR".to_string(),
                message: format!("Input variable '{key}' is not declared by template/frontmatter"),
                path: Some(template_path.to_path_buf()),
                line: None,
                column: None,
                include_chain: include_chain.to_vec(),
            };

            match unknown_policy {
                UnknownVariablePolicy::Error => errors.push(diagnostic),
                UnknownVariablePolicy::Warn => warnings.push(diagnostic),
                UnknownVariablePolicy::Ignore => {}
            }
        }
    }

    ContextMergeReport {
        context,
        variable_sources,
        warnings,
        errors,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn merge_context_applies_precedence() {
        let path = Path::new("template.md.j2");
        let required = vec!["name".to_string(), "role".to_string()];
        let declared = BTreeSet::from(["name".to_string(), "role".to_string()]);
        let defaults = BTreeMap::from([
            ("name".to_string(), "default".to_string()),
            ("role".to_string(), "reader".to_string()),
        ]);
        let env = BTreeMap::from([("name".to_string(), "env".to_string())]);
        let input = BTreeMap::from([("name".to_string(), "input".to_string())]);

        let report = merge_context(
            path,
            &[],
            &required,
            &BTreeMap::new(),
            &declared,
            &defaults,
            &env,
            &input,
            UnknownVariablePolicy::Error,
        );

        assert!(report.errors.is_empty());
        assert_eq!(report.context.get("name"), Some(&"input".to_string()));
        assert_eq!(report.context.get("role"), Some(&"reader".to_string()));
        assert_eq!(
            report.variable_sources.get("name"),
            Some(&VariableSource::Input)
        );
        assert_eq!(
            report.variable_sources.get("role"),
            Some(&VariableSource::Default)
        );
    }
}
