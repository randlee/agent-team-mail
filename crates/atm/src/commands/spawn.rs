use agent_team_mail_core::config::{ConfigOverrides, resolve_config};
use agent_team_mail_core::spawn::{PaneMode, SpawnDraft, apply_edits, parse_pane_mode};
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

    let mut draft = SpawnDraft {
        team: args
            .team
            .clone()
            .unwrap_or_else(|| config.core.default_team.clone()),
        member: args
            .member
            .clone()
            .unwrap_or_else(|| runtime_name(&args.runtime).to_string()),
        model: args.model.clone().unwrap_or_else(|| "unknown".to_string()),
        agent_type: args.agent_type.clone(),
        pane_mode: parse_pane_mode(&args.pane_mode)?,
        worktree: args
            .worktree
            .as_ref()
            .map(|p| p.to_string_lossy().to_string()),
    };

    let stdin_is_tty = io::stdin().is_tty();
    enforce_tty_guard(stdin_is_tty, args.yes)?;
    if should_use_interactive(stdin_is_tty, args.yes) {
        run_interactive_panel(&mut draft, &args.runtime)?;
    }

    validate_draft(&draft)?;

    if args.dry_run {
        println!("{}", build_dry_run_output(&draft, &args.runtime));
        return Ok(());
    }

    execute_via_teams_spawn(&draft, &args.runtime)
}

fn execute_via_teams_spawn(draft: &SpawnDraft, runtime: &RuntimeKind) -> Result<()> {
    let mut cmd = Command::new(std::env::current_exe()?);
    cmd.arg("teams")
        .arg("spawn")
        .arg(&draft.member)
        .arg("--runtime")
        .arg(runtime_name(runtime))
        .arg("--team")
        .arg(&draft.team)
        .arg("--model")
        .arg(&draft.model);

    if let Some(worktree) = &draft.worktree {
        cmd.arg("--folder").arg(worktree);
    }
    if draft.pane_mode != PaneMode::NewPane {
        cmd.arg("--env")
            .arg(format!("ATM_SPAWN_PANE_MODE={}", draft.pane_mode.as_str()));
    }

    let status = cmd.status()?;
    if !status.success() {
        anyhow::bail!("spawn failed (teams spawn exited with status {status})");
    }
    Ok(())
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

fn validate_draft(draft: &SpawnDraft) -> Result<()> {
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

fn run_interactive_panel(draft: &mut SpawnDraft, runtime: &RuntimeKind) -> Result<()> {
    loop {
        render_panel(draft, runtime);
        print!("> ");
        io::stdout().flush()?;
        let mut line = String::new();
        io::stdin().read_line(&mut line)?;
        let input = line.trim();
        if input.is_empty() {
            validate_draft(draft)?;
            return Ok(());
        }
        if input.eq_ignore_ascii_case("q") || input == "\u{1b}" {
            anyhow::bail!("spawn cancelled");
        }
        apply_edits(draft, input)?;
        validate_draft(draft)?;
    }
}

fn render_panel(draft: &SpawnDraft, runtime: &RuntimeKind) {
    println!();
    println!("atm spawn — interactive mode");
    println!("Spawning runtime: {}", runtime_name(runtime));
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
    println!();
    println!("Enter to confirm · q to cancel");
    println!("Change items with n=value or n=value,m=value2");
}

fn build_dry_run_output(draft: &SpawnDraft, runtime: &RuntimeKind) -> String {
    let pane_line = match draft.pane_mode {
        PaneMode::NewPane => "tmux action: split-window -h (new pane)",
        PaneMode::ExistingPane => "tmux action: send-keys to selected existing pane",
        PaneMode::CurrentPane => "tmux action: send-keys to current pane",
    };
    let mut cmd = format!(
        "atm teams spawn {} --runtime {} --team {} --model {}",
        shell_quote(&draft.member),
        runtime_name(runtime),
        shell_quote(&draft.team),
        shell_quote(&draft.model)
    );
    if let Some(worktree) = &draft.worktree {
        cmd.push_str(" --folder ");
        cmd.push_str(&shell_quote(worktree));
    }
    format!(
        "[dry-run] What would happen:\n1. {pane_line}\n2. Run command:\n   {cmd}\n3. Register/update member metadata via teams spawn\n\nNo changes made (dry-run)."
    )
}

fn shell_quote(input: &str) -> String {
    if input.is_empty() {
        return "''".to_string();
    }
    format!("'{}'", input.replace('\'', "'\"'\"'"))
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
        }
    }

    #[test]
    fn test_spawn_non_interactive_with_yes_flag() {
        assert!(!should_use_interactive(false, true));
        assert!(enforce_tty_guard(false, true).is_ok());
    }

    #[test]
    fn test_spawn_dry_run_output() {
        let out = build_dry_run_output(&draft(), &RuntimeKind::Codex);
        assert!(out.contains("[dry-run] What would happen:"));
        assert!(out.contains("atm teams spawn"));
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
    fn test_spawn_apply_edits_parses_comma_separated() {
        let mut d = draft();
        apply_edits(&mut d, "1=atm-qa,2=quality-mgr,5=existing-pane").unwrap();
        assert_eq!(d.team, "atm-qa");
        assert_eq!(d.member, "quality-mgr");
        assert_eq!(d.pane_mode, PaneMode::ExistingPane);
    }
}
