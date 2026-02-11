//! Agent addressing parser (agent@team syntax)

use anyhow::Result;

/// Parse agent address in the format "agent" or "agent@team"
///
/// # Arguments
///
/// * `address` - Address string (e.g., "ci-agent" or "ci-agent@backend")
/// * `team_flag` - Optional --team flag override
/// * `default_team` - Default team from config
///
/// # Returns
///
/// Tuple of (agent_name, team_name)
pub fn parse_address(
    address: &str,
    team_flag: &Option<String>,
    default_team: &str,
) -> Result<(String, String)> {
    // Split on @ if present
    if let Some(pos) = address.find('@') {
        let agent = &address[..pos];
        let team = &address[pos + 1..];

        if agent.is_empty() {
            anyhow::bail!("Invalid address format: agent name cannot be empty");
        }

        if team.is_empty() {
            anyhow::bail!("Invalid address format: team name cannot be empty");
        }

        // --team flag overrides @team syntax
        let final_team = team_flag.as_ref().map(|s| s.as_str()).unwrap_or(team);

        Ok((agent.to_string(), final_team.to_string()))
    } else {
        // No @ symbol - use team flag or default
        let team = team_flag
            .as_ref()
            .map(|s| s.as_str())
            .unwrap_or(default_team);

        Ok((address.to_string(), team.to_string()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_simple_address() {
        let (agent, team) = parse_address("ci-agent", &None, "default-team").unwrap();
        assert_eq!(agent, "ci-agent");
        assert_eq!(team, "default-team");
    }

    #[test]
    fn test_parse_address_with_at() {
        let (agent, team) = parse_address("ci-agent@backend", &None, "default-team").unwrap();
        assert_eq!(agent, "ci-agent");
        assert_eq!(team, "backend");
    }

    #[test]
    fn test_parse_address_with_team_flag() {
        let team_flag = Some("override-team".to_string());
        let (agent, team) = parse_address("ci-agent", &team_flag, "default-team").unwrap();
        assert_eq!(agent, "ci-agent");
        assert_eq!(team, "override-team");
    }

    #[test]
    fn test_parse_address_team_flag_overrides_at() {
        let team_flag = Some("override-team".to_string());
        let (agent, team) = parse_address("ci-agent@backend", &team_flag, "default-team").unwrap();
        assert_eq!(agent, "ci-agent");
        assert_eq!(team, "override-team");
    }

    #[test]
    fn test_parse_address_empty_agent() {
        let result = parse_address("@backend", &None, "default-team");
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("agent name cannot be empty"));
    }

    #[test]
    fn test_parse_address_empty_team() {
        let result = parse_address("ci-agent@", &None, "default-team");
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("team name cannot be empty"));
    }
}
