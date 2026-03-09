use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use crate::ComposerError;
use crate::diagnostics::Diagnostic;
use crate::frontmatter::{self, Frontmatter};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IncludeExpansionResult {
    pub body: String,
    pub included_files: Vec<PathBuf>,
    pub required_variables: BTreeSet<String>,
    pub required_variable_sources: BTreeMap<String, PathBuf>,
    pub defaults: BTreeMap<String, String>,
}

pub fn expand_includes(
    root: &Path,
    source_path: &Path,
    body: &str,
    policy_max_depth: usize,
    allowed_roots: &[PathBuf],
) -> Result<IncludeExpansionResult, ComposerError> {
    let mut state = IncludeState {
        root,
        max_depth: policy_max_depth,
        allowed_roots,
    };
    state.expand(source_path, body, 0, &mut vec![source_path.to_path_buf()])
}

struct IncludeState<'a> {
    root: &'a Path,
    max_depth: usize,
    allowed_roots: &'a [PathBuf],
}

impl<'a> IncludeState<'a> {
    fn expand(
        &mut self,
        source_path: &Path,
        body: &str,
        depth: usize,
        stack: &mut Vec<PathBuf>,
    ) -> Result<IncludeExpansionResult, ComposerError> {
        if depth > self.max_depth {
            return Err(ComposerError::IncludeError {
                diagnostic: Box::new(
                    Diagnostic::new(
                        "INCLUDE_DEPTH_EXCEEDED",
                        format!("Include depth exceeded max depth {}", self.max_depth),
                    )
                    .with_path(source_path.to_path_buf())
                    .with_include_chain(stack.clone()),
                ),
            });
        }

        let mut rendered = String::new();
        let mut included_files = Vec::new();
        let mut required_variables = BTreeSet::new();
        let mut required_variable_sources = BTreeMap::new();
        let mut defaults = BTreeMap::new();

        for (line_index, segment) in body.split_inclusive('\n').enumerate() {
            let line = segment.trim_end_matches('\n').trim_end_matches('\r');
            let has_newline = segment.ends_with('\n');
            if let Some(target_raw) = parse_include_directive(line) {
                let include_path =
                    self.resolve_include_path(source_path, target_raw, line_index + 1, stack)?;
                if stack.contains(&include_path) {
                    return Err(ComposerError::IncludeError {
                        diagnostic: Box::new(
                            Diagnostic::new(
                                "INCLUDE_CYCLE",
                                format!("Include cycle detected for {}", include_path.display()),
                            )
                            .with_path(include_path)
                            .with_position(line_index + 1, 1)
                            .with_include_chain(stack.clone()),
                        ),
                    });
                }

                let text = std::fs::read_to_string(&include_path).map_err(|source| {
                    ComposerError::TemplateRead {
                        path: include_path.clone(),
                        source,
                    }
                })?;
                let parsed = frontmatter::parse_document(&include_path, &text)?;
                let fm = parsed.frontmatter.unwrap_or_default();
                for var in fm.required_variables {
                    required_variables.insert(var.clone());
                    required_variable_sources
                        .entry(var)
                        .or_insert_with(|| include_path.clone());
                }
                for (k, v) in fm.defaults {
                    defaults.entry(k).or_insert(v);
                }

                stack.push(include_path.clone());
                let nested = self.expand(&include_path, &parsed.body, depth + 1, stack)?;
                stack.pop();

                required_variables.extend(nested.required_variables);
                for (k, v) in nested.required_variable_sources {
                    required_variable_sources.entry(k).or_insert(v);
                }
                for (k, v) in nested.defaults {
                    defaults.entry(k).or_insert(v);
                }
                included_files.push(include_path.clone());
                included_files.extend(nested.included_files);
                rendered.push_str(&nested.body);
                if has_newline && !rendered.ends_with('\n') {
                    rendered.push('\n');
                }
                continue;
            }
            rendered.push_str(segment);
        }

        Ok(IncludeExpansionResult {
            body: rendered,
            included_files,
            required_variables,
            required_variable_sources,
            defaults,
        })
    }

    fn resolve_include_path(
        &self,
        source_path: &Path,
        target_raw: &str,
        line: usize,
        stack: &[PathBuf],
    ) -> Result<PathBuf, ComposerError> {
        let target = PathBuf::from(target_raw);
        let mut candidates = Vec::new();
        let mut saw_out_of_root = false;
        if target.is_absolute() {
            candidates.push(target);
        } else {
            if let Some(parent) = source_path.parent() {
                candidates.push(parent.join(&target));
            }
            candidates.push(self.root.join(&target));
        }

        for candidate in candidates {
            let canonical = canonical_or_original(&candidate);
            if !self.is_allowed_root(&canonical) {
                saw_out_of_root = true;
                continue;
            }
            if canonical.exists() {
                return Ok(canonical);
            }
        }

        if saw_out_of_root {
            return Err(ComposerError::IncludeError {
                diagnostic: Box::new(
                    Diagnostic::new(
                        "ROOT_ESCAPE",
                        format!("Include path '{target_raw}' escapes allowed roots"),
                    )
                    .with_path(source_path.to_path_buf())
                    .with_position(line, 1)
                    .with_include_chain(stack.to_vec()),
                ),
            });
        }

        Err(ComposerError::IncludeError {
            diagnostic: Box::new(
                Diagnostic::new(
                    "INCLUDE_NOT_FOUND",
                    format!("Unable to resolve include '{target_raw}'"),
                )
                .with_path(source_path.to_path_buf())
                .with_position(line, 1)
                .with_include_chain(stack.to_vec()),
            ),
        })
    }

