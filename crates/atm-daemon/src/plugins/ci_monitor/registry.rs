pub use agent_team_mail_ci_monitor::{CiProviderFactory, CiProviderRegistry};

use super::provider::ErasedCiProvider;
use super::types::CiProviderError;

pub trait CiProviderRegistryPort: Send + Sync + std::fmt::Debug {
    fn create_provider(
        &self,
        name: &str,
        config: Option<&toml::Table>,
    ) -> Result<Box<dyn ErasedCiProvider>, CiProviderError>;

    fn list_provider_names(&self) -> Vec<String>;

    fn provider_count(&self) -> usize;
}

impl CiProviderRegistryPort for CiProviderRegistry {
    fn create_provider(
        &self,
        name: &str,
        config: Option<&toml::Table>,
    ) -> Result<Box<dyn ErasedCiProvider>, CiProviderError> {
        CiProviderRegistry::create_provider(self, name, config)
    }

    fn list_provider_names(&self) -> Vec<String> {
        self.list_providers()
            .into_iter()
            .map(str::to_string)
            .collect()
    }

    fn provider_count(&self) -> usize {
        self.len()
    }
}
