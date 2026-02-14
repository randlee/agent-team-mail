//! Provider registry for runtime registration of CI providers

use super::provider::ErasedCiProvider;
use crate::plugin::PluginError;
use std::collections::HashMap;
use std::sync::Arc;

/// A factory function that creates a CI provider instance
pub type CiFactoryFn = Arc<
    dyn Fn(Option<&toml::Table>) -> Result<Box<dyn ErasedCiProvider>, PluginError> + Send + Sync,
>;

/// A factory that can create a CI provider instance
#[derive(Clone)]
pub struct CiProviderFactory {
    /// Provider name (e.g., "github", "azure-pipelines")
    pub name: String,
    /// Human-readable description
    pub description: String,
    /// Factory function: takes optional config, returns a provider
    pub create: CiFactoryFn,
}

impl std::fmt::Debug for CiProviderFactory {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CiProviderFactory")
            .field("name", &self.name)
            .field("description", &self.description)
            .field("create", &"<factory_fn>")
            .finish()
    }
}

/// Registry for CI providers
///
/// Allows runtime registration of providers from built-in and dynamically loaded sources.
#[derive(Debug, Clone)]
pub struct CiProviderRegistry {
    /// Factory functions keyed by provider name (e.g., "github", "azure-pipelines")
    factories: HashMap<String, CiProviderFactory>,
}

impl CiProviderRegistry {
    /// Create a new empty registry
    pub fn new() -> Self {
        Self {
            factories: HashMap::new(),
        }
    }

    /// Register a CI provider factory
    ///
    /// If a factory with the same name already exists, it will be replaced.
    pub fn register(&mut self, factory: CiProviderFactory) {
        self.factories.insert(factory.name.clone(), factory);
    }

    /// Create a provider by name with optional config
    ///
    /// # Arguments
    ///
    /// * `name` - The provider name (e.g., "github")
    /// * `config` - Optional TOML config table for provider-specific settings
    ///
    /// # Errors
    ///
    /// Returns `PluginError::Provider` if the provider is not registered or creation fails.
    pub fn create_provider(
        &self,
        name: &str,
        config: Option<&toml::Table>,
    ) -> Result<Box<dyn ErasedCiProvider>, PluginError> {
        let factory = self.factories.get(name).ok_or_else(|| {
            PluginError::Provider {
                message: format!("CI provider '{name}' not registered"),
                source: None,
            }
        })?;

        (factory.create)(config)
    }

    /// List registered provider names
    pub fn list_providers(&self) -> Vec<&str> {
        let mut names: Vec<&str> = self.factories.keys().map(|s| s.as_str()).collect();
        names.sort_unstable();
        names
    }

    /// Check if a provider is registered
    pub fn has_provider(&self, name: &str) -> bool {
        self.factories.contains_key(name)
    }

    /// Get the number of registered providers
    pub fn len(&self) -> usize {
        self.factories.len()
    }

    /// Check if the registry is empty
    pub fn is_empty(&self) -> bool {
        self.factories.is_empty()
    }
}

impl Default for CiProviderRegistry {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::plugins::ci_monitor::github::GitHubActionsProvider;

    fn create_test_factory(name: &str, description: &str) -> CiProviderFactory {
        CiProviderFactory {
            name: name.to_string(),
            description: description.to_string(),
            create: Arc::new(move |_config| {
                Ok(Box::new(GitHubActionsProvider::new(
                    "owner".to_string(),
                    "repo".to_string(),
                )) as Box<dyn ErasedCiProvider>)
            }),
        }
    }

    #[test]
    fn test_registry_new() {
        let registry = CiProviderRegistry::new();
        assert!(registry.is_empty());
        assert_eq!(registry.len(), 0);
    }

    #[test]
    fn test_registry_register() {
        let mut registry = CiProviderRegistry::new();
        let factory = create_test_factory("github", "GitHub Actions provider");

        registry.register(factory);

        assert_eq!(registry.len(), 1);
        assert!(registry.has_provider("github"));
        assert!(!registry.has_provider("azure"));
    }

    #[test]
    fn test_registry_create_provider() {
        let mut registry = CiProviderRegistry::new();
        let factory = create_test_factory("github", "GitHub Actions provider");
        registry.register(factory);

        let provider = registry.create_provider("github", None);
        assert!(provider.is_ok());
        let provider = provider.unwrap();
        assert_eq!(provider.provider_name(), "GitHub Actions");
    }

    #[test]
    fn test_registry_create_provider_not_found() {
        let registry = CiProviderRegistry::new();
        let result = registry.create_provider("missing-provider", None);
        assert!(result.is_err());
        assert!(result
            .unwrap_err()
            .to_string()
            .contains("not registered"));
    }

    #[test]
    fn test_registry_list_providers() {
        let mut registry = CiProviderRegistry::new();
        registry.register(create_test_factory("github", "GitHub Actions"));
        registry.register(create_test_factory("azure", "Azure Pipelines"));
        registry.register(create_test_factory("gitlab", "GitLab CI"));

        let providers = registry.list_providers();
        assert_eq!(providers, vec!["azure", "github", "gitlab"]);
    }

    #[test]
    fn test_registry_replace_factory() {
        let mut registry = CiProviderRegistry::new();
        registry.register(create_test_factory("github", "First version"));
        assert_eq!(registry.len(), 1);

        registry.register(create_test_factory("github", "Second version"));
        assert_eq!(registry.len(), 1); // Still only one entry
        assert!(registry.has_provider("github"));
    }

    #[test]
    fn test_registry_clone() {
        let mut registry = CiProviderRegistry::new();
        registry.register(create_test_factory("github", "GitHub Actions"));

        let cloned = registry.clone();
        assert_eq!(cloned.len(), registry.len());
        assert!(cloned.has_provider("github"));
    }
}