    fn is_allowed_root(&self, candidate: &Path) -> bool {
        let mut roots = vec![canonical_or_original(self.root)];
        roots.extend(self.allowed_roots.iter().map(|p| canonical_or_original(p)));
        roots.into_iter().any(|root| candidate.starts_with(root))
    }
}

fn parse_include_directive(line: &str) -> Option<&str> {
    let trimmed = line.trim();
    if !trimmed.starts_with("@<") || !trimmed.ends_with('>') {
        return None;
    }
    Some(trimmed.trim_start_matches("@<").trim_end_matches('>'))
}

fn canonical_or_original(path: &Path) -> PathBuf {
    std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf())
}

pub fn merge_frontmatter(
    parent: Option<Frontmatter>,
    parent_path: &Path,
    include_required: &BTreeSet<String>,
    include_required_sources: &BTreeMap<String, PathBuf>,
    include_defaults: &BTreeMap<String, String>,
) -> (
    Vec<String>,
    BTreeMap<String, String>,
    BTreeMap<String, PathBuf>,
) {
    let mut required = BTreeSet::new();
    let mut required_sources = BTreeMap::new();
    let mut defaults = BTreeMap::new();

    for (k, v) in include_defaults {
        defaults.insert(k.clone(), v.clone());
    }
    for var in include_required {
        required.insert(var.clone());
    }
    for (k, v) in include_required_sources {
        required_sources.insert(k.clone(), v.clone());
    }

    if let Some(parent_fm) = parent {
        for (k, v) in parent_fm.defaults {
            defaults.insert(k, v);
        }
        for var in parent_fm.required_variables {
            required.insert(var.clone());
            required_sources
                .entry(var)
                .or_insert_with(|| parent_path.to_path_buf());
        }
    }

    (required.into_iter().collect(), defaults, required_sources)
}

#[cfg(test)]
mod tests {
    use tempfile::TempDir;

    use super::*;

    #[test]
    fn include_cycle_detection_reports_chain() {
        let tmp = TempDir::new().expect("tempdir");
        let root = tmp.path();
        std::fs::write(root.join("a.md.j2"), "@<b.md.j2>\n").expect("write");
        std::fs::write(root.join("b.md.j2"), "@<a.md.j2>\n").expect("write");

        let err = expand_includes(root, &root.join("a.md.j2"), "@<b.md.j2>\n", 8, &[])
            .expect_err("cycle should fail");
        match err {
            ComposerError::IncludeError { diagnostic } => {
                assert_eq!(diagnostic.code, "INCLUDE_CYCLE");
                assert!(!diagnostic.include_chain.is_empty());
            }
            other => panic!("unexpected error: {other}"),
        }
    }

    #[test]
    fn include_respects_root_confinement() {
        let tmp = TempDir::new().expect("tempdir");
        let root = tmp.path();
        let outside = TempDir::new().expect("outside");
        std::fs::write(outside.path().join("secret.md"), "secret").expect("write");
        let source = root.join("main.md.j2");
        std::fs::write(
            &source,
            format!("@<{}>\n", outside.path().join("secret.md").display()),
        )
        .expect("write");
        let err = expand_includes(
            root,
            &source,
            &std::fs::read_to_string(&source).unwrap(),
            8,
            &[],
        )
        .expect_err("out of root include should fail");
        match err {
            ComposerError::IncludeError { diagnostic } => {
                assert_eq!(diagnostic.code, "ROOT_ESCAPE");
            }
            other => panic!("unexpected error: {other}"),
        }
    }

    #[test]
    fn include_depth_limit_returns_error() {
        let tmp = TempDir::new().expect("tempdir");
        let root = tmp.path();
        let max_depth = 2usize;

        std::fs::write(root.join("a.md.j2"), "@<b.md.j2>\n").expect("write a");
        std::fs::write(root.join("b.md.j2"), "@<c.md.j2>\n").expect("write b");
        std::fs::write(root.join("c.md.j2"), "@<d.md.j2>\n").expect("write c");
        std::fs::write(root.join("d.md.j2"), "done\n").expect("write d");

        let err = expand_includes(root, &root.join("a.md.j2"), "@<b.md.j2>\n", max_depth, &[])
            .expect_err("depth limit should fail");
        match err {
            ComposerError::IncludeError { diagnostic } => {
                assert_eq!(diagnostic.code, "INCLUDE_DEPTH_EXCEEDED");
            }
            other => panic!("unexpected error: {other}"),
        }
    }
}
