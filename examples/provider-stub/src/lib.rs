//! Example external issue provider for atm-daemon
//!
//! This is a stub provider demonstrating how to build an external issue provider
//! that can be dynamically loaded by atm-daemon.
//!
//! # Building
//!
//! ```bash
//! cargo build --release
//! ```
//!
//! This produces a shared library at:
//! - macOS: `target/release/libatm_provider_stub.dylib`
//! - Linux: `target/release/libatm_provider_stub.so`
//! - Windows: `target/release/atm_provider_stub.dll`
//!
//! # Installing
//!
//! Copy the library to `~/.config/atm/providers/` (or `$ATM_HOME/providers/`):
//!
//! ```bash
//! mkdir -p ~/.config/atm/providers
//! cp target/release/libatm_provider_stub.dylib ~/.config/atm/providers/
//! ```
//!
//! # Usage
//!
//! In your `.atm.toml`:
//!
//! ```toml
//! [plugins.issues]
//! enabled = true
//! provider = "stub"
//! ```
//!
//! The daemon will automatically discover and load this provider.

use agent_team_mail_daemon::plugins::issues::{
    ErasedIssueProvider, Issue, IssueComment, IssueFilter, IssueProvider, ProviderFactory,
};
use agent_team_mail_daemon::plugin::PluginError;
use std::sync::Arc;

/// Stub issue provider for demonstration
#[derive(Debug)]
pub struct StubProvider {
    name: String,
}

impl StubProvider {
    pub fn new(name: String) -> Self {
        Self { name }
    }
}

impl IssueProvider for StubProvider {
    async fn list_issues(&self, _filter: &IssueFilter) -> Result<Vec<Issue>, PluginError> {
        Err(PluginError::Provider {
            message: format!("Stub provider '{}' not yet implemented", self.name),
            source: None,
        })
    }

    async fn get_issue(&self, number: u64) -> Result<Issue, PluginError> {
        Err(PluginError::Provider {
            message: format!(
                "Stub provider '{}' not yet implemented (issue {})",
                self.name, number
            ),
            source: None,
        })
    }

    async fn add_comment(&self, issue_number: u64, _body: &str) -> Result<IssueComment, PluginError> {
        Err(PluginError::Provider {
            message: format!(
                "Stub provider '{}' not yet implemented (comment on issue {})",
                self.name, issue_number
            ),
            source: None,
        })
    }

    async fn list_comments(&self, issue_number: u64) -> Result<Vec<IssueComment>, PluginError> {
        Err(PluginError::Provider {
            message: format!(
                "Stub provider '{}' not yet implemented (comments for issue {})",
                self.name, issue_number
            ),
            source: None,
        })
    }

    fn provider_name(&self) -> &str {
        &self.name
    }
}

/// C-ABI function that creates a provider factory
///
/// This function MUST be exported with `#[no_mangle]` and `extern "C"`.
/// The daemon will look for this symbol when loading the library.
///
/// # Safety
///
/// The returned pointer must be created with `Box::into_raw()` and will be
/// freed by the daemon using `Box::from_raw()`.
#[no_mangle]
pub extern "C" fn atm_create_provider_factory() -> *mut ProviderFactory {
    let factory = ProviderFactory {
        name: "stub".to_string(),
        description: "Example stub provider for demonstration".to_string(),
        create: Arc::new(|_config| {
            Ok(Box::new(StubProvider::new("stub".to_string())) as Box<dyn ErasedIssueProvider>)
        }),
    };

    Box::into_raw(Box::new(factory))
}
