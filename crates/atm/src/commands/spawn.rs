use agent_team_mail_core::config::{ConfigOverrides, resolve_config};
use agent_team_mail_core::daemon_client::query_team_member_states;
use agent_team_mail_core::spawn::{
    PaneMode, SpawnDraft, apply_edits, parse_pane_mode, read_agent_frontmatter,
};
use anyhow::Result;
use clap::Args;
use crossterm::tty::IsTty;
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::process::Command;

use crate::commands::runtime_adapter::RuntimeKind;
use crate::util::settings::get_home_dir;

#[derive(Args, Debug, Clone)]
pub struct SpawnArgs {
    /// Runtime to launch
    #[arg(value_enum, default_value_t = RuntimeKind::Codex)]
    runtime: RuntimeKind,

    /// Team name (defaults to configured default team)
    #[arg(long)]
    team: Option<String>,

    /// Member name (defaults to runtime name)
    #[arg(long)]
    member: Option<String>,

    /// Model identifier
    #[arg(long)]
    model: Option<String>,

    /// Agent type label shown in review panel
    #[arg(long, default_value = "general-purpose")]
    agent_type: String,

    /// Pane mode: new-pane, existing-pane, current-pane
    #[arg(long, default_value = "new-pane")]
    pane_mode: String,

    /// Working directory for launch
    #[arg(long)]
    worktree: Option<PathBuf>,

    /// Show generated command without executing
    #[arg(long)]
    dry_run: bool,

    /// Skip interactive panel and execute immediately
    #[arg(long)]
    yes: bool,

