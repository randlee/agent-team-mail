//! Integration test for dynamic CI provider loading

use std::path::PathBuf;
use std::process::Command;
use tempfile::TempDir;

/// Helper to build the Azure provider stub cdylib
fn build_azure_provider() -> Result<PathBuf, Box<dyn std::error::Error>> {
    let manifest_dir = std::env::var("CARGO_MANIFEST_DIR")?;
    let workspace_root = PathBuf::from(manifest_dir).parent().unwrap().parent().unwrap().to_path_buf();
    let example_dir = workspace_root.join("examples").join("ci-provider-azdo");

    // Build the provider
    let output = Command::new("cargo")
        .arg("build")
        .arg("--release")
        .arg("--manifest-path")
        .arg(example_dir.join("Cargo.toml"))
        .output()?;

    if !output.status.success() {
        return Err(format!(
            "Failed to build Azure provider: {}",
            String::from_utf8_lossy(&output.stderr)
        )
        .into());
    }

    // Determine library extension based on platform
    #[cfg(target_os = "macos")]
    let lib_name = "libatm_ci_provider_azdo.dylib";
    #[cfg(target_os = "linux")]
    let lib_name = "libatm_ci_provider_azdo.so";
    #[cfg(target_os = "windows")]
    let lib_name = "atm_ci_provider_azdo.dll";

    // Since the example has its own [workspace], it builds to its own target directory
    let lib_path = example_dir
        .join("target")
        .join("release")
        .join(lib_name);

    if !lib_path.exists() {
        return Err(format!("Built library not found at {}", lib_path.display()).into());
    }

    Ok(lib_path)
}

#[test]
fn test_build_azure_provider() {
    // This test verifies that the Azure provider stub can be built as a cdylib
    let result = build_azure_provider();
    assert!(
        result.is_ok(),
        "Failed to build Azure provider: {:?}",
        result.err()
    );

    let lib_path = result.unwrap();
    assert!(
        lib_path.exists(),
        "Library should exist at {}",
        lib_path.display()
    );
}

#[test]
fn test_azure_provider_exports_factory() {
    // Build the provider
    let lib_path = build_azure_provider().expect("Failed to build Azure provider");

    // Load the library
    unsafe {
        let lib = libloading::Library::new(&lib_path).expect("Failed to load library");

        // Check that the required symbol exists
        let symbol: Result<libloading::Symbol<unsafe extern "C" fn() -> *mut ()>, _> =
            lib.get(b"atm_create_ci_provider_factory");

        assert!(
            symbol.is_ok(),
            "Library should export atm_create_ci_provider_factory"
        );
    }
}

#[test]
fn test_azure_provider_factory_creates_provider() {
    use atm_daemon::plugins::ci_monitor::CiProviderFactory;

    // Build the provider
    let lib_path = build_azure_provider().expect("Failed to build Azure provider");

    // Load the library and create a provider
    unsafe {
        let lib = libloading::Library::new(&lib_path).expect("Failed to load library");

        // Get the factory function
        let create_factory: libloading::Symbol<unsafe extern "C" fn() -> *mut CiProviderFactory> =
            lib.get(b"atm_create_ci_provider_factory")
                .expect("Failed to find factory function");

        // Call the factory function
        let factory_ptr = create_factory();
        assert!(!factory_ptr.is_null(), "Factory pointer should not be null");

        // Take ownership of the factory
        let factory = Box::from_raw(factory_ptr);

        // Verify factory metadata
        assert_eq!(factory.name, "azure-pipelines");
        assert!(factory.description.contains("Azure"));

        // Create a provider instance
        let provider_result = (factory.create)(None);
        assert!(
            provider_result.is_ok(),
            "Factory should create provider: {:?}",
            provider_result.err()
        );

        let provider = provider_result.unwrap();
        assert_eq!(provider.provider_name(), "Azure Pipelines (stub)");
    }
}

#[tokio::test]
async fn test_azure_provider_list_runs() {
    use atm_daemon::plugins::ci_monitor::{CiFilter, CiProviderFactory};

    // Build the provider
    let lib_path = build_azure_provider().expect("Failed to build Azure provider");

    // Load the library and create a provider
    unsafe {
        let lib = libloading::Library::new(&lib_path).expect("Failed to load library");

        let create_factory: libloading::Symbol<unsafe extern "C" fn() -> *mut CiProviderFactory> =
            lib.get(b"atm_create_ci_provider_factory")
                .expect("Failed to find factory function");

        let factory_ptr = create_factory();
        let factory = Box::from_raw(factory_ptr);

        let provider = (factory.create)(None).expect("Failed to create provider");

        // Call list_runs
        let filter = CiFilter::default();
        let runs = provider.list_runs(&filter).await;

        assert!(runs.is_ok(), "list_runs should succeed: {:?}", runs.err());

        let runs = runs.unwrap();
        assert!(!runs.is_empty(), "Stub provider should return at least one run");

        // Verify stub data
        let run = &runs[0];
        assert_eq!(run.name, "Azure Pipeline");
        assert!(run.url.contains("dev.azure.com"));
    }
}

