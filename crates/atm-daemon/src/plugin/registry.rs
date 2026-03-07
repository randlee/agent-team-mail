use super::traits::ErasedPlugin;
use super::{Capability, Plugin, PluginContext, PluginError, PluginMetadata, PluginState};
use std::sync::Arc;
use tokio::sync::Mutex;

/// Type alias for a plugin wrapped in Arc<Mutex<>> for concurrent access
pub type SharedPlugin = Arc<Mutex<Box<dyn ErasedPlugin>>>;

/// Entry in the registry tracking a plugin and its state
struct PluginEntry {
    plugin: Box<dyn ErasedPlugin>,
    state: PluginState,
    init_error: Option<String>,
}

/// Manages plugin lifecycle and discovery
pub struct PluginRegistry {
    plugins: Vec<PluginEntry>,
}

/// Snapshot of a plugin that failed to initialize and was disabled.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FailedPluginInit {
    pub name: String,
    pub error: String,
}

impl PluginRegistry {
    pub fn new() -> Self {
        Self {
            plugins: Vec::new(),
        }
    }

    /// Register a plugin. It starts in Created state.
    pub fn register<P: Plugin + 'static>(&mut self, plugin: P) {
        self.plugins.push(PluginEntry {
            plugin: Box::new(plugin),
            state: PluginState::Created,
            init_error: None,
        });
    }

    /// Initialize all registered plugins
    pub async fn init_all(&mut self, ctx: &PluginContext) -> Result<(), PluginError> {
        for entry in &mut self.plugins {
            match entry.plugin.init(ctx).await {
                Ok(()) => {
                    entry.state = PluginState::Initialized;
                    entry.init_error = None;
                }
                Err(err) => {
                    entry.state = PluginState::Failed;
                    entry.init_error = Some(err.to_string());
                }
            }
        }
        Ok(())
    }

    /// Get all plugins that failed init and were disabled for this daemon run.
    pub fn failed_init_plugins(&self) -> Vec<FailedPluginInit> {
        self.plugins
            .iter()
            .filter(|e| e.state == PluginState::Failed)
            .map(|e| FailedPluginInit {
                name: e.plugin.metadata().name.to_string(),
                error: e
                    .init_error
                    .clone()
                    .unwrap_or_else(|| "plugin init failed".to_string()),
            })
            .collect()
    }

    /// Get plugin metadata and state by name
    pub fn get_by_name(&self, name: &str) -> Option<(PluginMetadata, PluginState)> {
        self.plugins
            .iter()
            .find(|e| e.plugin.metadata().name == name)
            .map(|e| (e.plugin.metadata(), e.state))
    }

    /// Get metadata for all plugins with a given capability
    pub fn get_by_capability(&self, cap: &Capability) -> Vec<(PluginMetadata, PluginState)> {
        self.plugins
            .iter()
            .filter(|e| e.plugin.metadata().capabilities.contains(cap))
            .map(|e| (e.plugin.metadata(), e.state))
            .collect()
    }

    /// Number of registered plugins
    pub fn len(&self) -> usize {
        self.plugins.len()
    }

    pub fn is_empty(&self) -> bool {
        self.plugins.is_empty()
    }

    /// Get the state of a plugin by name
    pub fn state_of(&self, name: &str) -> Option<PluginState> {
        self.plugins
            .iter()
            .find(|e| e.plugin.metadata().name == name)
            .map(|e| e.state)
    }

    /// Update the state of a plugin by name
    pub fn set_state(&mut self, name: &str, state: PluginState) -> bool {
        if let Some(entry) = self
            .plugins
            .iter_mut()
            .find(|e| e.plugin.metadata().name == name)
        {
            entry.state = state;
            true
        } else {
            false
        }
    }

    /// Take all plugins out of the registry for task spawning.
    /// Each plugin is wrapped in Arc<Mutex<>> for safe concurrent access.
    /// Transitions initialized plugins to Running state.
    /// Failed-init plugins remain in the registry for status surfacing.
    pub fn take_plugins(&mut self) -> Vec<(PluginMetadata, SharedPlugin)> {
        let mut running_plugins: Vec<(PluginMetadata, SharedPlugin)> = Vec::new();
        let mut retained: Vec<PluginEntry> = Vec::new();

        for mut entry in self.plugins.drain(..) {
            if entry.state == PluginState::Initialized {
                entry.state = PluginState::Running;
                let metadata = entry.plugin.metadata();
                running_plugins.push((metadata, Arc::new(Mutex::new(entry.plugin))));
            } else {
                retained.push(entry);
            }
        }

        self.plugins = retained;
        running_plugins
    }
}

impl Default for PluginRegistry {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::plugin::MailService;
    use crate::roster::RosterService;
    use agent_team_mail_core::config::Config;
    use agent_team_mail_core::context::{Platform, SystemContext};
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::{Arc, Mutex as StdMutex};
    use tokio_util::sync::CancellationToken;