    /// Print tmux targeting help
    #[arg(long)]
    tmux_help: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct TmuxContext {
    session: String,
    window_index: String,
    window_name: String,
    pane_id: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct TmuxPane {
    index: String,
    title: String,
}

#[derive(Debug, Clone, Default)]
struct PanelState {
    tmux_context: Option<TmuxContext>,
    available_panes: Vec<TmuxPane>,
    selected_existing_pane: Option<String>,
    member_running: bool,
}

pub fn execute(args: SpawnArgs) -> Result<()> {
    if args.tmux_help {
        print_tmux_help();
        return Ok(());
    }

    let home = get_home_dir()?;
    let current_dir = std::env::current_dir()?;
    let config = resolve_config(
        &ConfigOverrides {
            team: args.team.clone(),
            ..Default::default()
        },
        &current_dir,
        &home,
    )?;

    let member_name = args
        .member
        .clone()
        .unwrap_or_else(|| runtime_name(&args.runtime).to_string());
    let frontmatter = read_agent_frontmatter(&home, &member_name);
    let mut draft = SpawnDraft {
        team: args
            .team
            .clone()
            .unwrap_or_else(|| config.core.default_team.clone()),
        member: member_name,
        model: args
            .model
            .clone()
            .or(frontmatter.model)
            .unwrap_or_else(|| default_model_for_runtime(&args.runtime).to_string()),
        agent_type: args.agent_type.clone(),
        pane_mode: parse_pane_mode(&args.pane_mode)?,
        worktree: args
            .worktree
            .as_ref()
            .map(|p| p.to_string_lossy().to_string()),
        color: frontmatter.color,
    };
    let mut panel_state = PanelState {
        tmux_context: detect_tmux_context(),
        ..Default::default()
    };

    let stdin_is_tty = io::stdin().is_tty();
    enforce_tty_guard(stdin_is_tty, args.yes)?;
    if should_use_interactive(stdin_is_tty, args.yes) {
        run_interactive_panel(&mut draft, &args.runtime, &mut panel_state)?;
    }
    refresh_panel_state_for_draft(&mut panel_state, &draft);
    validate_spawn_request(&draft, &panel_state, false)?;

    if args.dry_run {
        println!(
            "{}",
            build_dry_run_output(&draft, &args.runtime, &panel_state)
        );
        return Ok(());
    }

    execute_spawn(&draft, &args.runtime, &panel_state)
}

fn execute_spawn(
    draft: &SpawnDraft,
    runtime: &RuntimeKind,
    panel_state: &PanelState,
) -> Result<()> {
    if panel_state.tmux_context.is_none() {
        return execute_via_teams_spawn(draft, runtime);
    }
    let exe = std::env::current_exe()?;
    let args = build_teams_spawn_args(draft, runtime);
    let command_str = build_shell_command(exe.to_string_lossy().as_ref(), &args);
    match draft.pane_mode {
        PaneMode::NewPane => {
            let status = Command::new("tmux")
                .arg("split-window")
                .arg("-h")
                .arg(command_str)
                .status()?;
            if !status.success() {
                anyhow::bail!("tmux split-window failed with status {status}");
            }
            Ok(())
        }
        PaneMode::ExistingPane => {
            let tmux_ctx = panel_state.tmux_context.as_ref().expect("checked above");
            let target_pane = panel_state
                .selected_existing_pane
                .as_deref()
                .ok_or_else(|| anyhow::anyhow!("existing-pane mode requires pane selection"))?;
            let target = format!(
                "{}:{}.{}",
                tmux_ctx.session, tmux_ctx.window_index, target_pane
            );
            let status = Command::new("tmux")
                .arg("send-keys")
                .arg("-t")
                .arg(target)
                .arg(command_str)
                .arg("Enter")
                .status()?;
            if !status.success() {
                anyhow::bail!("tmux send-keys failed with status {status}");
            }
            Ok(())
        }
        PaneMode::CurrentPane => {
            let tmux_ctx = panel_state.tmux_context.as_ref().expect("checked above");
            let status = Command::new("tmux")
                .arg("send-keys")
                .arg("-t")
                .arg(&tmux_ctx.pane_id)
                .arg(command_str)
                .arg("Enter")
                .status()?;
            if !status.success() {
                anyhow::bail!("tmux send-keys failed with status {status}");
            }
            Ok(())
        }
    }
}

fn execute_via_teams_spawn(draft: &SpawnDraft, runtime: &RuntimeKind) -> Result<()> {
    let exe = std::env::current_exe()?;
    let args = build_teams_spawn_args(draft, runtime);
    let status = Command::new(exe).args(args).status()?;
    if !status.success() {
        anyhow::bail!("spawn failed (teams spawn exited with status {status})");
    }
    Ok(())
}

fn build_teams_spawn_args(draft: &SpawnDraft, runtime: &RuntimeKind) -> Vec<String> {
    let mut args = vec![
        "teams".to_string(),
        "spawn".to_string(),
        draft.member.clone(),
        "--runtime".to_string(),
        runtime_name(runtime).to_string(),
        "--team".to_string(),
        draft.team.clone(),
        "--model".to_string(),
        draft.model.clone(),
    ];
    if let Some(color) = &draft.color {
        args.push("--color".to_string());
        args.push(color.clone());
    }
    if let Some(worktree) = &draft.worktree {
        args.push("--folder".to_string());
        args.push(worktree.clone());
    }
    args
}

fn build_shell_command(binary: &str, args: &[String]) -> String {
    let mut parts = Vec::with_capacity(args.len() + 1);
    parts.push(shell_quote(binary));
    parts.extend(args.iter().map(|arg| shell_quote(arg)));
    parts.join(" ")
}

fn print_tmux_help() {
    println!("Pane modes:");
    println!("  new-pane      create a new tmux pane");
    println!("  existing-pane target an existing tmux pane");
    println!("  current-pane  run in current pane");
}

fn runtime_name(runtime: &RuntimeKind) -> &'static str {
    match runtime {
        RuntimeKind::Claude => "claude",
        RuntimeKind::Codex => "codex",
        RuntimeKind::Gemini => "gemini",
        RuntimeKind::Opencode => "opencode",
    }
}

fn default_model_for_runtime(runtime: &RuntimeKind) -> &'static str {
    match runtime {
        // Codex spawns should default to the roster baseline when neither the
        // CLI nor the agent frontmatter pins a model explicitly.
        RuntimeKind::Codex => "claude-sonnet-4-5",
        _ => "unknown",
    }
}

fn should_use_interactive(stdin_is_tty: bool, yes: bool) -> bool {
    stdin_is_tty && !yes
}

fn enforce_tty_guard(stdin_is_tty: bool, yes: bool) -> Result<()> {
    if !stdin_is_tty && !yes {
        anyhow::bail!(
            "interactive mode requires a terminal (stdin is not a tty). hint: use --yes for non-interactive execution"
        );
    }
    Ok(())
}

fn validate_draft_fields(draft: &SpawnDraft) -> Result<()> {
    if draft.team.trim().is_empty() {
        anyhow::bail!("team cannot be empty");
    }
    if draft.member.trim().is_empty() {
        anyhow::bail!("member cannot be empty");
    }
    if draft.model.trim().is_empty() {
        anyhow::bail!("model cannot be empty");
    }
    if let Some(worktree) = &draft.worktree {
        let p = Path::new(worktree);
        if !p.exists() {
            anyhow::bail!("worktree '{}' does not exist", p.display());
        }
        if !p.is_dir() {
            anyhow::bail!("worktree '{}' is not a directory", p.display());
        }
    }
    Ok(())
}

