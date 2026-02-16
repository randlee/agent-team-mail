use agent_team_mail_core::io::{inbox_append, InboxError, WriteOutcome};
use agent_team_mail_core::schema::InboxMessage;
use std::path::PathBuf;

/// Thin wrapper around atm-core inbox operations for plugin use
pub struct MailService {
    /// Root path for teams directory (~/.claude/teams/)
    teams_root: PathBuf,
}

impl MailService {
    pub fn new(teams_root: PathBuf) -> Self {
        Self { teams_root }
    }

    /// Get the teams root directory path
    pub fn teams_root(&self) -> &PathBuf {
        &self.teams_root
    }

    /// Send a message to an agent's inbox
    pub fn send(
        &self,
        team: &str,
        agent: &str,
        message: &InboxMessage,
    ) -> Result<WriteOutcome, InboxError> {
        let inbox_path = self.inbox_path(team, agent);
        // Ensure parent directories exist
        if let Some(parent) = inbox_path.parent() {
            std::fs::create_dir_all(parent).map_err(|e| InboxError::Io {
                path: parent.to_path_buf(),
                source: e,
            })?;
        }
        inbox_append(&inbox_path, message, team, agent)
    }

    /// Read all messages from an agent's inbox
    pub fn read_inbox(&self, team: &str, agent: &str) -> Result<Vec<InboxMessage>, InboxError> {
        let inbox_path = self.inbox_path(team, agent);
        if !inbox_path.exists() {
            return Ok(Vec::new());
        }
        let content = std::fs::read(&inbox_path).map_err(|e| InboxError::Io {
            path: inbox_path.clone(),
            source: e,
        })?;
        serde_json::from_slice(&content).map_err(|e| InboxError::Json {
            path: inbox_path,
            source: e,
        })
    }

    /// Get the inbox file path for a team/agent
    fn inbox_path(&self, team: &str, agent: &str) -> PathBuf {
        self.teams_root
            .join(team)
            .join("inboxes")
            .join(format!("{agent}.json"))
    }
}
