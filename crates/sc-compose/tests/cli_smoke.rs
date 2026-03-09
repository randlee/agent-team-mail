use assert_cmd::cargo;
use assert_cmd::prelude::*;
use predicates::prelude::*;
use serde_json::Value;
use std::fs;
use std::process::Command;
use tempfile::TempDir;

fn run_sc_compose() -> Command {
    Command::new(cargo::cargo_bin!("sc-compose"))
}

#[test]
fn render_round_trip() {
    let tmp = TempDir::new().expect("tempdir");
    let template = tmp.path().join("template.md.j2");
    fs::write(&template, "hello {{ name }}").expect("write");

    run_sc_compose()
        .arg("--root")
        .arg(tmp.path())
        .arg("--var")
        .arg("name=Kai")
        .arg("render")
        .arg(&template)
        .assert()
        .success()
        .stdout(predicate::str::contains("hello Kai"));
}

#[test]
fn missing_var_exits_two() {
    let tmp = TempDir::new().expect("tempdir");
    let template = tmp.path().join("template.md.j2");
    fs::write(
        &template,
        "---\nrequired_variables:\n  - name\n---\nhello {{ name }}",
    )
    .expect("write");

    run_sc_compose()
        .arg("--root")
        .arg(tmp.path())
        .arg("validate")
        .arg(&template)
        .assert()
        .code(2);
}

#[test]
fn json_error_payload_is_valid_json() {
    let tmp = TempDir::new().expect("tempdir");
    let template = tmp.path().join("template.md.j2");
    fs::write(
        &template,
        "---\nrequired_variables:\n  - name\n---\nhello {{ name }}",
    )
    .expect("write");

    let out = run_sc_compose()
        .arg("--json")
        .arg("--root")
        .arg(tmp.path())
        .arg("validate")
        .arg(&template)
        .output()
        .expect("run");
    assert_eq!(out.status.code(), Some(2));
    let stderr = String::from_utf8(out.stderr).expect("utf8");
    let payload: Value = serde_json::from_str(stderr.trim()).expect("json stderr");
    assert_eq!(payload["errorCode"], "VALIDATION_FAILED");
}

#[test]
fn dry_run_render_does_not_write_output() {
    let tmp = TempDir::new().expect("tempdir");
    let template = tmp.path().join("template.md.j2");
    let out_file = tmp.path().join("out.md");
    fs::write(&template, "hello {{ name }}").expect("write");

    run_sc_compose()
        .arg("--root")
        .arg(tmp.path())
        .arg("--var")
        .arg("name=Kai")
        .arg("--dry-run")
        .arg("render")
        .arg(&template)
        .arg("--output")
        .arg(&out_file)
        .assert()
        .success();

    assert!(!out_file.exists(), "dry-run must not write output file");
}

#[test]
fn profile_validate_includes_search_trace_in_json_error() {
    let tmp = TempDir::new().expect("tempdir");

    let out = run_sc_compose()
        .arg("--json")
        .arg("--mode")
        .arg("profile")
        .arg("--kind")
        .arg("agent")
        .arg("--agent-type")
        .arg("missing-agent")
        .arg("--runtime")
        .arg("codex")
        .arg("--root")
        .arg(tmp.path())
        .arg("validate")
        .output()
        .expect("run");
    assert_eq!(out.status.code(), Some(2));
    let stderr = String::from_utf8(out.stderr).expect("utf8");
    let payload: Value = serde_json::from_str(stderr.trim()).expect("json stderr");
    let attempted = payload
        .get("attemptedPaths")
        .and_then(Value::as_array)
        .expect("attemptedPaths array");
    assert!(!attempted.is_empty(), "search trace should be populated");
}
