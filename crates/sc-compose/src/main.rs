use agent_team_mail_core::home::get_home_dir;
use anyhow::{Context, Result};
use clap::error::ErrorKind;
use clap::{ArgAction, Parser, Subcommand, ValueEnum};
use sc_composer::{
    ComposeMode, ComposePolicy, ComposeRequest, ComposerError, ObservabilityEmitter, ProfileKind,
    RuntimeKind, UnknownVariablePolicy,
};
use sc_observability::{LogConfig as SharedLogConfig, LogLevel, Logger as SharedLogger};
use serde::Serialize;
use serde_json::json;
use std::collections::BTreeMap;
use std::io::Read as _;
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::str::FromStr;
use std::sync::Arc;

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

    /// Path to a JSON or YAML file containing variables, or `-` to read from stdin.
    #[arg(long = "var-file", global = true)]
    var_file: Option<String>,

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
        #[arg(long, default_value_t = false)]
        write: bool,
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
    let logger = Arc::new(Logger::new());
    sc_composer::install_observability_emitter(composer_emitter(Arc::clone(&logger)));
    logger.emit(
        "command_start",
        "started",
        json!({"command": format!("{:?}", cli.command)}),
    );

    let json_output = cli.json;
    let result = run(&cli, logger.as_ref());
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

