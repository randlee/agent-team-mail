use assert_cmd::cargo;
use assert_cmd::prelude::*;
use predicates::prelude::*;
use serde_json::Value;
use std::fs;
use std::path::Path;
use std::process::Command;
use tempfile::TempDir;

fn run_sc_compose() -> Command {
    Command::new(cargo::cargo_bin!("sc-compose"))
}

fn read_log_events(path: &Path) -> Vec<Value> {
    fs::read_to_string(path)
        .expect("log file should be readable")
        .lines()
        .map(|line| serde_json::from_str::<Value>(line).expect("each log line must be valid json"))
        .collect()
}

fn read_log_lines(path: &Path) -> Vec<String> {
    fs::read_to_string(path)
        .expect("log file should be readable")
        .lines()
        .map(str::to_string)
        .collect()
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
fn render_write_derives_output_path_for_common_j2_suffixes() {
    let tmp = TempDir::new().expect("tempdir");
    let template_md = tmp.path().join("template.md.j2");
    let template_xml = tmp.path().join("layout.xml.j2");
    let template_txt = tmp.path().join("notes.txt.j2");
    let template_plain = tmp.path().join("note.j2");
    fs::write(&template_md, "hello {{ name }}").expect("write md");
    fs::write(&template_xml, "<r>{{ name }}</r>").expect("write xml");
    fs::write(&template_txt, "{{ name }} text").expect("write txt");
    fs::write(&template_plain, "{{ name }}").expect("write plain");

    run_sc_compose()
        .arg("--root")
        .arg(tmp.path())
        .arg("--var")
        .arg("name=Kai")
        .arg("render")
        .arg(&template_md)
        .arg("--write")
        .assert()
        .success();
    run_sc_compose()
        .arg("--root")
        .arg(tmp.path())
        .arg("--var")
        .arg("name=Kai")
        .arg("render")
        .arg(&template_xml)
        .arg("--write")
        .assert()
        .success();
    run_sc_compose()
        .arg("--root")
        .arg(tmp.path())
        .arg("--var")
        .arg("name=Kai")
        .arg("render")
        .arg(&template_txt)
        .arg("--write")
        .assert()
        .success();
    run_sc_compose()
        .arg("--root")
        .arg(tmp.path())
        .arg("--var")
        .arg("name=Kai")
        .arg("render")
        .arg(&template_plain)
        .arg("--write")
        .assert()
        .success();

    assert_eq!(
        fs::read_to_string(tmp.path().join("template.md")).expect("read md output"),
        "hello Kai"
    );
    assert_eq!(
        fs::read_to_string(tmp.path().join("layout.xml")).expect("read xml output"),
        "<r>Kai</r>"
    );
    assert_eq!(
        fs::read_to_string(tmp.path().join("notes.txt")).expect("read txt output"),
        "Kai text"
    );
    assert_eq!(
        fs::read_to_string(tmp.path().join("note")).expect("read plain output"),
        "Kai"
    );
}

#[test]
fn render_write_profile_derives_prompts_output() {
    let tmp = TempDir::new().expect("tempdir");
    let profile = tmp.path().join(".codex/agents/rust-dev.md.j2");
    fs::create_dir_all(profile.parent().expect("profile parent")).expect("mkdir");
    fs::write(&profile, "role={{ role }}").expect("write profile");

    run_sc_compose()
        .arg("--mode")
        .arg("profile")
        .arg("--kind")
        .arg("agent")
        .arg("--agent-type")
        .arg("rust-dev")
        .arg("--runtime")
        .arg("codex")
        .arg("--root")
        .arg(tmp.path())
        .arg("--var")
        .arg("role=coder")
        .arg("render")
        .arg("--write")
        .assert()
        .success();

    let prompts_dir = tmp.path().join(".prompts");
    let files: Vec<_> = fs::read_dir(&prompts_dir)
        .expect("read prompts dir")
        .filter_map(|entry| entry.ok().map(|e| e.path()))
        .collect();
    assert_eq!(files.len(), 1, "expected one derived profile output");
    let file_name = files[0]
        .file_name()
        .and_then(|n| n.to_str())
        .expect("utf8 filename");
    assert!(
        file_name.starts_with("rust-dev-") && file_name.ends_with(".md"),
        "unexpected prompts filename: {file_name}"
    );
    assert_eq!(
        fs::read_to_string(&files[0]).expect("read prompts output"),
        "role=coder"
    );
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
fn json_missing_var_diagnostic_includes_path_and_include_chain() {
    let tmp = TempDir::new().expect("tempdir");
    let include = tmp.path().join("partials/need_name.md.j2");
    fs::create_dir_all(include.parent().expect("include parent")).expect("mkdir");
    fs::write(
        tmp.path().join("base.md.j2"),
        "---\nrequired_variables: []\n---\n@<partials/need_name.md.j2>\n",
    )
    .expect("write base");
    fs::write(
        &include,
        "---\nrequired_variables:\n  - name\n---\nhello {{ name }}",
    )
    .expect("write include");

    let out = run_sc_compose()
        .arg("--json")
        .arg("--root")
        .arg(tmp.path())
        .arg("validate")
        .arg(tmp.path().join("base.md.j2"))
        .output()
        .expect("run");
    assert_eq!(out.status.code(), Some(2));
    let stderr = String::from_utf8(out.stderr).expect("utf8");
    let payload: Value = serde_json::from_str(stderr.trim()).expect("json stderr");
    let errors = payload["errors"].as_array().expect("errors array");
    let missing = errors
        .iter()
        .find(|diag| diag["code"] == "MISSING_VAR")
        .expect("MISSING_VAR diagnostic");

    assert!(
        missing["path"].is_string(),
        "diagnostic path missing: {missing:?}"
    );
    let diagnostic_path = missing["path"]
        .as_str()
        .expect("diagnostic path should be string");
    assert!(
        Path::new(diagnostic_path).ends_with(Path::new("partials/need_name.md.j2")),
        "diagnostic path should identify declaring include file: {missing:?}"
    );
    let include_chain = missing["include_chain"]
        .as_array()
        .expect("include_chain array");
    assert!(
        include_chain.len() >= 2,
        "include_chain should include root and include files: {missing:?}"
    );
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

#[test]
fn command_end_is_logged_on_success() {
    let tmp = TempDir::new().expect("tempdir");
    let template = tmp.path().join("template.md.j2");
    let log_path = tmp.path().join("sc-compose.log");
    fs::write(&template, "hello {{ name }}").expect("write");

    run_sc_compose()
        .env("SC_COMPOSE_LOG_FILE", &log_path)
        .arg("--root")
        .arg(tmp.path())
        .arg("--var")
        .arg("name=Kai")
        .arg("render")
        .arg(&template)
        .assert()
        .success();

    let events = read_log_events(&log_path);
    assert!(
        events
            .iter()
            .any(|event| event["action"] == "command_end" && event["outcome"] == "success"),
        "command_end success event missing: {events:?}"
    );
}

#[test]
fn include_expansion_events_are_logged_for_success_and_failure() {
    let tmp = TempDir::new().expect("tempdir");
    let log_path = tmp.path().join("sc-compose.log");

    let include = tmp.path().join("include.md.j2");
    let success_template = tmp.path().join("success.md.j2");
    fs::write(&include, "included text").expect("write include");
    fs::write(&success_template, "@<include.md.j2>\nroot").expect("write template");

    run_sc_compose()
        .env("SC_COMPOSE_LOG_FILE", &log_path)
        .arg("--root")
        .arg(tmp.path())
        .arg("render")
        .arg(&success_template)
        .assert()
        .success();

    let failure_template = tmp.path().join("failure.md.j2");
    fs::write(&failure_template, "@<missing.md.j2>\nroot").expect("write bad template");
    run_sc_compose()
        .env("SC_COMPOSE_LOG_FILE", &log_path)
        .arg("--root")
        .arg(tmp.path())
        .arg("render")
        .arg(&failure_template)
        .assert()
        .code(2);

    let events = read_log_events(&log_path);
    assert!(
        events
            .iter()
            .any(|event| event["action"] == "include_expansion" && event["outcome"] == "success"),
        "include_expansion success event missing: {events:?}"
    );
    assert!(
        events
            .iter()
            .any(|event| event["action"] == "include_expansion" && event["outcome"] == "error"),
        "include_expansion error event missing: {events:?}"
    );
}

#[test]
fn sc_compose_log_level_warn_suppresses_debug_and_info_events() {
    let tmp = TempDir::new().expect("tempdir");
    let log_path = tmp.path().join("sc-compose.log");
    let template = tmp.path().join("template.md.j2");
    fs::write(
        &template,
        "---\nrequired_variables:\n  - name\n---\nhello {{ name }}",
    )
    .expect("write");

    run_sc_compose()
        .env("SC_COMPOSE_LOG_FILE", &log_path)
        .env("SC_COMPOSE_LOG_LEVEL", "warn")
        .arg("--root")
        .arg(tmp.path())
        .arg("validate")
        .arg(&template)
        .assert()
        .code(2);

    let events = read_log_events(&log_path);
    assert!(
        events.iter().all(|event| event["level"] != "debug"),
        "debug events must be suppressed at warn level: {events:?}"
    );
    assert!(
        events
            .iter()
            .any(|event| event["action"] == "command_end" && event["outcome"] == "error"),
        "error events should still be logged at warn level: {events:?}"
    );
}

#[test]
fn sc_compose_log_format_human_writes_human_readable_lines() {
    let tmp = TempDir::new().expect("tempdir");
    let log_path = tmp.path().join("sc-compose-human.log");
    let template = tmp.path().join("template.md.j2");
    fs::write(&template, "hello {{ name }}").expect("write");

    run_sc_compose()
        .env("SC_COMPOSE_LOG_FILE", &log_path)
        .env("SC_COMPOSE_LOG_FORMAT", "human")
        .arg("--root")
        .arg(tmp.path())
        .arg("--var")
        .arg("name=Kai")
        .arg("render")
        .arg(&template)
        .assert()
        .success();

    let lines = read_log_lines(&log_path);
    assert!(
        !lines.is_empty(),
        "human log should contain at least one line"
    );
    assert!(
        lines
            .iter()
            .any(|line| line.contains("action=command_start")),
        "human lines should include action fields: {lines:?}"
    );
    assert!(
        serde_json::from_str::<Value>(&lines[0]).is_err(),
        "human mode should not emit JSONL: {}",
        lines[0]
    );
}

#[test]
fn sc_compose_config_prefers_atm_home_for_default_log_path() {
    let tmp = TempDir::new().expect("tempdir");
    let atm_home = tmp.path().join("atm-home");
    let template = tmp.path().join("template.md.j2");
    fs::write(&template, "hello {{ name }}").expect("write");

    run_sc_compose()
        .env("ATM_HOME", &atm_home)
        .arg("--root")
        .arg(tmp.path())
        .arg("--var")
        .arg("name=Kai")
        .arg("render")
        .arg(&template)
        .assert()
        .success();

    let expected = atm_home.join(".config/sc-compose/logs/sc-compose.log");
    assert!(
        expected.exists(),
        "ATM_HOME-derived log path should exist: {}",
        expected.display()
    );
}

#[test]
fn var_file_dash_reads_json_from_stdin() {
    let tmp = TempDir::new().expect("tempdir");
    let template = tmp.path().join("template.md.j2");
    fs::write(&template, "hello {{ name }}").expect("write");

    // Use assert_cmd::Command (wraps std::process::Command) so that write_stdin
    // is available.  The cargo_bin! macro resolves the binary path at compile
    // time; Command::new avoids the deprecated cargo_bin associated function.
    let mut cmd = assert_cmd::Command::new(cargo::cargo_bin!("sc-compose"));
    cmd.arg("--root")
        .arg(tmp.path())
        .arg("--var-file")
        .arg("-")
        .arg("render")
        .arg(&template)
        .write_stdin(r#"{"name": "Stdin"}"#)
        .assert()
        .success()
        .stdout(predicate::str::contains("hello Stdin"));
}

#[test]
fn var_file_dash_reads_yaml_from_stdin() {
    let tmp = TempDir::new().expect("tempdir");
    let template = tmp.path().join("template.md.j2");
    fs::write(&template, "hello {{ name }}").expect("write");

    let mut cmd = assert_cmd::Command::new(cargo::cargo_bin!("sc-compose"));
    cmd.arg("--root")
        .arg(tmp.path())
        .arg("--var-file")
        .arg("-")
        .arg("render")
        .arg(&template)
        .write_stdin("name: Stdin\n")
        .assert()
        .success()
        .stdout(predicate::str::contains("hello Stdin"));
}
