//! Provider registry for runtime registration of issue providers

use super::provider::ErasedIssueProvider;
use crate::plugin::PluginError;
use std::collections::HashMap;
use std::sync::Arc;

/// A factory function that creates an issue provider instance
pub type FactoryFn = Arc<
    dyn Fn(Option<&toml::Table>) -> Result<Box<dyn ErasedIssueProvider>, PluginError>
        + Send
        + Sync,
>;

/// A factory that can create an issue provider instance
#[derive(Clone)]
pub struct ProviderFactory {
    /// Provider name (e.g., "github", "gitlab")
    pub name: String,
    /// Human-readable description
    pub description: String,
    /// Factory function: takes optional config, returns a provider
    pub create: FactoryFn,
}

impl std::fmt::Debug for ProviderFactory {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ProviderFactory")
            .field("name", &self.name)
            .field("description", &self.description)
            .field("create", &"<factory_fn>")
            .finish()
    }
}

/// Registry for issue providers
///
/// Allows runtime registration of providers from built-in and dynamically loaded sources.
#[derive(Debug, Clone)]
pub struct ProviderRegistry {
    /// Factory functions keyed by provider name (e.g., "github", "gitlab")
    factories: HashMap<String, ProviderFactory>,
}

impl ProviderRegistry {
    /// Create a new empty registry
    pub fn new() -> Self {
        Self {
            factories: HashMap::new(),
        }
    }

    /// Register a provider factory
    ///
    /// If a factory with the same name already exists, it will be replaced.
    pub fn register(&mut self, factory: ProviderFactory) {
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
    ) -> Result<Box<dyn ErasedIssueProvider>, PluginError> {
        let factory = self.factories.get(name).ok_or_else(|| {
            PluginError::Provider {
                message: format!("Provider '{name}' not registered"),
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

impl Default for ProviderRegistry {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::plugins::issues::mock_provider::MockProvider;

    fn create_test_factory(name: &str, description: &str) -> ProviderFactory {
        ProviderFactory {
            name: name.to_string(),
            description: description.to_string(),
            create: Arc::new(move |_config| {
                Ok(Box::new(MockProvider::new()) as Box<dyn ErasedIssueProvider>)
            }),
        }
    }

    #[test]
    fn test_registry_new() {
        let registry = ProviderRegistry::new();
        assert!(registry.is_empty());
        assert_eq!(registry.len(), 0);
    }

    #[test]
    fn test_registry_register() {
        let mut registry = ProviderRegistry::new();
        let factory = create_test_factory("test-provider", "Test provider");

        registry.register(factory);

        assert_eq!(registry.len(), 1);
        assert!(registry.has_provider("test-provider"));
        assert!(!registry.has_provider("other-provider"));
    }

    #[test]
    fn test_registry_create_provider() {
        let mut registry = ProviderRegistry::new();
        let factory = create_test_factory("test-provider", "Test provider");
        registry.register(factory);

        let provider = registry.create_provider("test-provider", None);
        assert!(provider.is_ok());
        let provider = provider.unwrap();
        assert_eq!(provider.provider_name(), "MockProvider");
    }

    #[test]
    fn test_registry_create_provider_not_found() {
        let registry = ProviderRegistry::new();
        let result = registry.create_provider("missing-provider", None);
        assert!(result.is_err());
        assert!(result
            .unwrap_err()
            .to_string()
            .contains("not registered"));
    }

    #[test]
    fn test_registry_list_providers() {
        let mut registry = ProviderRegistry::new();
        registry.register(create_test_factory("provider-a", "Provider A"));
        registry.register(create_test_factory("provider-c", "Provider C"));
        registry.register(create_test_factory("provider-b", "Provider B"));

        let providers = registry.list_providers();
        assert_eq!(providers, vec!["provider-a", "provider-b", "provider-c"]);
    }

    #[test]
    fn test_registry_replace_factory() {
        let mut registry = ProviderRegistry::new();
        registry.register(create_test_factory("test-provider", "First version"));
        assert_eq!(registry.len(), 1);

        registry.register(create_test_factory("test-provider", "Second version"));
        assert_eq!(registry.len(), 1); // Still only one entry
        assert!(registry.has_provider("test-provider"));
    }

    #[test]
    fn test_registry_clone() {
        let mut registry = ProviderRegistry::new();
        registry.register(create_test_factory("test-provider", "Test"));

        let cloned = registry.clone();
        assert_eq!(cloned.len(), registry.len());
        assert!(cloned.has_provider("test-provider"));
    }
}
