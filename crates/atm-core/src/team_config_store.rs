use crate::io::{
    atomic::atomic_swap,
    lock::{FileLock, acquire_lock},
};
use crate::schema::TeamConfig;
use anyhow::{Context, Result, anyhow};
use std::io::Write;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone)]
pub enum UpdateOutcome {
    Updated(TeamConfig),
    Unchanged(TeamConfig),
}

#[derive(Debug, Clone)]
pub struct TeamConfigStore {
    config_path: PathBuf,
    lock_path: PathBuf,
}

impl TeamConfigStore {
    pub fn open(team_dir: &Path) -> Self {
        let config_path = team_dir.join("config.json");
        let lock_path = config_path.with_extension("lock");
        Self {
            config_path,
            lock_path,
        }
    }

    pub fn read(&self) -> Result<TeamConfig> {
        let _lock = self.acquire_store_lock()?;
        self.read_from_disk()
    }

    pub fn update<F>(&self, f: F) -> Result<UpdateOutcome>
    where
        F: FnOnce(TeamConfig) -> Result<Option<TeamConfig>>,
    {
        let _lock = self.acquire_store_lock()?;
        let current = self.read_from_disk()?;
        self.apply_update_locked(current, f)
    }

    pub fn create_or_update<F, D>(&self, default_fn: D, f: F) -> Result<UpdateOutcome>
    where
        D: FnOnce() -> TeamConfig,
        F: FnOnce(TeamConfig) -> Result<Option<TeamConfig>>,
    {
        let _lock = self.acquire_store_lock()?;
        let current = match self.read_from_disk() {
            Ok(config) => config,
            Err(err) if self.is_missing_config(&err) => default_fn(),
            Err(err) => return Err(err),
        };
        self.apply_update_locked(current, f)
    }

    /// Async wrapper for `update`. Runs the blocking I/O on a `spawn_blocking` thread. No async lock may be held across this await boundary.
    pub async fn update_async<F>(&self, f: F) -> Result<UpdateOutcome>
    where
        F: FnOnce(TeamConfig) -> Result<Option<TeamConfig>> + Send + 'static,
    {
        let store = self.clone();
        tokio::task::spawn_blocking(move || store.update(f))
            .await
            .context("TeamConfigStore::update_async join failure")?
    }

    /// Async wrapper for `create_or_update`. Runs the blocking I/O on a `spawn_blocking` thread. No async lock may be held across this await boundary.
    pub async fn create_or_update_async<F, D>(&self, default_fn: D, f: F) -> Result<UpdateOutcome>
    where
        D: FnOnce() -> TeamConfig + Send + 'static,
        F: FnOnce(TeamConfig) -> Result<Option<TeamConfig>> + Send + 'static,
    {
        let store = self.clone();
        tokio::task::spawn_blocking(move || store.create_or_update(default_fn, f))
            .await
            .context("TeamConfigStore::create_or_update_async join failure")?
    }

    fn apply_update_locked<F>(&self, current: TeamConfig, f: F) -> Result<UpdateOutcome>
    where
        F: FnOnce(TeamConfig) -> Result<Option<TeamConfig>>,
    {
        match f(current.clone())? {
            Some(updated) => {
                self.write_locked(&updated)?;
                Ok(UpdateOutcome::Updated(updated))
            }
            None => Ok(UpdateOutcome::Unchanged(current)),
        }
    }

    fn read_from_disk(&self) -> Result<TeamConfig> {
        let content = std::fs::read(&self.config_path)
            .with_context(|| format!("failed to read {}", self.config_path.display()))?;
        serde_json::from_slice(&content)
            .with_context(|| format!("failed to parse {}", self.config_path.display()))
    }

