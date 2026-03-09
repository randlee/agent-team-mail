mod observability;

use anyhow::{Context, Result};
use clap::error::ErrorKind;
use clap::{ArgAction, Parser, Subcommand, ValueEnum};
use sc_composer::{
    ComposeMode, ComposePolicy, ComposeRequest, ComposerError, ProfileKind, RuntimeKind,
    UnknownVariablePolicy,
};
use serde::Serialize;
use serde_json::json;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

#[derive(Debug, Parser)]
#[command(
    name = "sc-compose",
    version,
    about = "Compose and validate AI prompt templates"
)]
struct Cli {
    #[arg(long, global = true, value_enum, default_value_t = ModeArg::File)]
    mode: ModeArg,

    #[arg(long, global = true, value_enum)]
    kind: Option<KindArg>,

    #[arg(long = "agent-type", visible_alias = "agent", global = true)]
    agent_type: Option<String>,

    #[arg(long = "runtime", visible_alias = "ai", global = true, value_enum, default_value_t = RuntimeArg::Claude)]
    runtime: RuntimeArg,

    #[arg(long, global = true, default_value = ".")]
    root: PathBuf,

    #[arg(long = "var", global = true, value_parser = parse_key_val, action = ArgAction::Append)]
    vars: Vec<(String, String)>,

    #[arg(long = "var-file", global = true)]
    var_file: Option<PathBuf>,

    #[arg(long = "env-prefix", global = true)]
    env_prefix: Option<String>,

    #[arg(long, global = true)]
    json: bool,

    #[arg(long, global = true)]
    dry_run: bool,

    #[command(subcommand)]
    command: CommandArg,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
enum ModeArg {
    File,
    Profile,
}

#[derive(Debug, Clone, Copy, ValueEnum)]
enum KindArg {
    Agent,
    Command,
    Skill,
}

#[derive(Debug, Clone, Copy, ValueEnum)]
enum RuntimeArg {
    Claude,
    Codex,
    Gemini,
    Opencode,
}

#[derive(Debug, Subcommand)]
enum CommandArg {
    Render {
        template: Option<PathBuf>,
        #[arg(long)]
        output: Option<PathBuf>,
    },
    Resolve {
        target: Option<String>,
    },
    Validate {
        template: Option<PathBuf>,
    },
    FrontmatterInit {
        file: PathBuf,
        #[arg(long)]
        force: bool,
    },
    Init,
}

fn main() -> ExitCode {
    let cli = match Cli::try_parse() {
        Ok(cli) => cli,
        Err(err) => {
            let code = match err.kind() {
                ErrorKind::DisplayHelp | ErrorKind::DisplayVersion => 0,
                _ => 3,
            };
            let _ = err.print();
            return ExitCode::from(code);
        }
    };
    let logger = observability::Logger::new();
    logger.emit(
        "command_start",
        "started",
        json!({"command": format!("{:?}", cli.command)}),
    );

    let json_output = cli.json;
    let result = run(&cli, &logger);
    match result {
        Ok(()) => {
            logger.emit("command_end", "success", json!({"code": 0}));
            ExitCode::from(0)
        }
        Err(err) => {
            let code = classify_error_code(&err);
            emit_error(&err, json_output);
            emit_include_expansion_failure(&logger, &err);
            logger.emit(
                "command_end",
                "error",
                json!({"code": code, "error": err.to_string()}),
            );
            ExitCode::from(code)
        }
    }
}

fn run(cli: &Cli, logger: &observability::Logger) -> Result<()> {
    match &cli.command {
        CommandArg::Render { template, output } => {
            run_render(cli, template.clone(), output.clone(), logger)
        }
        CommandArg::Resolve { target } => run_resolve(cli, target.clone(), logger),
        CommandArg::Validate { template } => run_validate(cli, template.clone(), logger),
        CommandArg::FrontmatterInit { file, force } => run_frontmatter_init(cli, file, *force),
        CommandArg::Init => run_init(cli),
    }
}

fn run_render(
    cli: &Cli,
    template: Option<PathBuf>,
    output: Option<PathBuf>,
    logger: &observability::Logger,
) -> Result<()> {
    let request = build_request(cli, template, None)?;
    if cli.mode == ModeArg::Profile && request.agent.is_none() {
        anyhow::bail!("--agent-type/--agent is required in --mode profile");
    }

    logger.emit(
        "resolver_decision",
        "attempt",
        json!({"mode": format!("{:?}", cli.mode), "kind": format!("{:?}", cli.kind)}),
    );
    let result = sc_composer::compose(&request)?;
    emit_include_expansion_success(logger, &result.resolved_files);
    let output_path = output;

    if cli.json {
        eprintln!(
            "{}",
            serde_json::to_string(&json!({
                "warnings": result.warnings,
                "searchTrace": to_string_paths(&result.search_trace),
                "resolvedFiles": to_string_paths(&result.resolved_files),
            }))?
        );
    }

    if let Some(path) = output_path {
        if cli.dry_run {
            let would_change = std::fs::read_to_string(&path)
                .map(|existing| existing != result.rendered_text)
                .unwrap_or(true);
            if cli.json {
                eprintln!(
                    "{}",
                    serde_json::to_string(&json!({
                        "resolvedTemplate": request.template_path.map(|p| p.display().to_string()),
                        "resolvedOutput": path.display().to_string(),
                        "wouldChange": would_change
                    }))?
                );
            } else {
                eprintln!("dry-run: output path {}", path.display());
                eprintln!("dry-run: would change = {would_change}");
            }
            return Ok(());
        }
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).with_context(|| {
                format!("failed to create output directory {}", parent.display())
            })?;
        }
        std::fs::write(&path, result.rendered_text)
            .with_context(|| format!("failed to write output {}", path.display()))?;
        logger.emit(
            "render_outcome",
            "success",
            json!({"output": path.display().to_string()}),
        );
        return Ok(());
    }

    println!("{}", result.rendered_text);
    logger.emit("render_outcome", "success", json!({"output": "stdout"}));
    Ok(())
}