fn run(cli: &Cli, logger: &Logger) -> Result<()> {
    match &cli.command {
        CommandArg::Render {
            template,
            output,
            write,
        } => run_render(cli, template.clone(), output.clone(), *write, logger),
        CommandArg::Resolve { target } => run_resolve(cli, target.clone(), logger),
        CommandArg::Validate { template } => run_validate(cli, template.clone(), logger),
        CommandArg::FrontmatterInit { file, force } => run_frontmatter_init(cli, file, *force),
        CommandArg::Init => run_init(cli),
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum LogFormat {
    Jsonl,
    Human,
}

impl LogFormat {
    fn from_env() -> Self {
        match std::env::var("SC_COMPOSE_LOG_FORMAT")
            .ok()
            .map(|v| v.trim().to_ascii_lowercase())
            .as_deref()
        {
            Some("human") => Self::Human,
            _ => Self::Jsonl,
        }
    }
}

#[derive(Debug, Clone)]
struct Logger {
    inner: SharedLogger,
    threshold: LogLevel,
    format: LogFormat,
}

impl Logger {
    fn new() -> Self {
        let mut cfg = sc_compose_config();
        let threshold = parse_level_env().unwrap_or(cfg.level);
        cfg.level = threshold;
        let format = LogFormat::from_env();
        Self {
            inner: SharedLogger::new(cfg),
            threshold,
            format,
        }
    }

    fn emit(&self, action: &str, result: &str, fields: serde_json::Value) {
        let level = event_level(action, result);
        if !should_emit(level, self.threshold) {
            return;
        }

        match self.format {
            LogFormat::Jsonl => {
                let _ = self.inner.emit_action(
                    "sc-compose",
                    "sc_compose::cli",
                    action,
                    Some(result),
                    fields,
                );
            }
            LogFormat::Human => {
                let _ = self
                    .inner
                    .emit_human(level.as_str(), action, result, &fields);
            }
        }
    }
}

fn composer_emitter(logger: Arc<Logger>) -> ObservabilityEmitter {
    Arc::new(move |action, outcome, fields| logger.emit(action, outcome, fields))
}

fn sc_compose_config() -> SharedLogConfig {
    let home_dir = resolve_home_dir().unwrap_or_else(|| PathBuf::from("."));
    let mut cfg = SharedLogConfig::from_home(&home_dir);
    cfg.log_path = default_log_path().unwrap_or_else(|| {
        home_dir
            .join(".config")
            .join("sc-compose")
            .join("logs")
            .join("sc-compose.log")
    });
    cfg.spool_dir = default_spool_dir(&cfg.log_path);
    cfg
}

fn resolve_home_dir() -> Option<PathBuf> {
    get_home_dir().ok()
}

fn default_log_path() -> Option<PathBuf> {
    if let Ok(explicit) = std::env::var("SC_COMPOSE_LOG_FILE")
        && !explicit.trim().is_empty()
    {
        return Some(PathBuf::from(explicit));
    }
    if let Ok(home) = std::env::var("ATM_HOME")
        && !home.trim().is_empty()
    {
        return Some(
            PathBuf::from(home)
                .join(".config")
                .join("sc-compose")
                .join("logs")
                .join("sc-compose.log"),
        );
    }
    #[cfg(windows)]
    {
        if let Ok(app_data) = std::env::var("APPDATA")
            && !app_data.trim().is_empty()
        {
            return Some(
                PathBuf::from(app_data)
                    .join("sc-compose")
                    .join("logs")
                    .join("sc-compose.log"),
            );
        }
    }
    if let Ok(xdg) = std::env::var("XDG_CONFIG_HOME")
        && !xdg.trim().is_empty()
    {
        return Some(PathBuf::from(xdg).join("sc-compose/logs/sc-compose.log"));
    }
    resolve_home_dir().map(|home| home.join(".config/sc-compose/logs/sc-compose.log"))
}

fn default_spool_dir(log_path: &Path) -> PathBuf {
    let parent = log_path.parent().unwrap_or_else(|| Path::new("."));
    if parent
        .file_name()
        .and_then(|s| s.to_str())
        .map(|s| s.eq_ignore_ascii_case("logs"))
        .unwrap_or(false)
    {
        parent
            .parent()
            .unwrap_or_else(|| Path::new("."))
            .join("log-spool")
    } else {
        parent.join("log-spool")
    }
}

fn parse_level_env() -> Option<LogLevel> {
    std::env::var("SC_COMPOSE_LOG_LEVEL")
        .ok()
        .and_then(|v| LogLevel::from_str(&v).ok())
}

fn event_level(action: &str, result: &str) -> LogLevel {
    if result.eq_ignore_ascii_case("error") {
        return LogLevel::Error;
    }
    if action == "resolver_decision" {
        return LogLevel::Debug;
    }
    LogLevel::Info
}

fn should_emit(level: LogLevel, threshold: LogLevel) -> bool {
    level_rank(level) >= level_rank(threshold)
}

fn level_rank(level: LogLevel) -> u8 {
    match level {
        LogLevel::Trace => 0,
        LogLevel::Debug => 1,
        LogLevel::Info => 2,
        LogLevel::Warn => 3,
        LogLevel::Error => 4,
    }
}

fn run_render(
    cli: &Cli,
    template: Option<PathBuf>,
    output: Option<PathBuf>,
    write: bool,
    logger: &Logger,
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
    let output_path = resolve_render_output_path(cli, &request, &result, output, write);

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

fn resolve_render_output_path(
    cli: &Cli,
    request: &ComposeRequest,
    result: &sc_composer::ComposeResult,
    explicit_output: Option<PathBuf>,
    write: bool,
) -> Option<PathBuf> {
    if let Some(path) = explicit_output {
        return Some(path);
    }
    if !write {
        return None;
    }

    let resolved_template = result
        .resolved_files
        .first()
        .or(request.template_path.as_ref())?;
    Some(match cli.mode {
        ModeArg::File => derive_file_output_path(resolved_template),
        ModeArg::Profile => derive_profile_output_path(cli, request, resolved_template),
    })
}

fn derive_file_output_path(template_path: &Path) -> PathBuf {
    let Some(file_name) = template_path.file_name().and_then(|n| n.to_str()) else {
        return template_path.to_path_buf();
    };
    if let Some(stripped) = file_name.strip_suffix(".j2") {
        return template_path.with_file_name(stripped);
    }
    template_path.to_path_buf()
}

fn derive_profile_output_path(
    cli: &Cli,
    request: &ComposeRequest,
    resolved_template: &Path,
) -> PathBuf {
    let base = request
        .agent
        .as_deref()
        .filter(|s| !s.trim().is_empty())
        .map(sanitize_prompt_name)
        .unwrap_or_else(|| {
            let fallback = resolved_template
                .file_name()
                .and_then(|n| n.to_str())
                .unwrap_or("prompt");
            sanitize_prompt_name(fallback.trim_end_matches(".j2"))
        });
    let ulid = ulid::Ulid::new().to_string().to_ascii_lowercase();
    cli.root.join(".prompts").join(format!("{base}-{ulid}.md"))
}

fn sanitize_prompt_name(raw: &str) -> String {
    let mut out = String::with_capacity(raw.len());
    for ch in raw.chars() {
        if ch.is_ascii_alphanumeric() || ch == '-' || ch == '_' {
            out.push(ch);
        } else if ch == '.' || ch.is_whitespace() {
            out.push('-');
        }
    }
    let normalized = out.trim_matches('-').to_string();
    if normalized.is_empty() {
        "prompt".to_string()
    } else {
        normalized
    }
}

fn emit_include_expansion_success(logger: &Logger, resolved_files: &[PathBuf]) {
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

fn emit_include_expansion_failure(logger: &Logger, err: &anyhow::Error) {
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

fn run_resolve(cli: &Cli, target: Option<String>, logger: &Logger) -> Result<()> {
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

fn run_validate(cli: &Cli, template: Option<PathBuf>, logger: &Logger) -> Result<()> {
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

/// Load variables from a file path or from stdin when `spec` is `"-"`.
///
/// When `spec` is `"-"`, the entire contents of stdin are read and parsed as
/// JSON (tried first) or YAML. When `spec` is a file path, the format is
/// inferred from the `.json` extension; all other extensions are parsed as YAML.
fn load_var_file(spec: Option<&str>) -> Result<BTreeMap<String, String>> {
    let Some(spec) = spec else {
        return Ok(BTreeMap::new());
    };

    if spec == "-" {
        let mut raw = String::new();
        std::io::stdin()
            .read_to_string(&mut raw)
            .context("failed to read var-file from stdin")?;
        return parse_var_file_content(&raw, "<stdin>");
    }

    let path = Path::new(spec);
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

/// Parse a JSON-or-YAML string into a variable map.
///
/// JSON is tried first (the JSON spec is a strict subset of YAML, but the
/// `serde_json` error messages are clearer for JSON input). Falls back to YAML.
fn parse_var_file_content(raw: &str, source_label: &str) -> Result<BTreeMap<String, String>> {
    if let Ok(map) = serde_json::from_str::<BTreeMap<String, String>>(raw) {
        return Ok(map);
    }
    serde_yaml::from_str::<BTreeMap<String, String>>(raw)
        .with_context(|| format!("invalid JSON/YAML var-file from {source_label}"))
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
