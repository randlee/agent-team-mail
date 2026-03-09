use agent_team_mail_daemon::plugins::worker_adapter::codex_tmux::TmuxPayload;
use agent_team_mail_daemon::plugins::worker_adapter::{
    CodexTmuxBackend, WorkerAdapter, WorkerHandle,
};
use serial_test::serial;
use std::fs;
#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::process::{Command, Stdio};
use std::sync::Arc;

fn write_fake_tmux(bin_dir: &Path) {
    let tmux_path = bin_dir.join("tmux");
    let script = r#"#!/bin/sh
set -eu
log_file="${ATM_FAKE_TMUX_LOG:?missing ATM_FAKE_TMUX_LOG}"
printf '%s\n' "$*" >> "$log_file"
if [ "$1" = "display-message" ]; then
  printf '%s\n' "${ATM_FAKE_PANE_PID:?missing ATM_FAKE_PANE_PID}"
  exit 0
fi
if [ "$1" = "send-keys" ]; then
  exit 0
fi
if [ "$1" = "kill-pane" ]; then
  exit 0
fi
if [ "$1" = "-V" ]; then
  printf 'tmux 3.4\n'
  exit 0
fi
exit 0
"#;
    fs::write(&tmux_path, script).expect("write fake tmux");
    #[cfg(unix)]
    {
        let mut perms = fs::metadata(&tmux_path)
            .expect("tmux metadata")
            .permissions();
        perms.set_mode(0o755);
        fs::set_permissions(&tmux_path, perms).expect("chmod fake tmux");
    }
}

#[cfg(unix)]
#[tokio::test]
#[serial]
async fn test_gemini_shutdown_escalates_to_sigkill_after_sigterm() {
    let temp = tempfile::tempdir().expect("temp dir");
    let bin_dir = temp.path().join("bin");
    fs::create_dir_all(&bin_dir).expect("create bin dir");
    write_fake_tmux(&bin_dir);

    let tmux_log = temp.path().join("tmux.log");
    let term_marker = temp.path().join("term.marker");
    fs::write(&tmux_log, "").expect("init tmux log");

    let shell_script = format!(
        "trap 'echo TERM >> {}' TERM; trap '' INT; while true; do sleep 0.2; done",
        term_marker.display()
    );

    let mut child = Command::new("sh")
        .arg("-c")
        .arg(shell_script)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("spawn worker child");

    let old_path = std::env::var("PATH").ok();
    // SAFETY: test-scoped environment mutation, serialized with #[serial].
    unsafe {
        std::env::set_var(
            "PATH",
            format!(
                "{}:{}",
                bin_dir.display(),
                old_path.clone().unwrap_or_default()
            ),
        );
        std::env::set_var("ATM_FAKE_TMUX_LOG", tmux_log.display().to_string());
        std::env::set_var("ATM_FAKE_PANE_PID", child.id().to_string());
        std::env::set_var("ATM_GEMINI_SHUTDOWN_WAIT_SECS", "1");
    }

    let log_dir = temp.path().join("logs");
    fs::create_dir_all(&log_dir).expect("create log dir");
    let mut backend = CodexTmuxBackend::new("atm-test-session".to_string(), log_dir.clone());
    let runtime_home = std::env::temp_dir()
        .join("runtime/gemini/atm-dev/arch-ctm/home")
        .to_string_lossy()
        .into_owned();

    let handle = WorkerHandle {
        agent_id: "arch-ctm".to_string(),
        backend_id: "%42".to_string(),
        log_file_path: log_dir.join("arch-ctm.log"),
        payload: Some(Arc::new(TmuxPayload {
            session: "atm-test-session".to_string(),
            pane_id: "%42".to_string(),
            window_name: "arch-ctm".to_string(),
            runtime: "gemini".to_string(),
            runtime_session_id: Some("gemini-session-123".to_string()),
            runtime_home: Some(runtime_home),
        })),
    };

    backend
        .shutdown(&handle)
        .await
        .expect("gemini shutdown succeeds");

    let status = child.wait().expect("wait child");
    assert!(
        !status.success(),
        "expected forced termination after SIGTERM trap, got: {status:?}"
    );

    let marker = fs::read_to_string(&term_marker).expect("read TERM marker");
    assert!(
        marker.contains("TERM"),
        "SIGTERM should be delivered before SIGKILL"
    );

    let tmux_trace = fs::read_to_string(&tmux_log).expect("read tmux log");
    assert!(tmux_trace.contains("display-message -t %42 -p #{pane_pid}"));
    assert!(tmux_trace.contains("send-keys -t %42 C-c"));
    assert!(tmux_trace.contains("kill-pane -t %42"));

    // SAFETY: restore env after test; serialized with #[serial].
    unsafe {
        match old_path {
            Some(path) => std::env::set_var("PATH", path),
            None => std::env::remove_var("PATH"),
        }
        std::env::remove_var("ATM_FAKE_TMUX_LOG");
        std::env::remove_var("ATM_FAKE_PANE_PID");
        std::env::remove_var("ATM_GEMINI_SHUTDOWN_WAIT_SECS");
    }
}