    struct OkPlugin;
    struct FailPlugin;
    struct RuntimeErrPlugin;
    struct RuntimePanicPlugin;
    struct ToggleInitPlugin {
        fail_init: Arc<AtomicBool>,
        recovery_log: Arc<StdMutex<Vec<String>>>,
    }

    impl Plugin for OkPlugin {
        fn metadata(&self) -> PluginMetadata {
            PluginMetadata {
                name: "ok_plugin",
                version: "0.1.0",
                description: "ok",
                capabilities: vec![],
            }
        }

        async fn init(&mut self, _ctx: &PluginContext) -> Result<(), PluginError> {
            Ok(())
        }

        async fn run(
            &mut self,
            _cancel: tokio_util::sync::CancellationToken,
        ) -> Result<(), PluginError> {
            Ok(())
        }

        async fn shutdown(&mut self) -> Result<(), PluginError> {
            Ok(())
        }
    }

    impl Plugin for FailPlugin {
        fn metadata(&self) -> PluginMetadata {
            PluginMetadata {
                name: "fail_plugin",
                version: "0.1.0",
                description: "fail",
                capabilities: vec![],
            }
        }

        async fn init(&mut self, _ctx: &PluginContext) -> Result<(), PluginError> {
            Err(PluginError::Init {
                message: "bad config".to_string(),
                source: None,
            })
        }

        async fn run(
            &mut self,
            _cancel: tokio_util::sync::CancellationToken,
        ) -> Result<(), PluginError> {
            Ok(())
        }

        async fn shutdown(&mut self) -> Result<(), PluginError> {
            Ok(())
        }
    }

    impl Plugin for RuntimeErrPlugin {
        fn metadata(&self) -> PluginMetadata {
            PluginMetadata {
                name: "runtime_err_plugin",
                version: "0.1.0",
                description: "runtime error plugin",
                capabilities: vec![],
            }
        }

        async fn init(&mut self, _ctx: &PluginContext) -> Result<(), PluginError> {
            Ok(())
        }

        async fn run(
            &mut self,
            _cancel: tokio_util::sync::CancellationToken,
        ) -> Result<(), PluginError> {
            Err(PluginError::Runtime {
                message: "simulated runtime error".to_string(),
                source: None,
            })
        }

        async fn shutdown(&mut self) -> Result<(), PluginError> {
            Ok(())
        }
    }

    impl Plugin for RuntimePanicPlugin {
        fn metadata(&self) -> PluginMetadata {
            PluginMetadata {
                name: "runtime_panic_plugin",
                version: "0.1.0",
                description: "runtime panic plugin",
                capabilities: vec![],
            }
        }

        async fn init(&mut self, _ctx: &PluginContext) -> Result<(), PluginError> {
            Ok(())
        }

        async fn run(
            &mut self,
            _cancel: tokio_util::sync::CancellationToken,
        ) -> Result<(), PluginError> {
            panic!("simulated runtime panic");
        }

        async fn shutdown(&mut self) -> Result<(), PluginError> {
            Ok(())
        }
    }

    impl Plugin for ToggleInitPlugin {
        fn metadata(&self) -> PluginMetadata {
            PluginMetadata {
                name: "toggle_init_plugin",
                version: "0.1.0",
                description: "toggle init plugin",
                capabilities: vec![],
            }
        }

        async fn init(&mut self, _ctx: &PluginContext) -> Result<(), PluginError> {
            if self.fail_init.load(Ordering::SeqCst) {
                self.recovery_log
                    .lock()
                    .expect("recovery log lock")
                    .push("init_failed".to_string());
                Err(PluginError::Init {
                    message: "simulated config error".to_string(),
                    source: None,
                })
            } else {
                self.recovery_log
                    .lock()
                    .expect("recovery log lock")
                    .push("init_ok".to_string());
                Ok(())
            }
        }

        async fn run(
            &mut self,
            _cancel: tokio_util::sync::CancellationToken,
        ) -> Result<(), PluginError> {
            Ok(())
        }

        async fn shutdown(&mut self) -> Result<(), PluginError> {
            Ok(())
        }
    }

    fn test_context() -> PluginContext {
        let tmp = tempfile::tempdir().unwrap();
        let teams_root = tmp.path().to_path_buf();
        let system = SystemContext::new(
            "test-host".to_string(),
            Platform::Linux,
            std::env::temp_dir().join(".claude"),
            "0.1.0".to_string(),
            "atm-dev".to_string(),
        );
        let config: Config = toml::from_str(
            r#"
[core]
default_team = "atm-dev"
identity = "team-lead"
            "#,
        )
        .unwrap();
        let mail = MailService::new(teams_root.clone());
        let roster = RosterService::new(teams_root);
        PluginContext::new(
            Arc::new(system),
            Arc::new(mail),
            Arc::new(config),
            Arc::new(roster),
        )
    }