#[tokio::test]
async fn test_azure_provider_get_run() {
    use atm_daemon::plugins::ci_monitor::CiProviderFactory;

    // Build the provider
    let lib_path = build_azure_provider().expect("Failed to build Azure provider");

    unsafe {
        let lib = libloading::Library::new(&lib_path).expect("Failed to load library");

        let create_factory: libloading::Symbol<unsafe extern "C" fn() -> *mut CiProviderFactory> =
            lib.get(b"atm_create_ci_provider_factory")
                .expect("Failed to find factory function");

        let factory_ptr = create_factory();
        let factory = Box::from_raw(factory_ptr);

        let provider = (factory.create)(None).expect("Failed to create provider");

        // Call get_run
        let run = provider.get_run(1001).await;

        assert!(run.is_ok(), "get_run should succeed: {:?}", run.err());

        let run = run.unwrap();
        assert_eq!(run.id, 1001);
        assert!(run.jobs.is_some(), "Run should have jobs");

        let jobs = run.jobs.unwrap();
        assert!(!jobs.is_empty(), "Run should have at least one job");
        assert_eq!(jobs[0].name, "Build");
    }
}

#[tokio::test]
async fn test_azure_provider_get_job_log() {
    use atm_daemon::plugins::ci_monitor::CiProviderFactory;

    // Build the provider
    let lib_path = build_azure_provider().expect("Failed to build Azure provider");

    unsafe {
        let lib = libloading::Library::new(&lib_path).expect("Failed to load library");

        let create_factory: libloading::Symbol<unsafe extern "C" fn() -> *mut CiProviderFactory> =
            lib.get(b"atm_create_ci_provider_factory")
                .expect("Failed to find factory function");

        let factory_ptr = create_factory();
        let factory = Box::from_raw(factory_ptr);

        let provider = (factory.create)(None).expect("Failed to create provider");

        // Call get_job_log
        let log = provider.get_job_log(2001).await;

        assert!(log.is_ok(), "get_job_log should succeed: {:?}", log.err());

        let log = log.unwrap();
        assert!(log.contains("Azure Pipelines"));
        assert!(log.contains("stub"));
        assert!(log.contains("2001"));
    }
}

#[test]
fn test_azure_provider_with_config() {
    use atm_daemon::plugins::ci_monitor::CiProviderFactory;

    // Build the provider
    let lib_path = build_azure_provider().expect("Failed to build Azure provider");

    unsafe {
        let lib = libloading::Library::new(&lib_path).expect("Failed to load library");

        let create_factory: libloading::Symbol<unsafe extern "C" fn() -> *mut CiProviderFactory> =
            lib.get(b"atm_create_ci_provider_factory")
                .expect("Failed to find factory function");

        let factory_ptr = create_factory();
        let factory = Box::from_raw(factory_ptr);

        // Create config table
        let mut config = toml::Table::new();
        let mut azure_config = toml::Table::new();
        azure_config.insert("organization".to_string(), toml::Value::String("test-org".to_string()));
        azure_config.insert("project".to_string(), toml::Value::String("test-project".to_string()));
        azure_config.insert("repo".to_string(), toml::Value::String("test-repo".to_string()));
        config.insert("azure".to_string(), toml::Value::Table(azure_config));

        // Create provider with config
        let provider = (factory.create)(Some(&config)).expect("Failed to create provider with config");

        assert_eq!(provider.provider_name(), "Azure Pipelines (stub)");
    }
}

#[test]
fn test_provider_library_file_discovery() {
    // Test that we can discover provider libraries in a directory
    let temp_dir = TempDir::new().expect("Failed to create temp dir");
    let providers_dir = temp_dir.path().join("providers");
    std::fs::create_dir_all(&providers_dir).expect("Failed to create providers dir");

    // Build and copy the Azure provider
    let lib_path = build_azure_provider().expect("Failed to build Azure provider");
    let target_path = providers_dir.join(lib_path.file_name().unwrap());
    std::fs::copy(&lib_path, &target_path).expect("Failed to copy library");

    // Verify the file exists
    assert!(target_path.exists());

    // In a real implementation, the ProviderLoader would scan this directory
    // and load all provider libraries it finds
}
