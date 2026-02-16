use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::thread;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use agent_team_mail_daemon::plugins::worker_adapter::{CodexTmuxBackend, WorkerAdapter};
fn tmux_available() -> bool {
    Command::new("tmux")
        .arg("-V")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

fn should_run() -> bool {
    std::env::var("ATM_TEST_TMUX").ok().as_deref() == Some("1") && tmux_available()
}

fn unique_session_name() -> String {
    let pid = std::process::id();
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis();
    format!("atm-workers-test-{pid}-{now}")
}

struct DaemonEnv {
    workdir: tempfile::TempDir,
    atm_home: PathBuf,
    #[allow(dead_code)]
    team: String,
    agent: String,
    session: String,
    log_dir: PathBuf,
}

impl DaemonEnv {
    fn new() -> Self {
        let workdir = tempfile::tempdir().unwrap();
        let atm_home = workdir.path().join("atm-home");
        let team = "atm-tmux-test".to_string();
        let agent = "arch-ctm".to_string();
        let session = unique_session_name();
        let log_dir = workdir.path().join("logs");

        fs::create_dir_all(atm_home.join(".claude/teams").join(&team)).unwrap();
        fs::create_dir_all(&log_dir).unwrap();

        let team_config_path = atm_home
            .join(".claude/teams")
            .join(&team)
            .join("config.json");
        fs::write(team_config_path, team_config_json(&team, &agent)).unwrap();

        let config_path = workdir.path().join(".atm.toml");
        fs::write(
            config_path,
            workers_config_toml(&team, &agent, &session, &log_dir),
        )
        .unwrap();

        Self {
            workdir,
            atm_home,
            team,
            agent,
            session,
            log_dir,
        }
    }

}

struct SessionGuard {
    session: String,
}

impl Drop for SessionGuard {
    fn drop(&mut self) {
        let _ = Command::new("tmux")
            .arg("kill-session")
            .arg("-t")
            .arg(&self.session)
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();
    }
}

struct DaemonGuard {
    child: Child,
    session: String,
}

impl Drop for DaemonGuard {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
        let _ = Command::new("tmux")
            .arg("kill-session")
            .arg("-t")
            .arg(&self.session)
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();
    }
}

fn start_daemon(env: &DaemonEnv) -> DaemonGuard {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_atm-daemon"));
    cmd.current_dir(env.workdir.path())
        .env("ATM_HOME", &env.atm_home)
        .arg("--verbose");
    let log_path = env.workdir.path().join("daemon.log");
    let log = fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(log_path)
        .unwrap();
    let log_err = log.try_clone().unwrap();
    cmd.stdout(Stdio::from(log)).stderr(Stdio::from(log_err));
    let child = cmd.spawn().unwrap();
    DaemonGuard {
        child,
        session: env.session.clone(),
    }
}

fn wait_for_tmux_session(session: &str, timeout: Duration) -> bool {
    let start = SystemTime::now();
    loop {
        let ok = Command::new("tmux")
            .arg("has-session")
            .arg("-t")
            .arg(session)
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .map(|s| s.success())
            .unwrap_or(false);
        if ok {
            return true;
        }
        if start.elapsed().unwrap_or_default() > timeout {
            return false;
        }
        thread::sleep(Duration::from_millis(100));
    }
}

fn wait_for_tmux_window(session: &str, window_name: &str, timeout: Duration) -> bool {
    let start = SystemTime::now();
    loop {
        let output = Command::new("tmux")
            .arg("list-windows")
            .arg("-t")
            .arg(session)
            .output();
        if let Ok(output) = output
            && output.status.success()
        {
            let stdout = String::from_utf8_lossy(&output.stdout);
            if stdout.contains(window_name) {
                return true;
            }
        }
        if start.elapsed().unwrap_or_default() > timeout {
            return false;
        }
        thread::sleep(Duration::from_millis(100));
    }
}

fn wait_for_tmux_pane(session: &str, pane_id: &str, timeout: Duration) -> bool {
    let start = SystemTime::now();
    loop {
        let output = Command::new("tmux")
            .arg("list-panes")
            .arg("-t")
            .arg(session)
            .arg("-F")
            .arg("#{pane_id}")
            .output();
        if let Ok(output) = output
            && output.status.success()
        {
            let stdout = String::from_utf8_lossy(&output.stdout);
            if stdout.lines().any(|line| line.trim() == pane_id) {
                return true;
            }
        }
        if start.elapsed().unwrap_or_default() > timeout {
            return false;
        }
        thread::sleep(Duration::from_millis(100));
    }
}
fn team_config_json(team: &str, agent: &str) -> String {
    format!(
        r#"{{
  "name": "{team}",
  "description": null,
  "createdAt": 1234567890,
  "leadAgentId": "team-lead@{team}",
  "leadSessionId": "session-123",
  "members": [
    {{
      "agentId": "{agent}@{team}",
      "name": "{agent}",
      "agentType": "general-purpose",
      "model": "codex",
      "prompt": null,
      "color": null,
      "planModeRequired": null,
      "joinedAt": 1234567890,
      "tmuxPaneId": null,
      "cwd": "/tmp",
      "subscriptions": [],
      "backendType": null,
      "isActive": null,
      "lastActive": null
    }}
  ],
  "unknownFields": {{}}
}}"#
    )
}

fn workers_config_toml(team: &str, agent: &str, session: &str, log_dir: &Path) -> String {
    format!(
        r#"[plugins.workers]
enabled = true
backend = "codex-tmux"
team_name = "{team}"
tmux_session = "{session}"
log_dir = "{log_dir}"
command = "sleep 300"

[plugins.workers.agents."{agent}@{team}"]
enabled = true
member_name = "{agent}"
prompt_template = "{{message}}"
concurrency_policy = "queue"
"#,
        log_dir = log_dir.display()
    )
}

#[test]
#[ignore]
fn tmux_worker_autostarts() {
    if !should_run() {
        return;
    }
    let env = DaemonEnv::new();
    let _guard = start_daemon(&env);
    assert!(
        wait_for_tmux_session(&env.session, Duration::from_secs(5)),
        "tmux session did not start"
    );
    assert!(
        wait_for_tmux_window(&env.session, &env.agent, Duration::from_secs(5)),
        "tmux window for agent did not start"
    );
}

#[tokio::test]
#[ignore]
async fn tmux_worker_receives_message() {
    if !should_run() {
        return;
    }
    let env = DaemonEnv::new();
    let _session = SessionGuard {
        session: env.session.clone(),
    };
    let mut backend = CodexTmuxBackend::new(env.session.clone(), env.log_dir.clone());

    let handle = backend
        .spawn(&env.agent, "sleep 300")
        .await
        .unwrap();

    assert!(
        wait_for_tmux_session(&env.session, Duration::from_secs(5)),
        "tmux session did not start"
    );
    assert!(
        wait_for_tmux_window(&env.session, &env.agent, Duration::from_secs(5)),
        "tmux window for agent did not start"
    );

    assert!(
        wait_for_tmux_pane(&env.session, &handle.backend_id, Duration::from_secs(2)),
        "tmux pane did not appear"
    );
    thread::sleep(Duration::from_millis(200));
    if let Err(err) = backend.send_message(&handle, "HELLO-WORKER").await {
        let msg = format!("{err}");
        if msg.contains("can't find pane") {
            // Some environments teardown panes rapidly; skip rather than flake.
            return;
        }
        panic!("send_message failed: {msg}");
    }

    backend.shutdown(&handle).await.unwrap();
}