    #[tokio::test]
    async fn test_init_all_isolates_failed_plugins() {
        let mut registry = PluginRegistry::new();
        registry.register(FailPlugin);
        registry.register(OkPlugin);
        let ctx = test_context();

        let result = registry.init_all(&ctx).await;
        assert!(result.is_ok(), "init_all must not fail-fast");

        let failed = registry.failed_init_plugins();
        assert_eq!(failed.len(), 1);
        assert_eq!(failed[0].name, "fail_plugin");
        assert!(failed[0].error.contains("bad config"));

        let runnable = registry.take_plugins();
        assert_eq!(runnable.len(), 1, "only healthy plugins should run");
        assert_eq!(runnable[0].0.name, "ok_plugin");
    }

    #[tokio::test]
    async fn test_runtime_faults_are_isolated_to_failing_plugins() {
        let mut registry = PluginRegistry::new();
        registry.register(RuntimeErrPlugin);
        registry.register(RuntimePanicPlugin);
        registry.register(OkPlugin);
        let ctx = test_context();

        registry
            .init_all(&ctx)
            .await
            .expect("init_all must succeed");
        let runnable = registry.take_plugins();
        assert_eq!(runnable.len(), 3);

        let mut handles = Vec::new();
        for (meta, plugin_arc) in runnable {
            let name = meta.name.to_string();
            handles.push(tokio::spawn(async move {
                let mut plugin = plugin_arc.lock().await;
                let result = plugin.run(CancellationToken::new()).await;
                (name, result.map_err(|e| e.to_string()))
            }));
        }

        let mut ok_plugins = Vec::new();
        let mut err_plugins = Vec::new();
        let mut panic_plugins = 0usize;
        for handle in handles {
            match handle.await {
                Ok((name, Ok(()))) => ok_plugins.push(name),
                Ok((name, Err(_))) => err_plugins.push(name),
                Err(join_err) if join_err.is_panic() => panic_plugins += 1,
                Err(join_err) => panic!("unexpected runtime join error: {join_err}"),
            }
        }

        assert!(
            ok_plugins.iter().any(|name| name == "ok_plugin"),
            "healthy plugin must remain healthy despite sibling runtime faults"
        );
        assert!(
            err_plugins.iter().any(|name| name == "runtime_err_plugin"),
            "runtime error should be isolated to runtime_err_plugin"
        );
        assert_eq!(panic_plugins, 1, "exactly one plugin should panic");
    }

    #[tokio::test]
    async fn test_repeated_init_faults_remain_bounded_to_plugin_count() {
        let mut registry = PluginRegistry::new();
        registry.register(FailPlugin);
        registry.register(OkPlugin);
        let ctx = test_context();

        for _ in 0..25 {
            let result = registry.init_all(&ctx).await;
            assert!(result.is_ok(), "daemon init path must remain fail-open");

            let failed = registry.failed_init_plugins();
            assert_eq!(
                failed.len(),
                1,
                "failed-plugin tracking must remain bounded (no unbounded growth)"
            );
            assert_eq!(failed[0].name, "fail_plugin");
            assert_eq!(registry.len(), 2, "registry cardinality must remain stable");
        }
    }

    #[tokio::test]
    async fn test_plugin_recovery_after_config_correction_and_reload() {
        let fail_init = Arc::new(AtomicBool::new(true));
        let recovery_log = Arc::new(StdMutex::new(Vec::<String>::new()));
        let mut registry = PluginRegistry::new();
        registry.register(ToggleInitPlugin {
            fail_init: Arc::clone(&fail_init),
            recovery_log: Arc::clone(&recovery_log),
        });
        let ctx = test_context();

        registry.init_all(&ctx).await.expect("fail-open init");
        let failed_before = registry.failed_init_plugins();
        assert_eq!(failed_before.len(), 1);
        assert_eq!(failed_before[0].name, "toggle_init_plugin");
        assert_eq!(
            registry.state_of("toggle_init_plugin"),
            Some(PluginState::Failed)
        );

        // Simulate corrected config and daemon reload.
        fail_init.store(false, Ordering::SeqCst);
        registry
            .init_all(&ctx)
            .await
            .expect("re-init after correction");

        assert!(
            registry.failed_init_plugins().is_empty(),
            "plugin should recover from failed init after config correction"
        );
        assert_eq!(
            registry.state_of("toggle_init_plugin"),
            Some(PluginState::Initialized)
        );

        let runnable = registry.take_plugins();
        assert_eq!(runnable.len(), 1);
        assert_eq!(runnable[0].0.name, "toggle_init_plugin");

        let entries = recovery_log.lock().expect("recovery log lock").clone();
        assert_eq!(
            entries,
            vec!["init_failed".to_string(), "init_ok".to_string()]
        );
    }
}
