# External Azure DevOps Pipelines Provider Example

This example demonstrates how to create an external CI provider for Azure DevOps Pipelines that can be dynamically loaded by `atm-daemon`.

## Overview

External CI providers are Rust libraries compiled as shared libraries (cdylib) that export a specific C-ABI function. The daemon discovers and loads these libraries from a provider directory.

This is a **stub implementation** that returns placeholder data. A production implementation would use the `az pipelines` CLI or Azure DevOps REST API.

## Building

```bash
cd examples/ci-provider-azdo
cargo build --release
```

This produces a shared library:
- **macOS**: `target/release/libatm_ci_provider_azdo.dylib`
- **Linux**: `target/release/libatm_ci_provider_azdo.so`
- **Windows**: `target/release/atm_ci_provider_azdo.dll`

## Installing

Copy the built library to the providers directory:

```bash
# Default location
mkdir -p ~/.config/atm/providers
cp target/release/libatm_ci_provider_azdo.dylib ~/.config/atm/providers/

# Or use ATM_HOME if set
mkdir -p $ATM_HOME/providers
cp target/release/libatm_ci_provider_azdo.dylib $ATM_HOME/providers/
```

## Configuration

In your `.atm.toml`, specify the provider:

```toml
[plugins.ci_monitor]
enabled = true
provider = "azure-pipelines"
team = "my-team"
agent = "ci-monitor"
poll_interval_secs = 300

# Provider-specific configuration (passed to the factory function)
[plugins.ci_monitor.azure]
organization = "your-org"
project = "your-project"
repo = "your-repo"
```

Or load it explicitly by path using the `providers` table:

```toml
[plugins.ci_monitor]
enabled = true
provider = "azure-pipelines"
team = "my-team"
agent = "ci-monitor"
poll_interval_secs = 300

# Explicit provider library paths
[plugins.ci_monitor.providers]
azure-pipelines = "~/.config/atm/providers/libatm_ci_provider_azdo.dylib"

# Provider-specific configuration
[plugins.ci_monitor.azure]
organization = "your-org"
project = "your-project"
repo = "your-repo"
```

**Note**: The `providers` table is under `[plugins.ci_monitor.providers]`, not `[plugins.ci_monitor.provider_libraries]`. The parser reads the `providers` key and stores library paths in the `provider_libraries` field internally.

## Architecture

### Required Export

Every CI provider library MUST export this C-ABI function:

```rust
#[no_mangle]
pub extern "C" fn atm_create_ci_provider_factory() -> *mut CiProviderFactory;
```

The daemon calls this function once when loading the library. The function must:
1. Create a `CiProviderFactory` struct
2. Return it as a raw pointer using `Box::into_raw()`

The daemon takes ownership and will free the pointer using `Box::from_raw()`.

### Provider Implementation

Implement the `CiProvider` trait:

```rust
use agent_team_mail_daemon::plugins::ci_monitor::{CiProvider, CiRun, CiFilter};
use agent_team_mail_daemon::plugin::PluginError;

#[derive(Debug)]
pub struct AzurePipelinesProvider {
    organization: String,
    project: String,
    repo: String,
}

impl CiProvider for AzurePipelinesProvider {
    async fn list_runs(&self, filter: &CiFilter) -> Result<Vec<CiRun>, PluginError> {
        // Call az pipelines runs list ...
    }

    async fn get_run(&self, run_id: u64) -> Result<CiRun, PluginError> {
        // Call az pipelines runs show ...
    }

    async fn get_job_log(&self, job_id: u64) -> Result<String, PluginError> {
        // Fetch job logs
    }

    fn provider_name(&self) -> &str {
        "Azure Pipelines"
    }
}
```

### Factory Function

The factory receives optional configuration from `.atm.toml` and returns a boxed trait object:

```rust
use std::sync::Arc;
use agent_team_mail_daemon::plugins::ci_monitor::{CiProviderFactory, ErasedCiProvider};

#[no_mangle]
pub extern "C" fn atm_create_ci_provider_factory() -> *mut CiProviderFactory {
    let factory = CiProviderFactory {
        name: "azure-pipelines".to_string(),
        description: "Azure DevOps Pipelines provider".to_string(),
        create: Arc::new(|config| {
            // Parse config
            let org = config.and_then(|t| t.get("azure")...)
                .unwrap_or("default-org");

            let provider = AzurePipelinesProvider::new(org, ...);
            Ok(Box::new(provider) as Box<dyn ErasedCiProvider>)
        }),
    };

    Box::into_raw(Box::new(factory))
}
```

## Implementation Notes

### Azure Pipelines CLI

The Azure CLI provides pipeline commands:

```bash
# List pipeline runs
az pipelines runs list \
  --organization https://dev.azure.com/your-org \
  --project your-project \
  --output json

# Get run details
az pipelines runs show \
  --id <run-id> \
  --organization https://dev.azure.com/your-org \
  --project your-project \
  --output json

# Get run artifacts (logs)
az pipelines runs artifact download \
  --artifact-name logs \
  --run-id <run-id> \
  --path ./logs
```

### REST API Alternative

Azure DevOps also provides a REST API:

```
GET https://dev.azure.com/{organization}/{project}/_apis/build/builds?api-version=7.0
GET https://dev.azure.com/{organization}/{project}/_apis/build/builds/{buildId}?api-version=7.0
GET https://dev.azure.com/{organization}/{project}/_apis/build/builds/{buildId}/logs?api-version=7.0
```

## Security Considerations

- Only load trusted libraries from secure locations
- Provider libraries run in the daemon's process with full privileges
- Validate all inputs in provider implementations
- Handle errors gracefully without panicking
- Sanitize Azure credentials (use environment variables or Azure CLI auth)

## See Also

- Provider trait: `crates/atm-daemon/src/plugins/ci_monitor/provider.rs`
- Registry: `crates/atm-daemon/src/plugins/ci_monitor/registry.rs`
- GitHub Actions provider: `crates/atm-daemon/src/plugins/ci_monitor/github.rs`