fn validate_spawn_request(
    draft: &SpawnDraft,
    panel_state: &PanelState,
    interactive_mode: bool,
) -> Result<()> {
    validate_draft_fields(draft)?;
    if panel_state.tmux_context.is_none()
        && matches!(draft.pane_mode, PaneMode::NewPane | PaneMode::ExistingPane)
    {
        anyhow::bail!("pane-mode new-pane/existing-pane requires an active tmux session.");
    }
    if draft.pane_mode == PaneMode::ExistingPane {
        if panel_state.available_panes.is_empty() {
            anyhow::bail!(
                "existing-pane mode requires at least one pane in the current tmux window."
            );
        }
        let Some(selected) = panel_state.selected_existing_pane.as_deref() else {
            if interactive_mode {
                anyhow::bail!("existing-pane mode requires pane selection. Set 7=<pane-index>.");
            }
            anyhow::bail!(
                "existing-pane mode requires pane selection. Re-run without --yes and set 7=<pane-index>."
            );
        };
        if !panel_state
            .available_panes
            .iter()
            .any(|pane| pane.index == selected)
        {
            anyhow::bail!("selected pane index '{selected}' is not available in current window.");
        }
    }
    Ok(())
}

fn refresh_panel_state_for_draft(panel_state: &mut PanelState, draft: &SpawnDraft) {
    panel_state.member_running = query_member_running(&draft.team, &draft.member);
    if draft.pane_mode == PaneMode::ExistingPane {
        if panel_state.tmux_context.is_some() {
            panel_state.available_panes = list_tmux_panes().unwrap_or_default();
            if let Some(selected) = panel_state.selected_existing_pane.as_deref()
                && !panel_state
                    .available_panes
                    .iter()
                    .any(|pane| pane.index == selected)
            {
                panel_state.selected_existing_pane = None;
            }
        }
    } else {
        panel_state.selected_existing_pane = None;
        panel_state.available_panes.clear();
    }
}

fn run_interactive_panel(
    draft: &mut SpawnDraft,
    runtime: &RuntimeKind,
    panel_state: &mut PanelState,
) -> Result<()> {
    let mut inline_error: Option<String> = None;
    loop {
        refresh_panel_state_for_draft(panel_state, draft);
        render_panel(draft, runtime, panel_state, inline_error.as_deref());
        print!("> ");
        io::stdout().flush()?;
        let mut line = String::new();
        io::stdin().read_line(&mut line)?;
        let input = line.trim();
        if input.is_empty() {
            match validate_spawn_request(draft, panel_state, true) {
                Ok(()) => return Ok(()),
                Err(err) => {
                    inline_error = Some(err.to_string());
                    continue;
                }
            }
        }
        if input.eq_ignore_ascii_case("q") || input == "\u{1b}" {
            std::process::exit(0);
        }
        if let Err(err) = apply_panel_edits(draft, panel_state, input) {
            inline_error = Some(err.to_string());
            continue;
        }
        refresh_panel_state_for_draft(panel_state, draft);
        inline_error = validate_spawn_request(draft, panel_state, true)
            .err()
            .map(|err| err.to_string());
    }
}

fn render_panel(
    draft: &SpawnDraft,
    runtime: &RuntimeKind,
    panel_state: &PanelState,
    inline_error: Option<&str>,
) {
    println!();
    println!("atm spawn — interactive mode");
    println!("Spawning runtime: {}", runtime_name(runtime));
    println!();
    if let Some(tmux) = &panel_state.tmux_context {
        println!(
            "tmux context: session={} window={} ({})",
            tmux.session, tmux.window_index, tmux.window_name
        );
    }
    if panel_state.member_running {
        println!("⚠ member appears to be running already");
    }
    println!();
    println!("  1. team:       {}", draft.team);
    println!("  2. member:     {}", draft.member);
    println!("  3. model:      {}", draft.model);
    println!("  4. agent-type: {}", draft.agent_type);
    println!("  5. pane-mode:  {}", draft.pane_mode.as_str());
    println!(
        "  6. worktree:   {}",
        draft.worktree.as_deref().unwrap_or("(none)")
    );
    if draft.pane_mode == PaneMode::ExistingPane {
        println!(
            "  7. pane-index: {}",
            panel_state
                .selected_existing_pane
                .as_deref()
                .unwrap_or("(required)")
        );
        if panel_state.available_panes.is_empty() {
            println!("  Available panes in current window: (none)");
        } else {
            println!("  Available panes in current window:");
            for pane in &panel_state.available_panes {
                println!("    pane {}: '{}'", pane.index, pane.title);
            }
        }
    }
    println!();
    if let Some(err) = inline_error {
        println!("  [error] {err}");
        println!("  valid: 5=new-pane|existing-pane|current-pane");
        if draft.pane_mode == PaneMode::ExistingPane {
            println!("  valid: 7=<pane-index>");
        }
        println!();
    }
    println!("Enter to confirm · q to cancel");
    println!("Change items with n=value or n=value,m=value2");
}

