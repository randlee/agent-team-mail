//! Integration test for loading external provider libraries

use atm_daemon::plugins::issues::ProviderLoader;
use std::path::PathBuf;
use std::process::Command;

fn workspace_root() -> PathBuf {
    let manifest_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    manifest_dir
        .parent()
        .and_then(|p| p.parent())
        .expect("Failed to find workspace root")
        .to_path_buf()
}

fn provider_stub_dir() -> PathBuf {
    workspace_root().join("examples").join("provider-stub")
}

fn provider_stub_lib_path() -> PathBuf {
    let dir = provider_stub_dir().join("target").join("release");
    let prefix = if cfg!(windows) { "" } else { "lib" };
    let suffix = std::env::consts::DLL_SUFFIX.trim_start_matches('.');
    let name = format!("{}atm_provider_stub.{}", prefix, suffix);
    dir.join(name)
}

#[test]
fn test_provider_loader_loads_stub_library() {
    let stub_dir = provider_stub_dir();
    assert!(stub_dir.exists(), "provider-stub directory not found");

    // Build the stub provider as a shared library
    let status = Command::new("cargo")
        .arg("build")
        .arg("--release")
        .current_dir(&stub_dir)
        .status()
        .expect("Failed to run cargo build for provider-stub");

    assert!(status.success(), "provider-stub build failed");

    let lib_path = provider_stub_lib_path();
    assert!(
        lib_path.exists(),
        "provider-stub library not found at {:?}",
        lib_path
    );

    let mut loader = ProviderLoader::new();
    let factories = loader.load_libraries(&[lib_path]);
    assert_eq!(factories.len(), 1);

    let factory = &factories[0];
    assert_eq!(factory.name, "stub");

    let provider = (factory.create)(None).expect("Failed to create stub provider");
    assert_eq!(provider.provider_name(), "stub");
}