    fn acquire_store_lock(&self) -> Result<FileLock> {
        if let Some(parent) = self.lock_path.parent() {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("failed to create {}", parent.display()))?;
        }
        acquire_lock(&self.lock_path, 10).map_err(|e| {
            anyhow!(
                "failed to acquire config lock {}: {e}",
                self.lock_path.display()
            )
        })
    }

    fn write_locked(&self, config: &TeamConfig) -> Result<()> {
        if let Some(parent) = self.config_path.parent() {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("failed to create {}", parent.display()))?;
        }

        let tmp_path = self.config_path.with_extension("tmp");
        let serialized =
            serde_json::to_string_pretty(config).context("failed to serialize TeamConfig")?;
        let mut tmp = std::fs::File::create(&tmp_path)
            .with_context(|| format!("failed to create {}", tmp_path.display()))?;
        tmp.write_all(serialized.as_bytes())
            .with_context(|| format!("failed to write {}", tmp_path.display()))?;
        tmp.sync_all()
            .with_context(|| format!("failed to sync {}", tmp_path.display()))?;
        drop(tmp);

        if !self.config_path.exists() {
            let placeholder = std::fs::File::create(&self.config_path)
                .with_context(|| format!("failed to create {}", self.config_path.display()))?;
            placeholder
                .sync_all()
                .with_context(|| format!("failed to sync {}", self.config_path.display()))?;
        }

        atomic_swap(&self.config_path, &tmp_path).map_err(|e| {
            anyhow!(
                "failed to atomically swap {}: {e}",
                self.config_path.display()
            )
        })?;

        if tmp_path.exists() {
            std::fs::remove_file(&tmp_path)
                .with_context(|| format!("failed to remove {}", tmp_path.display()))?;
        }

        Ok(())
    }

    fn is_missing_config(&self, err: &anyhow::Error) -> bool {
        err.downcast_ref::<std::io::Error>()
            .is_some_and(|io| io.kind() == std::io::ErrorKind::NotFound)
            || err
                .chain()
                .filter_map(|cause| cause.downcast_ref::<std::io::Error>())
                .any(|io| io.kind() == std::io::ErrorKind::NotFound)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::schema::AgentMember;
    use std::collections::HashMap;
    use std::sync::{Arc, Barrier};
    use tempfile::TempDir;

    fn test_config(team: &str, members: &[&str]) -> TeamConfig {
        TeamConfig {
            name: team.to_string(),
            description: Some("test".to_string()),
            created_at: 1,
            lead_agent_id: format!("team-lead@{team}"),
            lead_session_id: String::new(),
            members: members
                .iter()
                .map(|name| AgentMember {
                    agent_id: format!("{name}@{team}"),
                    name: (*name).to_string(),
                    agent_type: "general-purpose".to_string(),
                    model: "test".to_string(),
                    prompt: None,
                    color: None,
                    plan_mode_required: None,
                    joined_at: 1,
                    tmux_pane_id: None,
                    cwd: ".".to_string(),
                    subscriptions: Vec::new(),
                    backend_type: None,
                    is_active: Some(false),
                    last_active: None,
                    session_id: None,
                    external_backend_type: None,
                    external_model: None,
                    unknown_fields: HashMap::new(),
                })
                .collect(),
            unknown_fields: HashMap::new(),
        }
    }

    fn write_config(path: &Path, config: &TeamConfig) {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, serde_json::to_string_pretty(config).unwrap()).unwrap();
    }

    #[test]
    fn update_none_returns_unchanged_without_touching_disk() {
        let temp = TempDir::new().unwrap();
        let team_dir = temp.path().join("atm-dev");
        let config_path = team_dir.join("config.json");
        let config = test_config("atm-dev", &["team-lead"]);
        write_config(&config_path, &config);
        let before = std::fs::read_to_string(&config_path).unwrap();

        let store = TeamConfigStore::open(&team_dir);
        let outcome = store.update(|_| Ok(None)).unwrap();

        match outcome {
            UpdateOutcome::Unchanged(saved) => {
                assert_eq!(saved.name, config.name);
                assert_eq!(saved.members.len(), config.members.len());
            }
            UpdateOutcome::Updated(_) => panic!("expected unchanged outcome"),
        }
        assert_eq!(std::fs::read_to_string(&config_path).unwrap(), before);
    }

    #[test]
    fn create_or_update_creates_missing_file() {
        let temp = TempDir::new().unwrap();
        let team_dir = temp.path().join("atm-dev");
        let store = TeamConfigStore::open(&team_dir);

        let outcome = store
            .create_or_update(
                || test_config("atm-dev", &["team-lead"]),
                |mut config| {
                    config.description = Some("created".to_string());
                    Ok(Some(config))
                },
            )
            .unwrap();

        let saved = store.read().unwrap();
        assert!(matches!(outcome, UpdateOutcome::Updated(_)));
        assert_eq!(saved.description.as_deref(), Some("created"));
    }

    #[test]
    fn concurrent_updates_preserve_both_mutations() {
        let temp = TempDir::new().unwrap();
        let team_dir = temp.path().join("atm-dev");
        let config_path = team_dir.join("config.json");
        write_config(&config_path, &test_config("atm-dev", &["team-lead"]));

        let store = Arc::new(TeamConfigStore::open(&team_dir));
        let barrier = Arc::new(Barrier::new(3));
        let handles: Vec<_> = ["arch-ctm", "quality-mgr"]
            .into_iter()
            .map(|name| {
                let store = Arc::clone(&store);
                let barrier = Arc::clone(&barrier);
                std::thread::spawn(move || {
                    barrier.wait();
                    store
                        .update(|mut config| {
                            if !config.members.iter().any(|m| m.name == name) {
                                let mut single = test_config("atm-dev", &[name]);
                                config.members.push(single.members.remove(0));
                            }
                            Ok(Some(config))
                        })
                        .unwrap();
                })
            })
            .collect();

        barrier.wait();
        for handle in handles {
            handle.join().unwrap();
        }

        let saved = store.read().unwrap();
        assert!(saved.members.iter().any(|m| m.name == "arch-ctm"));
        assert!(saved.members.iter().any(|m| m.name == "quality-mgr"));
    }

    #[test]
    fn leftover_tmp_does_not_corrupt_config() {
        let temp = TempDir::new().unwrap();
        let team_dir = temp.path().join("atm-dev");
        let config_path = team_dir.join("config.json");
        let config = test_config("atm-dev", &["team-lead"]);
        write_config(&config_path, &config);
        std::fs::write(
            config_path.with_extension("tmp"),
            serde_json::to_string_pretty(&test_config("atm-dev", &["corrupt"])).unwrap(),
        )
        .unwrap();

        let store = TeamConfigStore::open(&team_dir);
        let read_back = store.read().unwrap();

        assert_eq!(read_back.name, config.name);
        assert_eq!(read_back.members.len(), config.members.len());
    }

    #[tokio::test]
    async fn update_async_persists_changes() {
        let temp = TempDir::new().unwrap();
        let team_dir = temp.path().join("atm-dev");
        let config_path = team_dir.join("config.json");
        write_config(&config_path, &test_config("atm-dev", &["team-lead"]));

        let store = TeamConfigStore::open(&team_dir);
        let outcome = store
            .update_async(|mut config| {
                config.description = Some("updated async".to_string());
                Ok(Some(config))
            })
            .await
            .unwrap();

        assert!(matches!(outcome, UpdateOutcome::Updated(_)));
        let saved = store.read().unwrap();
        assert_eq!(saved.description.as_deref(), Some("updated async"));
    }

    #[tokio::test]
    async fn create_or_update_async_creates_missing_file() {
        let temp = TempDir::new().unwrap();
        let team_dir = temp.path().join("atm-dev");
        let store = TeamConfigStore::open(&team_dir);

        let outcome = store
            .create_or_update_async(
                || test_config("atm-dev", &["team-lead"]),
                |mut config| {
                    config.description = Some("created async".to_string());
                    Ok(Some(config))
                },
            )
            .await
            .unwrap();

        assert!(matches!(outcome, UpdateOutcome::Updated(_)));
        let saved = store.read().unwrap();
        assert_eq!(saved.description.as_deref(), Some("created async"));
    }
}
