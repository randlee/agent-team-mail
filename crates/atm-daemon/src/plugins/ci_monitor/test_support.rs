use std::path::Path;

use agent_team_mail_core::schema::InboxMessage;
use tempfile::TempDir;

pub(crate) fn write_gh_monitor_config(home: &Path, team: &str) {
    let cfg_dir = home.join(".config/atm");
    std::fs::create_dir_all(&cfg_dir).unwrap();
    let config = format!(
        r#"[core]
default_team = "{team}"
identity = "daemon-test"

[plugins.gh_monitor]
enabled = true
team = "{team}"
agent = "gh-monitor"
repo = "o/r"
poll_interval_secs = 60
"#
    );
    std::fs::write(cfg_dir.join("config.toml"), config).unwrap();
}

pub(crate) fn write_repo_gh_monitor_config(repo_dir: &Path, team: &str) {
    std::fs::create_dir_all(repo_dir).unwrap();
    let config = format!(
        r#"[core]
default_team = "{team}"
identity = "daemon-test"

[plugins.gh_monitor]
enabled = true
team = "{team}"
agent = "gh-monitor"
repo = "o/r"
poll_interval_secs = 60
"#
    );
    std::fs::write(repo_dir.join(".atm.toml"), config).unwrap();
}

pub(crate) fn write_invalid_gh_monitor_config(home: &Path, team: &str) {
    let cfg_dir = home.join(".config/atm");
    std::fs::create_dir_all(&cfg_dir).unwrap();
    let config = format!(
        r#"[core]
default_team = "{team}"
identity = "daemon-test"

[plugins.gh_monitor]
enabled = true
team = "{team}"
agent = "gh-monitor"
poll_interval_secs = 1
"#
    );
    std::fs::write(cfg_dir.join("config.toml"), config).unwrap();
}

pub(crate) struct EnvGuard {
    key: &'static str,
    previous: Option<String>,
}

impl EnvGuard {
    pub(crate) fn set(key: &'static str, value: &str) -> Self {
        let previous = std::env::var(key).ok();
        // SAFETY: test-only env mutation, guarded by #[serial] on callers.
        unsafe {
            std::env::set_var(key, value);
        }
        Self { key, previous }
    }
}

impl Drop for EnvGuard {
    fn drop(&mut self) {
        // SAFETY: test-only env mutation, guarded by #[serial] on callers.
        unsafe {
            match &self.previous {
                Some(v) => std::env::set_var(self.key, v),
                None => std::env::remove_var(self.key),
            }
        }
    }
}

pub(crate) fn write_hook_auth_team_config(
    home_dir: &Path,
    team: &str,
    lead: &str,
    members: &[&str],
) {
    let team_dir = home_dir.join(".claude/teams").join(team);
    std::fs::create_dir_all(&team_dir).unwrap();
    let mut member_values = Vec::new();
    for m in members {
        member_values.push(serde_json::json!({
            "agentId": format!("{m}@{team}"),
            "name": m,
            "agentType": "general-purpose",
            "model": "unknown",
            "joinedAt": 1739284800000u64,
            "cwd": home_dir.to_string_lossy().to_string(),
            "subscriptions": []
        }));
    }
    let config = serde_json::json!({
        "name": team,
        "description": "test team",
        "createdAt": 1739284800000u64,
        "leadAgentId": format!("{lead}@{team}"),
        "leadSessionId": "test-lead-session",
        "members": member_values,
    });
    {
        use std::io::Write;
        let config_path = team_dir.join("config.json");
        let config_bytes = serde_json::to_string_pretty(&config).unwrap();
        let file = std::fs::File::create(&config_path).unwrap();
        let mut writer = std::io::BufWriter::new(&file);
        writer.write_all(config_bytes.as_bytes()).unwrap();
        writer.flush().unwrap();
        file.sync_all().unwrap();
    }
}

#[cfg(unix)]
pub(crate) fn read_team_inbox_messages(
    home_dir: &Path,
    team: &str,
    agent: &str,
) -> Vec<InboxMessage> {
    let path = home_dir
        .join(".claude/teams")
        .join(team)
        .join("inboxes")
        .join(format!("{agent}.json"));
    if !path.exists() {
        return Vec::new();
    }
    serde_json::from_str::<Vec<InboxMessage>>(&std::fs::read_to_string(path).unwrap())
        .unwrap_or_default()
}

#[cfg(unix)]
pub(crate) fn install_fake_gh_script(temp: &TempDir, script_body: &str) -> EnvGuard {
    use std::os::unix::fs::PermissionsExt;

    let script_path = temp.path().join("gh");
    let body = script_body
        .strip_prefix("#!/bin/sh\n")
        .unwrap_or(script_body);
    let wrapped = format!(
        r#"#!/bin/sh
if [ "$1" = "-R" ]; then
  shift
  if [ -n "$1" ]; then
    shift
  fi
fi
{body}
"#
    );
    std::fs::write(&script_path, wrapped).expect("write fake gh script");
    let mut perms = std::fs::metadata(&script_path)
        .expect("stat fake gh script")
        .permissions();
    perms.set_mode(0o755);
    std::fs::set_permissions(&script_path, perms).expect("chmod fake gh script");

    let previous_path = std::env::var("PATH").unwrap_or_default();
    let composed = if previous_path.is_empty() {
        temp.path().display().to_string()
    } else {
        format!("{}:{previous_path}", temp.path().display())
    };
    EnvGuard::set("PATH", &composed)
}