fn build_dry_run_output(
    draft: &SpawnDraft,
    runtime: &RuntimeKind,
    panel_state: &PanelState,
) -> String {
    let launch_command = build_shell_command("atm", &build_teams_spawn_args(draft, runtime));
    let pane_line = match draft.pane_mode {
        PaneMode::NewPane => "tmux action: tmux split-window -h '<spawn-command>'".to_string(),
        PaneMode::ExistingPane => format!(
            "tmux action: tmux send-keys -t <session>:<window>.{} '<spawn-command>' Enter",
            panel_state
                .selected_existing_pane
                .as_deref()
                .unwrap_or("<pane-index>")
        ),
        PaneMode::CurrentPane => {
            if panel_state.tmux_context.is_some() {
                "tmux action: tmux send-keys -t <current-pane> '<spawn-command>' Enter".to_string()
            } else {
                "tmux action: execute in current terminal (non-tmux fallback)".to_string()
            }
        }
    };
    format!(
        "[dry-run] What would happen:\n1. {pane_line}\n2. Run command:\n   {launch_command}\n3. Register/update member metadata via teams spawn\n\nNo changes made (dry-run)."
    )
}

fn shell_quote(input: &str) -> String {
    if input.is_empty() {
        return "''".to_string();
    }
    format!("'{}'", input.replace('\'', "'\"'\"'"))
}

fn parse_edit_pair(pair: &str) -> Result<(u8, String)> {
    let Some((idx_raw, value_raw)) = pair.split_once('=') else {
        anyhow::bail!("invalid edit '{pair}'. expected n=value");
    };
    let idx = idx_raw
        .trim()
        .parse::<u8>()
        .map_err(|_| anyhow::anyhow!("invalid field index '{idx_raw}'"))?;
    Ok((idx, value_raw.trim().to_string()))
}

fn apply_panel_edits(
    draft: &mut SpawnDraft,
    panel_state: &mut PanelState,
    edits: &str,
) -> Result<()> {
    for pair in edits.split(',') {
        let pair = pair.trim();
        if pair.is_empty() {
            continue;
        }
        let (idx, value) = parse_edit_pair(pair)?;
        if idx == 7 {
            if value.is_empty() {
                anyhow::bail!("pane index cannot be empty");
            }
            panel_state.selected_existing_pane = Some(value);
            continue;
        }
        apply_edits(draft, &format!("{idx}={value}"))?;
    }
    if draft.pane_mode != PaneMode::ExistingPane {
        panel_state.selected_existing_pane = None;
    }
    Ok(())
}

fn detect_tmux_context() -> Option<TmuxContext> {
    std::env::var("TMUX")
        .ok()
        .filter(|v| !v.trim().is_empty())?;
    let output = Command::new("tmux")
        .arg("display-message")
        .arg("-p")
        .arg("#S\t#I\t#W\t#{pane_id}")
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let text = String::from_utf8(output.stdout).ok()?;
    let mut parts = text.trim().split('\t');
    let session = parts.next()?.to_string();
    let window_index = parts.next()?.to_string();
    let window_name = parts.next()?.to_string();
    let pane_id = parts.next()?.to_string();
    Some(TmuxContext {
        session,
        window_index,
        window_name,
        pane_id,
    })
}

fn list_tmux_panes() -> Result<Vec<TmuxPane>> {
    let output = Command::new("tmux")
        .arg("list-panes")
        .arg("-F")
        .arg("#{pane_index}\t#{pane_title}")
        .output()?;
    if !output.status.success() {
        anyhow::bail!("failed to list tmux panes");
    }
    let text = String::from_utf8_lossy(&output.stdout);
    let panes = text
        .lines()
        .filter(|line| !line.trim().is_empty())
        .map(|line| {
            let mut parts = line.splitn(2, '\t');
            let index = parts.next().unwrap_or("").trim().to_string();
            let title = parts.next().unwrap_or("(untitled)").trim().to_string();
            TmuxPane { index, title }
        })
        .collect::<Vec<_>>();
    Ok(panes)
}

