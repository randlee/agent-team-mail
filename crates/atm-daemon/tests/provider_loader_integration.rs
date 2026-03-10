//! Integration test for loading external provider libraries

use agent_team_mail_daemon::plugins::issues::ProviderLoader;
use std::path::PathBuf;
use std::process::{Child, Command};
use std::thread;
use std::time::{Duration, Instant};

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
    let dir = provider_stub_dir().join("target").join("debug");
    let prefix = if cfg!(windows) { "" } else { "lib" };
    let suffix = std::env::consts::DLL_SUFFIX.trim_start_matches('.');
    let name = format!("{prefix}atm_provider_stub.{suffix}");
    dir.join(name)
}

fn wait_for_child_with_timeout(
    child: &mut Child,
    timeout: Duration,
    step: Duration,
) -> std::process::ExitStatus {
    let deadline = Instant::now() + timeout;
    loop {
        if let Some(status) = child
            .try_wait()
            .expect("Failed to poll cargo build for provider-stub")
        {
            return status;
        }
        if Instant::now() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            panic!(
                "provider-stub cargo build exceeded timeout of {}s",
                timeout.as_secs()
            );
        }
        thread::sleep(step);
    }
}

#[test]
fn test_provider_loader_loads_stub_library() {
    let stub_dir = provider_stub_dir();
    assert!(stub_dir.exists(), "provider-stub directory not found");

    // Build the stub provider as a shared library from the worktree-relative path.
    // Bound the subprocess so the required CI test cannot hang indefinitely.
    let mut child = Command::new("cargo")
        .arg("build")
        .current_dir(&stub_dir)
        .spawn()
        .expect("Failed to run cargo build for provider-stub");
    let status = wait_for_child_with_timeout(
        &mut child,
        Duration::from_secs(60),
        Duration::from_millis(100),
    );

    assert!(status.success(), "provider-stub build failed");

    let lib_path = provider_stub_lib_path();
    assert!(
        lib_path.exists(),
        "provider-stub library not found at {lib_path:?}"
    );

    let mut loader = ProviderLoader::new();
    let factories = loader.load_libraries(&[lib_path]);
    assert_eq!(factories.len(), 1);

    let factory = &factories[0];
    assert_eq!(factory.name, "stub");

    let provider = (factory.create)(None).expect("Failed to create stub provider");
    assert_eq!(provider.provider_name(), "stub");
}
