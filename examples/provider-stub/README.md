# External Provider Stub Example

This example demonstrates how to create an external issue provider for `atm-daemon` that can be dynamically loaded at runtime.

## Overview

External providers are Rust libraries compiled as shared libraries (cdylib) that export a specific C-ABI function. The daemon discovers and loads these libraries from a provider directory.

## Building

```bash
cd examples/provider-stub
cargo build --release
```

This produces a shared library:
- **macOS**: `target/release/libatm_provider_stub.dylib`
- **Linux**: `target/release/libatm_provider_stub.so`
- **Windows**: `target/release/atm_provider_stub.dll`

## Installing

Copy the built library to the providers directory:

```bash
# Default location
mkdir -p ~/.config/atm/providers
cp target/release/libatm_provider_stub.dylib ~/.config/atm/providers/

# Or use ATM_HOME if set
mkdir -p $ATM_HOME/providers
cp target/release/libatm_provider_stub.dylib $ATM_HOME/providers/
```

## Configuration

In your `.atm.toml`, specify the provider:

```toml
[plugins.issues]
enabled = true
provider = "stub"
```

Or load it explicitly by path:

```toml
[plugins.issues]
enabled = true

[plugins.issues.providers]
stub = "~/.config/atm/providers/libatm_provider_stub.dylib"
```

## Architecture

### Required Export

Every provider library MUST export this C-ABI function:

```rust
#[no_mangle]
pub extern "C" fn atm_create_provider_factory() -> *mut ProviderFactory;
```

The daemon calls this function once when loading the library. The function must:
1. Create a `ProviderFactory` struct
2. Return it as a raw pointer using `Box::into_raw()`

The daemon takes ownership and will free the pointer using `Box::from_raw()`.

### Provider Implementation

Implement the `IssueProvider` trait:

```rust
use agent_team_mail_daemon::plugins::issues::{IssueProvider, Issue, IssueFilter, IssueComment};
use agent_team_mail_daemon::plugin::PluginError;

#[derive(Debug)]
pub struct MyProvider {
    // Provider state
}

impl IssueProvider for MyProvider {
    async fn list_issues(&self, filter: &IssueFilter) -> Result<Vec<Issue>, PluginError> {
        // Implementation
    }

    async fn get_issue(&self, number: u64) -> Result<Issue, PluginError> {
        // Implementation
    }

    async fn add_comment(&self, issue_number: u64, body: &str) -> Result<IssueComment, PluginError> {
        // Implementation
    }

    async fn list_comments(&self, issue_number: u64) -> Result<Vec<IssueComment>, PluginError> {
        // Implementation
    }

    fn provider_name(&self) -> &str {
        "my-provider"
    }
}
```

### Factory Function

The factory receives optional configuration from `.atm.toml` and returns a boxed trait object:

```rust
use std::sync::Arc;
use agent_team_mail_daemon::plugins::issues::{ProviderFactory, ErasedIssueProvider};

#[no_mangle]
pub extern "C" fn atm_create_provider_factory() -> *mut ProviderFactory {
    let factory = ProviderFactory {
        name: "my-provider".to_string(),
        description: "My custom issue provider".to_string(),
        create: Arc::new(|config| {
            // Parse config if needed
            let provider = MyProvider::new(/* ... */);
            Ok(Box::new(provider) as Box<dyn ErasedIssueProvider>)
        }),
    };

    Box::into_raw(Box::new(factory))
}
```

## Security Considerations

- Only load trusted libraries from secure locations
- Provider libraries run in the daemon's process with full privileges
- Validate all inputs in provider implementations
- Handle errors gracefully without panicking

## See Also

- Provider trait: `crates/atm-daemon/src/plugins/issues/provider.rs`
- Registry: `crates/atm-daemon/src/plugins/issues/registry.rs`
- Loader: `crates/atm-daemon/src/plugins/issues/loader.rs`
