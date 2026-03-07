//! Shared spawn UX helpers.

use anyhow::{Result, anyhow};

/// Spawn pane placement mode.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PaneMode {
    NewPane,
    ExistingPane,
    CurrentPane,
}

impl PaneMode {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::NewPane => "new-pane",
            Self::ExistingPane => "existing-pane",
            Self::CurrentPane => "current-pane",
        }
    }
}

/// Mutable spawn review state used by the interactive panel.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SpawnDraft {
    pub team: String,
    pub member: String,
    pub model: String,
    pub agent_type: String,
    pub pane_mode: PaneMode,
    pub worktree: Option<String>,
}

/// Parse `pane-mode` into the canonical enum.
pub fn parse_pane_mode(raw: &str) -> Result<PaneMode> {
    match raw.trim().to_ascii_lowercase().as_str() {
        "new-pane" => Ok(PaneMode::NewPane),
        "existing-pane" => Ok(PaneMode::ExistingPane),
        "current-pane" => Ok(PaneMode::CurrentPane),
        _ => Err(anyhow!(
            "invalid pane mode '{raw}'. valid: new-pane, existing-pane, current-pane"
        )),
    }
}

/// Apply one or more `n=value` edits (comma-separated) to a draft.
pub fn apply_edits(draft: &mut SpawnDraft, edits: &str) -> Result<()> {
    for pair in edits.split(',') {
        let pair = pair.trim();
        if pair.is_empty() {
            continue;
        }
        let Some((idx_raw, value_raw)) = pair.split_once('=') else {
            return Err(anyhow!(
                "invalid edit '{pair}'. expected n=value (for example: 1=atm-dev)"
            ));
        };
        let idx: u8 = idx_raw
            .trim()
            .parse()
            .map_err(|_| anyhow!("invalid field index '{idx_raw}'"))?;
        let value = value_raw.trim();
        match idx {
            1 => draft.team = value.to_string(),
            2 => draft.member = value.to_string(),
            3 => draft.model = value.to_string(),
            4 => draft.agent_type = value.to_string(),
            5 => draft.pane_mode = parse_pane_mode(value)?,
            6 => {
                if value.is_empty() || value.eq_ignore_ascii_case("(none)") {
                    draft.worktree = None;
                } else {
                    draft.worktree = Some(value.to_string());
                }
            }
            _ => return Err(anyhow!("unknown field index '{idx}'. valid fields: 1..6")),
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn base_draft() -> SpawnDraft {
        SpawnDraft {
            team: "atm-dev".to_string(),
            member: "arch-ctm".to_string(),
            model: "unknown".to_string(),
            agent_type: "general-purpose".to_string(),
            pane_mode: PaneMode::NewPane,
            worktree: None,
        }
    }

    #[test]
    fn test_parse_pane_mode_validation() {
        assert_eq!(parse_pane_mode("new-pane").unwrap(), PaneMode::NewPane);
        assert_eq!(
            parse_pane_mode("existing-pane").unwrap(),
            PaneMode::ExistingPane
        );
        assert_eq!(
            parse_pane_mode("current-pane").unwrap(),
            PaneMode::CurrentPane
        );
        assert!(parse_pane_mode("invalid").is_err());
    }

    #[test]
    fn test_apply_edits_parses_comma_separated() {
        let mut draft = base_draft();
        apply_edits(
            &mut draft,
            "1=atm-qa,2=quality-mgr,3=claude-haiku-4-5,5=existing-pane",
        )
        .unwrap();
        assert_eq!(draft.team, "atm-qa");
        assert_eq!(draft.member, "quality-mgr");
        assert_eq!(draft.model, "claude-haiku-4-5");
        assert_eq!(draft.pane_mode, PaneMode::ExistingPane);
    }
}
