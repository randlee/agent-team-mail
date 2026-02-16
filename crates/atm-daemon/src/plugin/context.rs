use crate::roster::RosterService;
use super::MailService;
use agent_team_mail_core::config::Config;
use agent_team_mail_core::context::SystemContext;
use agent_team_mail_core::toml;
use std::sync::Arc;

/// Shared services available to plugins during init and runtime
#[derive(Clone)]
pub struct PluginContext {
    /// System context (hostname, platform, claude root, repo info)
    pub system: Arc<SystemContext>,
    /// Mail service for reading/writing inbox messages
    pub mail: Arc<MailService>,
    /// Application configuration
    pub config: Arc<Config>,
    /// Roster service for managing synthetic team members
    pub roster: Arc<RosterService>,
}

impl PluginContext {
    pub fn new(
        system: Arc<SystemContext>,
        mail: Arc<MailService>,
        config: Arc<Config>,
        roster: Arc<RosterService>,
    ) -> Self {
        Self {
            system,
            mail,
            config,
            roster,
        }
    }

    /// Get plugin-specific config section from .atm.toml [plugins.<name>]
    pub fn plugin_config(&self, plugin_name: &str) -> Option<&toml::Table> {
        self.config.plugin_config(plugin_name)
    }
}
