#![allow(dead_code)]

//! Shared GitHub CLI response schemas used by the CI monitor subsystem.

use serde::Deserialize;

/// GitHub run JSON schema (from `gh run list --json` and `gh run view --json`).
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct GhRun {
    pub(crate) database_id: u64,
    pub(crate) name: String,
    pub(crate) status: String,
    pub(crate) conclusion: Option<String>,
    pub(crate) head_branch: String,
    pub(crate) head_sha: String,
    pub(crate) url: String,
    pub(crate) created_at: String,
    pub(crate) updated_at: String,
    pub(crate) jobs: Option<Vec<GhJob>>,
}

/// GitHub job JSON schema.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct GhJob {
    pub(crate) database_id: u64,
    pub(crate) name: String,
    pub(crate) status: String,
    pub(crate) conclusion: Option<String>,
    pub(crate) started_at: Option<String>,
    pub(crate) completed_at: Option<String>,
    pub(crate) steps: Option<Vec<GhStep>>,
}

/// GitHub step JSON schema.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct GhStep {
    pub(crate) name: String,
    pub(crate) status: String,
    pub(crate) conclusion: Option<String>,
    pub(crate) number: u64,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct GhRunView {
    pub(crate) database_id: u64,
    pub(crate) name: String,
    pub(crate) status: String,
    #[serde(default)]
    pub(crate) conclusion: Option<String>,
    pub(crate) head_branch: String,
    pub(crate) head_sha: String,
    pub(crate) url: String,
    #[serde(default)]
    pub(crate) jobs: Vec<GhRunJob>,
    #[serde(default)]
    pub(crate) attempt: Option<u64>,
    #[serde(default)]
    pub(crate) pull_requests: Vec<GhPullRequest>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct GhRunJob {
    pub(crate) database_id: u64,
    pub(crate) name: String,
    pub(crate) status: String,
    #[serde(default)]
    pub(crate) conclusion: Option<String>,
    #[serde(default)]
    pub(crate) started_at: Option<String>,
    #[serde(default)]
    pub(crate) completed_at: Option<String>,
    #[serde(default)]
    pub(crate) steps: Vec<GhRunStep>,
    #[serde(default)]
    pub(crate) url: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct GhRunStep {
    pub(crate) name: String,
    #[serde(default)]
    pub(crate) status: Option<String>,
    #[serde(default)]
    pub(crate) conclusion: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct GhPullRequest {
    #[serde(default)]
    pub(crate) url: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct GhPrLookupView {
    #[serde(default)]
    pub(crate) head_ref_name: Option<String>,
    #[serde(default)]
    pub(crate) head_ref_oid: Option<String>,
    #[serde(default)]
    pub(crate) created_at: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct GhPrView {
    #[serde(default)]
    pub(crate) merge_state_status: Option<String>,
    #[serde(default)]
    pub(crate) url: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct GhRunListEntry {
    #[serde(default)]
    pub(crate) database_id: Option<u64>,
    #[serde(default)]
    pub(crate) head_sha: Option<String>,
    #[serde(default)]
    pub(crate) created_at: Option<String>,
}
