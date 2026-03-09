use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use serde::Deserialize;

use crate::{ComposerError, Diagnostic};

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Frontmatter {
    pub required_variables: Vec<String>,
    pub defaults: BTreeMap<String, String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParsedDocument {
    pub frontmatter: Option<Frontmatter>,
    pub body: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
struct FrontmatterYaml {
    #[serde(default)]
    required_variables: Vec<String>,
    #[serde(default)]
    defaults: BTreeMap<String, String>,
}

pub fn parse_document(path: &Path, text: &str) -> Result<ParsedDocument, ComposerError> {
    let mut segments = text.split_inclusive('\n');
    let Some(first) = segments.next() else {
        return Ok(ParsedDocument {
            frontmatter: None,
            body: String::new(),
        });
    };

    if normalize_line(first) != "---" {
        return Ok(ParsedDocument {
            frontmatter: None,
            body: text.to_string(),
        });
    }

    let mut yaml = String::new();
    let mut offset = first.len();
    let mut closing_found = false;

    for segment in segments {
        let normalized = normalize_line(segment);
        offset += segment.len();
        if normalized == "---" {
            closing_found = true;
            break;
        }
        yaml.push_str(segment);
    }

    if !closing_found {
        return Err(ComposerError::FrontmatterParse {
            path: path.to_path_buf(),
            message: "missing closing frontmatter delimiter '---'".to_string(),
        });
    }

    let fm: FrontmatterYaml =
        serde_yaml::from_str(&yaml).map_err(|err| ComposerError::FrontmatterParse {
            path: path.to_path_buf(),
            message: err.to_string(),
        })?;

    let body = text[offset..].to_string();
    Ok(ParsedDocument {
        frontmatter: Some(Frontmatter {
            required_variables: fm.required_variables,
            defaults: fm.defaults,
        }),
        body,
    })
}

pub fn is_template_file(path: &Path) -> bool {
    path.file_name()
        .and_then(|name| name.to_str())
        .is_some_and(|name| name.ends_with(".j2"))
}

pub fn frontmatter_missing_warning(path: &Path) -> Diagnostic {
    Diagnostic {
        code: "MISSING_FRONTMATTER".to_string(),
        message: format!(
            "Template has no frontmatter. Run: sc-compose frontmatter-init {}",
            path.display()
        ),
        path: Some(path.to_path_buf()),
    }
}

pub fn extract_template_variables(body: &str) -> BTreeSet<String> {
    let mut vars = BTreeSet::new();
    let mut remaining = body;
    while let Some(start) = remaining.find("{{") {
        let after_start = &remaining[start + 2..];
        let Some(end) = after_start.find("}}") else {
            break;
        };
        let expr = after_start[..end].trim();
        if let Some(name) = extract_identifier(expr) {
            vars.insert(name.to_string());
        }
        remaining = &after_start[end + 2..];
    }
    vars
}

fn normalize_line(line: &str) -> &str {
    line.trim_end_matches('\n').trim_end_matches('\r')
}

fn extract_identifier(expr: &str) -> Option<&str> {
    let first = expr
        .split(['|', ' ', '.', '(', ')', '[', ']', ',', ':'])
        .find(|token| !token.is_empty())?;

    if is_identifier(first) {
        Some(first)
    } else {
        None
    }
}

fn is_identifier(token: &str) -> bool {
    let mut chars = token.chars();
    let Some(first) = chars.next() else {
        return false;
    };
    if !(first == '_' || first.is_ascii_alphabetic()) {
        return false;
    }
    chars.all(|c| c == '_' || c.is_ascii_alphanumeric())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_document_without_frontmatter() {
        let path = Path::new("example.md.j2");
        let parsed = parse_document(path, "hello {{name}}").expect("parse should succeed");
        assert!(parsed.frontmatter.is_none());
        assert_eq!(parsed.body, "hello {{name}}");
    }

    #[test]
    fn parse_document_with_frontmatter() {
        let path = Path::new("example.md.j2");
        let parsed = parse_document(
            path,
            "---\nrequired_variables:\n  - name\ndefaults:\n  role: dev\n---\nhello {{name}}",
        )
        .expect("parse should succeed");
        let fm = parsed.frontmatter.expect("frontmatter should be present");
        assert_eq!(fm.required_variables, vec!["name".to_string()]);
        assert_eq!(fm.defaults.get("role"), Some(&"dev".to_string()));
        assert_eq!(parsed.body, "hello {{name}}");
    }

    #[test]
    fn parse_document_missing_closing_delimiter_errors() {
        let path = Path::new("example.md.j2");
        let err = parse_document(path, "---\nrequired_variables:\n  - name\nhello")
            .expect_err("must fail");
        match err {
            ComposerError::FrontmatterParse { .. } => {}
            other => panic!("unexpected error: {other}"),
        }
    }

    #[test]
    fn extract_template_variables_finds_identifiers() {
        let vars = extract_template_variables("a {{ name }} {{_role|upper}} {{user.id}}");
        assert!(vars.contains("name"));
        assert!(vars.contains("_role"));
        assert!(vars.contains("user"));
    }
}