fn emit_include_expansion_success(logger: &observability::Logger, resolved_files: &[PathBuf]) {
    let included_files: Vec<String> = resolved_files
        .iter()
        .skip(1)
        .map(|path| path.display().to_string())
        .collect();
    logger.emit(
        "include_expansion",
        "success",
        json!({
            "includedCount": included_files.len(),
            "includedFiles": included_files
        }),
    );
}

fn emit_include_expansion_failure(logger: &observability::Logger, err: &anyhow::Error) {
    let Some(compose_err) = err.downcast_ref::<ComposerError>() else {
        return;
    };
    match compose_err {
        ComposerError::IncludeError { diagnostic } => {
            logger.emit(
                "include_expansion",
                "error",
                json!({
                    "diagnostic": diagnostic,
                }),
            );
        }
        ComposerError::ValidationFailed { errors, .. } => {
            let include_related: Vec<&sc_composer::Diagnostic> = errors
                .iter()
                .filter(|diagnostic| {
                    diagnostic.code.starts_with("INCLUDE_") || diagnostic.code == "ROOT_ESCAPE"
                })
                .collect();
            if !include_related.is_empty() {
                logger.emit(
                    "include_expansion",
                    "error",
                    json!({
                        "diagnostics": include_related,
                    }),
                );
            }
        }
        _ => {}
    }
}

fn run_resolve(cli: &Cli, target: Option<String>, logger: &observability::Logger) -> Result<()> {
    let request = build_request(cli, None, target)?;
    let resolved = sc_composer::resolve(&request)?;

    if cli.json {
        eprintln!(
            "{}",
            serde_json::to_string(&json!({
                "resolvedPath": resolved.resolved_path.display().to_string(),
                "attemptedPaths": to_string_paths(&resolved.attempted_paths),
            }))?
        );
    } else {
        println!("{}", resolved.resolved_path.display());
        println!("attempted:");
        for candidate in resolved.attempted_paths {
            println!("  - {}", candidate.display());
        }
    }
    logger.emit("resolver_decision", "success", json!({}));
    Ok(())
}

fn run_validate(
    cli: &Cli,
    template: Option<PathBuf>,
    logger: &observability::Logger,
) -> Result<()> {
    let request = build_request(cli, template, None)?;
    let report = sc_composer::validate(&request)?;

    if report.ok && cli.json {
        eprintln!(
            "{}",
            serde_json::to_string(&SerializableReport::from(&report))?
        );
    } else if !cli.json {
        for warning in &report.warnings {
            eprintln!("warning [{}]: {}", warning.code, warning.message);
        }
        for error in &report.errors {
            eprintln!("error [{}]: {}", error.code, error.message);
        }
        if cli.mode == ModeArg::Profile && !report.search_trace.is_empty() {
            eprintln!("search trace:");
            for candidate in &report.search_trace {
                eprintln!("  - {}", candidate.display());
            }
        }
    }

    logger.emit(
        "validate_outcome",
        if report.ok { "ok" } else { "error" },
        json!({"ok": report.ok}),
    );

    if report.ok {
        Ok(())
    } else {
        Err(ComposerError::ValidationFailed {
            error_count: report.errors.len(),
            errors: Box::new(report.errors),
            warnings: Box::new(report.warnings),
        }
        .into())
    }
}

