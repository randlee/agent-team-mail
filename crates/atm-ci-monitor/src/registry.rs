//! Provider registry for runtime registration of CI providers.

use crate::provider::ErasedCiProvider;
use crate::types::CiProviderError;
use std::collections::HashMap;
use std::sync::Arc;

pub type CiFactoryFn = Arc<
    dyn Fn(Option<&toml::Table>) -> Result<Box<dyn ErasedCiProvider>, CiProviderError>
        + Send
        + Sync,
>;

#[derive(Clone)]
pub struct CiProviderFactory {
    pub name: String,
    pub description: String,
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

#[derive(Debug, Clone)]
pub struct CiProviderRegistry {
    factories: HashMap<String, CiProviderFactory>,
}

impl CiProviderRegistry {
    pub fn new() -> Self {
        Self {
            factories: HashMap::new(),
        }
    }

    pub fn register(&mut self, factory: CiProviderFactory) {
        self.factories.insert(factory.name.clone(), factory);
    }

    pub fn create_provider(
        &self,
        name: &str,
        config: Option<&toml::Table>,
    ) -> Result<Box<dyn ErasedCiProvider>, CiProviderError> {
        let factory = self.factories.get(name).ok_or_else(|| {
            CiProviderError::provider(format!("CI provider '{name}' not registered"))
        })?;
        (factory.create)(config)
    }

    pub fn list_providers(&self) -> Vec<&str> {
        let mut names: Vec<&str> = self.factories.keys().map(|s| s.as_str()).collect();
        names.sort_unstable();
        names
    }

    pub fn len(&self) -> usize {
        self.factories.len()
    }

    pub fn is_empty(&self) -> bool {
        self.factories.is_empty()
    }

    #[cfg(test)]
    pub fn has_provider(&self, name: &str) -> bool {
        self.factories.contains_key(name)
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
    use crate::mock_support::MockCiProvider;

    fn create_test_factory(name: &str, description: &str) -> CiProviderFactory {
        CiProviderFactory {
            name: name.to_string(),
            description: description.to_string(),
            create: Arc::new(move |_config| {
                Ok(Box::new(MockCiProvider::new()) as Box<dyn ErasedCiProvider>)
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
        registry.register(create_test_factory("github", "GitHub Actions provider"));

        assert_eq!(registry.len(), 1);
        assert!(registry.has_provider("github"));
        assert!(!registry.has_provider("azure"));
    }

    #[test]
    fn test_registry_create_provider() {
        let mut registry = CiProviderRegistry::new();
        registry.register(create_test_factory("github", "GitHub Actions provider"));

        let provider = registry.create_provider("github", None).unwrap();
        assert_eq!(provider.provider_name(), "MockCiProvider");
    }

    #[test]
    fn test_registry_create_provider_not_found() {
        let registry = CiProviderRegistry::new();
        let result = registry.create_provider("missing-provider", None);
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("not registered"));
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
        assert_eq!(registry.len(), 1);
        assert!(registry.has_provider("github"));
    }
}