fn query_member_running(team: &str, member: &str) -> bool {
    let Ok(Some(states)) = query_team_member_states(team) else {
        return false;
    };
    states
        .iter()
        .find(|state| state.agent == member)
        .map(|state| state.state == "active")
        .unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn draft() -> SpawnDraft {
        SpawnDraft {
            team: "atm-dev".to_string(),
            member: "arch-ctm".to_string(),
            model: "codex-5".to_string(),
            agent_type: "general-purpose".to_string(),
            pane_mode: PaneMode::NewPane,
            worktree: None,
            color: None,
        }
    }

    #[test]
    fn test_spawn_non_interactive_with_yes_flag() {
        assert!(!should_use_interactive(false, true));
        assert!(enforce_tty_guard(false, true).is_ok());
    }

    #[test]
    fn test_spawn_dry_run_output() {
        let out = build_dry_run_output(&draft(), &RuntimeKind::Codex, &PanelState::default());
        assert!(out.contains("[dry-run] What would happen:"));
        assert!(out.contains("'atm' 'teams' 'spawn'"));
        assert!(out.contains("No changes made (dry-run)."));
    }

    #[test]
    fn test_spawn_tty_guard_rejects_non_tty_stdin() {
        let err = enforce_tty_guard(false, false).unwrap_err().to_string();
        assert!(err.contains("interactive mode requires a terminal"));
    }

    #[test]
    fn test_spawn_pane_mode_validation() {
        assert!(parse_pane_mode("new-pane").is_ok());
        assert!(parse_pane_mode("existing-pane").is_ok());
        assert!(parse_pane_mode("current-pane").is_ok());
        assert!(parse_pane_mode("nope").is_err());
    }

    #[test]
    fn test_build_teams_spawn_args_includes_color_when_set() {
        let mut d = draft();
        d.color = Some("cyan".to_string());
        let args = build_teams_spawn_args(&d, &RuntimeKind::Claude);
        let color_idx = args.iter().position(|a| a == "--color");
        assert!(color_idx.is_some(), "--color flag should be present");
        assert_eq!(args[color_idx.unwrap() + 1], "cyan");
    }

    #[test]
    fn test_build_teams_spawn_args_omits_color_when_none() {
        let args = build_teams_spawn_args(&draft(), &RuntimeKind::Claude);
        assert!(!args.contains(&"--color".to_string()));
    }

    #[test]
    fn test_spawn_apply_edits_parses_comma_separated() {
        let mut d = draft();
        apply_edits(&mut d, "1=atm-qa,2=quality-mgr,5=existing-pane").unwrap();
        assert_eq!(d.team, "atm-qa");
        assert_eq!(d.member, "quality-mgr");
        assert_eq!(d.pane_mode, PaneMode::ExistingPane);
    }

    #[test]
    fn test_validate_spawn_request_blocks_new_pane_without_tmux() {
        let d = draft();
        let panel = PanelState::default();
        let err = validate_spawn_request(&d, &panel, false)
            .unwrap_err()
            .to_string();
        assert!(err.contains("requires an active tmux session"));
    }

    #[test]
    fn test_validate_spawn_request_existing_pane_requires_selection() {
        let mut d = draft();
        d.pane_mode = PaneMode::ExistingPane;
        let panel = PanelState {
            tmux_context: Some(TmuxContext {
                session: "work".to_string(),
                window_index: "1".to_string(),
                window_name: "dev".to_string(),
                pane_id: "%1".to_string(),
            }),
            available_panes: vec![TmuxPane {
                index: "0".to_string(),
                title: "shell".to_string(),
            }],
            selected_existing_pane: None,
            member_running: false,
        };
        let err = validate_spawn_request(&d, &panel, true)
            .unwrap_err()
            .to_string();
        assert!(err.contains("requires pane selection"));
    }

    #[test]
    fn test_apply_panel_edits_accepts_existing_pane_selection() {
        let mut d = draft();
        d.pane_mode = PaneMode::ExistingPane;
        let mut panel = PanelState::default();
        apply_panel_edits(&mut d, &mut panel, "7=2").unwrap();
        assert_eq!(panel.selected_existing_pane.as_deref(), Some("2"));
    }

    #[test]
    fn test_codex_runtime_defaults_to_claude_sonnet_when_model_missing() {
        assert_eq!(
            default_model_for_runtime(&RuntimeKind::Codex),
            "claude-sonnet-4-5"
        );
    }

    #[test]
    fn test_claude_runtime_does_not_receive_codex_default_model() {
        assert_eq!(default_model_for_runtime(&RuntimeKind::Claude), "unknown");
    }
}