fn run_frontmatter_init(cli: &Cli, file: &Path, force: bool) -> Result<()> {
    let original = std::fs::read_to_string(file)
        .with_context(|| format!("failed to read {}", file.display()))?;
    let (had_frontmatter, body_without_frontmatter) = strip_frontmatter_block(&original);

    if had_frontmatter && !force {
        anyhow::bail!("frontmatter already exists; re-run with --force to replace");
    }

    let mut vars = sc_composer::discover_template_variables(&body_without_frontmatter);
    vars.sort();
    vars.dedup();
    let required_lines = if vars.is_empty() {
        "required_variables: []".to_string()
    } else {
        format!(
            "required_variables:\n{}",
            vars.iter()
                .map(|v| format!("  - {v}"))
                .collect::<Vec<_>>()
                .join("\n")
        )
    };
    let frontmatter = format!("---\n{required_lines}\ndefaults: {{}}\nmetadata: {{}}\n---\n");
    let updated = format!("{frontmatter}{body_without_frontmatter}");

    if cli.dry_run {
        println!("{}", frontmatter.trim_end());
        eprintln!("dry-run: target {}", file.display());
        return Ok(());
    }

    std::fs::write(file, updated).with_context(|| format!("failed to write {}", file.display()))?;
    println!("frontmatter initialized: {}", file.display());
    Ok(())
}

fn run_init(cli: &Cli) -> Result<()> {
    let prompts_dir = cli.root.join(".prompts");
    let gitignore_path = cli.root.join(".gitignore");
    let gitignore_entry = ".prompts/";

    if cli.dry_run {
        println!("dry-run: would create {}", prompts_dir.display());
        println!(
            "dry-run: would ensure '{}' exists in {}",
            gitignore_entry,
            gitignore_path.display()
        );
        return Ok(());
    }

    std::fs::create_dir_all(&prompts_dir)
        .with_context(|| format!("failed to create {}", prompts_dir.display()))?;
    let mut gitignore = std::fs::read_to_string(&gitignore_path).unwrap_or_default();
    if !gitignore.lines().any(|line| line.trim() == gitignore_entry) {
        if !gitignore.ends_with('\n') && !gitignore.is_empty() {
            gitignore.push('\n');
        }
        gitignore.push_str(gitignore_entry);
        gitignore.push('\n');
        std::fs::write(&gitignore_path, gitignore)
            .with_context(|| format!("failed to write {}", gitignore_path.display()))?;
    }
    println!("initialized {}", prompts_dir.display());
    Ok(())
}

fn build_request(
    cli: &Cli,
    template: Option<PathBuf>,
    override_agent: Option<String>,
) -> Result<ComposeRequest> {
    let mut vars_input = load_var_file(cli.var_file.as_deref())?;
    for (k, v) in &cli.vars {
        vars_input.insert(k.clone(), v.clone());
    }

    let mut vars_env = BTreeMap::new();
    if let Some(prefix) = &cli.env_prefix {
        for (key, value) in std::env::vars() {
            if let Some(stripped) = key.strip_prefix(prefix) {
                vars_env.insert(stripped.to_string(), value);
            }
        }
    }

    let request = ComposeRequest {
        runtime: map_runtime(cli.runtime),
        mode: map_mode(cli.mode),
        kind: cli.kind.map(map_kind),
        root: cli.root.clone(),
        agent: override_agent.or_else(|| cli.agent_type.clone()),
        template_path: template,
        vars_input,
        vars_env,
        guidance_block: None,
        user_prompt: None,
        policy: ComposePolicy {
            unknown_variable_policy: UnknownVariablePolicy::Error,
            ..ComposePolicy::default()
        },
    };
    Ok(request)
}

fn load_var_file(path: Option<&Path>) -> Result<BTreeMap<String, String>> {
    let Some(path) = path else {
        return Ok(BTreeMap::new());
    };
    let raw = std::fs::read_to_string(path)
        .with_context(|| format!("failed to read {}", path.display()))?;
    if path.extension().and_then(|ext| ext.to_str()) == Some("json") {
        let map: BTreeMap<String, String> = serde_json::from_str(&raw)
            .with_context(|| format!("invalid JSON var-file {}", path.display()))?;
        return Ok(map);
    }
    let map: BTreeMap<String, String> = serde_yaml::from_str(&raw)
        .with_context(|| format!("invalid YAML var-file {}", path.display()))?;
    Ok(map)
}

