use crate::types::GhRepoStateFile;
use std::collections::HashMap;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

pub fn gh_repo_state_path_for(home: &Path) -> PathBuf {
    home.join(".atm/daemon/gh-monitor-repo-state.json")
}

pub fn load_repo_state(home: &Path) -> io::Result<GhRepoStateFile> {
    let path = gh_repo_state_path_for(home);
    if !path.exists() {
        return Ok(GhRepoStateFile::default());
    }
    let raw = fs::read_to_string(path)?;
    serde_json::from_str(&raw).map_err(io::Error::other)
}

pub fn write_repo_state(home: &Path, state: &GhRepoStateFile) -> io::Result<()> {
    let path = gh_repo_state_path_for(home);
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let tmp = path.with_extension("json.tmp");
    let bytes = serde_json::to_vec_pretty(state).map_err(io::Error::other)?;
    fs::write(&tmp, bytes)?;
    fs::rename(tmp, path)?;
    Ok(())
}

pub fn repo_state_map(home: &Path) -> io::Result<HashMap<String, crate::types::GhRepoStateRecord>> {
    let state = load_repo_state(home)?;
    Ok(state
        .records
        .into_iter()
        .map(|record| (repo_state_key(&record.team, &record.repo), record))
        .collect())
}

pub fn repo_state_key(team: &str, repo: &str) -> String {
    format!("{}|{}", team.trim(), repo.trim().to_ascii_lowercase())
}
