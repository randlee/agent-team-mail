use std::collections::BTreeMap;
use std::path::PathBuf;

use sc_composer::{
    ComposeMode, ComposePolicy, ComposeRequest, ComposerError, ProfileKind, RuntimeKind, compose,
};
use tempfile::TempDir;

fn write_file(root: &TempDir, rel_path: &str, content: &str) -> PathBuf {
    let path = root.path().join(rel_path);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).expect("create parent directories");
    }
    std::fs::write(&path, content).expect("write test file");
    path
}

fn file_request(root: &TempDir, rel_path: &str) -> ComposeRequest {
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
fn file_mode_end_to_end_renders_markdown_template_with_vars() {
    let tmp = TempDir::new().expect("tempdir");
    write_file(
        &tmp,
        "prompts/dev.md.j2",
        "---\nrequired_variables:\n  - name\n---\nHello {{ name }}",
    );

    let mut request = file_request(&tmp, "prompts/dev.md.j2");
    request
        .vars_input
        .insert("name".to_string(), "Kai".to_string());
    let result = compose(&request).expect("compose should succeed");

    assert_eq!(result.rendered_text, "Hello Kai");
}

#[test]
fn profile_mode_end_to_end_resolves_runtime_profile_and_renders() {
    let tmp = TempDir::new().expect("tempdir");
    write_file(
        &tmp,
        ".codex/agents/rust-dev.md.j2",
        "---\nrequired_variables:\n  - role\n---\nRole={{ role }}",
    );

    let mut vars = BTreeMap::new();
    vars.insert("role".to_string(), "reviewer".to_string());
    let request = ComposeRequest {
        runtime: RuntimeKind::Codex,
        mode: ComposeMode::Profile,
        kind: Some(ProfileKind::Agent),
        root: tmp.path().to_path_buf(),
        agent: Some("rust-dev".to_string()),
        template_path: None,
        vars_input: vars,
        vars_env: BTreeMap::new(),
        guidance_block: None,
        user_prompt: None,
        policy: ComposePolicy::default(),
    };

    let result = compose(&request).expect("profile compose should succeed");
    assert_eq!(result.rendered_text, "Role=reviewer");
    assert!(
        result
            .resolved_files
            .first()
            .is_some_and(|path| path.ends_with(".codex/agents/rust-dev.md.j2"))
    );
}

#[test]
fn include_expansion_end_to_end_merges_included_content() {
    let tmp = TempDir::new().expect("tempdir");
    write_file(&tmp, "partials/shared.md.j2", "Shared for {{ name }}");
    write_file(
        &tmp,
        "base.md.j2",
        "@<partials/shared.md.j2>\nMain {{ name }}",
    );

    let mut request = file_request(&tmp, "base.md.j2");
    request
        .vars_input
        .insert("name".to_string(), "Kai".to_string());
    let result = compose(&request).expect("compose with include should succeed");

    assert_eq!(result.rendered_text, "Shared for Kai\nMain Kai");
    assert!(
        result
            .resolved_files
            .iter()
            .any(|path| path.ends_with("partials/shared.md.j2"))
    );
}

#[test]
fn error_paths_report_missing_var_cycle_and_out_of_root() {
    let missing = TempDir::new().expect("tempdir");
    write_file(
        &missing,
        "missing.md.j2",
        "---\nrequired_variables:\n  - name\n---\nHello {{ name }}",
    );
    let missing_err = compose(&file_request(&missing, "missing.md.j2")).expect_err("must fail");
    match missing_err {
        ComposerError::ValidationFailed { errors, .. } => {
            assert!(
                errors
                    .iter()
                    .any(|diagnostic| diagnostic.code == "MISSING_VAR"),
                "expected MISSING_VAR diagnostic"
            );
        }
        other => panic!("unexpected error type: {other}"),
    }

    let cycle = TempDir::new().expect("tempdir");
    write_file(&cycle, "a.md.j2", "@<b.md.j2>\n");
    write_file(&cycle, "b.md.j2", "@<a.md.j2>\n");
    let cycle_err = compose(&file_request(&cycle, "a.md.j2")).expect_err("cycle must fail");
    match cycle_err {
        ComposerError::IncludeError { diagnostic } => {
            assert_eq!(diagnostic.code, "INCLUDE_CYCLE");
        }
        other => panic!("unexpected error type: {other}"),
    }

    let confined = TempDir::new().expect("tempdir");
    let outside = TempDir::new().expect("outside");
    let outside_file = write_file(&outside, "secret.md", "classified");
    write_file(
        &confined,
        "main.md.j2",
        &format!("@<{}>\n", outside_file.display()),
    );
    let root_escape_err =
        compose(&file_request(&confined, "main.md.j2")).expect_err("root escape must fail");
    match root_escape_err {
        ComposerError::IncludeError { diagnostic } => {
            assert_eq!(diagnostic.code, "ROOT_ESCAPE");
        }
        other => panic!("unexpected error type: {other}"),
    }
}

#[test]
fn cross_platform_tempdir_absolute_paths_are_supported() {
    let tmp = TempDir::new().expect("tempdir");
    let template_path = write_file(&tmp, "absolute.md.j2", "Absolute {{ value }}");

    let mut request = ComposeRequest {
        runtime: RuntimeKind::Claude,
        mode: ComposeMode::File,
        kind: None,
        root: tmp.path().to_path_buf(),
        agent: None,
        template_path: Some(template_path.clone()),
        vars_input: BTreeMap::new(),
        vars_env: BTreeMap::new(),
        guidance_block: None,
        user_prompt: None,
        policy: ComposePolicy::default(),
    };
    request
        .vars_input
        .insert("value".to_string(), "OK".to_string());

    let result = compose(&request).expect("absolute tempdir path should compose");
    assert_eq!(result.rendered_text, "Absolute OK");
    assert!(
        result
            .resolved_files
            .first()
            .is_some_and(|resolved| resolved == &template_path)
    );
}