fn parse_key_val(s: &str) -> Result<(String, String), String> {
    let Some((k, v)) = s.split_once('=') else {
        return Err("expected KEY=VALUE".to_string());
    };
    if k.trim().is_empty() {
        return Err("key cannot be empty".to_string());
    }
    Ok((k.to_string(), v.to_string()))
}

fn map_mode(mode: ModeArg) -> ComposeMode {
    match mode {
        ModeArg::File => ComposeMode::File,
        ModeArg::Profile => ComposeMode::Profile,
    }
}

fn map_kind(kind: KindArg) -> ProfileKind {
    match kind {
        KindArg::Agent => ProfileKind::Agent,
        KindArg::Command => ProfileKind::Command,
        KindArg::Skill => ProfileKind::Skill,
    }
}

fn map_runtime(runtime: RuntimeArg) -> RuntimeKind {
    match runtime {
        RuntimeArg::Claude => RuntimeKind::Claude,
        RuntimeArg::Codex => RuntimeKind::Codex,
        RuntimeArg::Gemini => RuntimeKind::Gemini,
        RuntimeArg::Opencode => RuntimeKind::Opencode,
    }
}

fn classify_error_code(err: &anyhow::Error) -> u8 {
    if let Some(composer) = err.downcast_ref::<ComposerError>() {
        return match composer {
            ComposerError::MissingTemplatePath
            | ComposerError::MissingProfileKind
            | ComposerError::MissingProfileName => 3,
            _ => 2,
        };
    }
    if err.to_string().contains("frontmatter already exists") {
        return 3;
    }
    2
}

fn emit_error(err: &anyhow::Error, json_output: bool) {
    if let Some(composer) = err.downcast_ref::<ComposerError>() {
        if json_output {
            let payload = match composer {
                ComposerError::ValidationFailed {
                    errors,
                    warnings,
                    error_count,
                } => json!({
                    "errorCode": "VALIDATION_FAILED",
                    "message": composer.to_string(),
                    "errorCount": error_count,
                    "errors": errors,
                    "warnings": warnings,
                }),
                ComposerError::ProfileResolutionFailed {
                    attempted_paths, ..
                } => json!({
                    "errorCode": "PROFILE_RESOLUTION_FAILED",
                    "message": composer.to_string(),
                    "attemptedPaths": to_string_paths(attempted_paths.as_ref()),
                }),
                ComposerError::IncludeError { diagnostic } => json!({
                    "errorCode": "INCLUDE_ERROR",
                    "message": composer.to_string(),
                    "errors": [diagnostic],
                }),
                _ => json!({
                    "errorCode": "COMPOSER_ERROR",
                    "message": composer.to_string(),
                }),
            };
            eprintln!(
                "{}",
                serde_json::to_string(&payload).unwrap_or_else(|_| {
                    "{\"errorCode\":\"SERIALIZE_ERROR\",\"message\":\"failed to serialize error\"}"
                        .to_string()
                })
            );
            return;
        }
        eprintln!("{composer}");
        return;
    }
    if json_output {
        eprintln!(
            "{}",
            serde_json::to_string(&json!({
                "errorCode": "UNEXPECTED_ERROR",
                "message": err.to_string(),
            }))
            .unwrap_or_else(|_| {
                "{\"errorCode\":\"SERIALIZE_ERROR\",\"message\":\"failed to serialize error\"}"
                    .to_string()
            })
        );
    } else {
        eprintln!("{err}");
    }
}

fn strip_frontmatter_block(content: &str) -> (bool, String) {
    let mut segments = content.split_inclusive('\n');
    let Some(first) = segments.next() else {
        return (false, String::new());
    };
    if first.trim_end_matches('\n').trim_end_matches('\r') != "---" {
        return (false, content.to_string());
    }

    let mut offset = first.len();
    let mut found = false;
    for segment in segments {
        offset += segment.len();
        if segment.trim_end_matches('\n').trim_end_matches('\r') == "---" {
            found = true;
            break;
        }
    }
    if found {
        (true, content[offset..].to_string())
    } else {
        (false, content.to_string())
    }
}

fn to_string_paths(paths: &[PathBuf]) -> Vec<String> {
    paths.iter().map(|p| p.display().to_string()).collect()
}

#[derive(Debug, Serialize)]
struct SerializableReport {
    ok: bool,
    warnings: Vec<sc_composer::Diagnostic>,
    errors: Vec<sc_composer::Diagnostic>,
    search_trace: Vec<String>,
}

impl From<&sc_composer::ValidationReport> for SerializableReport {
    fn from(value: &sc_composer::ValidationReport) -> Self {
        Self {
            ok: value.ok,
            warnings: value.warnings.clone(),
            errors: value.errors.clone(),
            search_trace: to_string_paths(&value.search_trace),
        }
    }
}
