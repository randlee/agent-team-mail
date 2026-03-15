#![allow(dead_code)]

use super::provider::ErasedCiProvider;
use super::types::{CiFilter, CiProviderError, CiPullRequest, CiRun};
use crate::plugin::PluginError;

fn provider_error_to_plugin_error(err: CiProviderError) -> PluginError {
    PluginError::Provider {
        message: err.to_string(),
        source: None,
    }
}

#[cfg(unix)]
pub(crate) async fn fetch_run(
    provider: &dyn ErasedCiProvider,
    run_id: u64,
) -> Result<CiRun, PluginError> {
    provider
        .get_run(run_id)
        .await
        .map_err(provider_error_to_plugin_error)
}

#[cfg(unix)]
pub(crate) async fn fetch_pull_request(
    provider: &dyn ErasedCiProvider,
    pr_number: u64,
) -> Result<Option<CiPullRequest>, PluginError> {
    provider
        .get_pull_request(pr_number)
        .await
        .map_err(provider_error_to_plugin_error)
}

#[cfg(unix)]
pub(crate) async fn try_find_pr_run_id(
    provider: &dyn ErasedCiProvider,
    pr_number: u64,
) -> Result<Option<u64>, PluginError> {
    let Some(pr_view) = fetch_pull_request(provider, pr_number).await? else {
        return Ok(None);
    };
    let Some(branch) = pr_view
        .head_ref_name
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
    else {
        return Ok(None);
    };

    let runs = provider
        .list_runs(&CiFilter {
            branch: Some(branch),
            per_page: Some(20),
            ..Default::default()
        })
        .await
        .map_err(provider_error_to_plugin_error)?;

    for run in runs {
        if let Some(expected_head_sha) = pr_view
            .head_ref_oid
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty())
            && run.head_sha.trim() != expected_head_sha
        {
            continue;
        }

        if !run_passes_pr_recency_gate(Some(run.created_at.as_str()), pr_view.created_at.as_deref())
        {
            continue;
        }

        return Ok(Some(run.id));
    }

    Ok(None)
}

#[cfg(unix)]
pub(crate) async fn try_find_workflow_run_id(
    provider: &dyn ErasedCiProvider,
    workflow: &str,
    reference: &str,
) -> Result<Option<u64>, PluginError> {
    let runs = provider
        .list_runs(&CiFilter {
            branch: Some(reference.to_string()),
            per_page: Some(20),
            ..Default::default()
        })
        .await
        .map_err(provider_error_to_plugin_error)?;

    Ok(runs.into_iter().find_map(|run| {
        let matches_workflow = run.name.is_empty() || run.name == workflow;
        let matches_ref = run.head_branch == reference || run.head_sha.starts_with(reference);
        if matches_workflow && matches_ref {
            Some(run.id)
        } else {
            None
        }
    }))
}

#[cfg(unix)]
pub(crate) async fn fetch_failed_log_excerpt(
    provider: &dyn ErasedCiProvider,
    job_id: u64,
) -> Result<String, PluginError> {
    let output = provider
        .get_job_log(job_id)
        .await
        .map_err(provider_error_to_plugin_error)?;
    Ok(output
        .lines()
        .filter(|line| !line.trim().is_empty())
        .take(8)
        .collect::<Vec<_>>()
        .join(" | "))
}

#[cfg(unix)]
pub(crate) fn run_passes_pr_recency_gate(
    run_created_at: Option<&str>,
    pr_created_at: Option<&str>,
) -> bool {
    let Some(pr_created_at) = pr_created_at else {
        return true;
    };
    let Some(run_created_at) = run_created_at else {
        return true;
    };

    let parse_ts = |s: &str| chrono::DateTime::parse_from_rfc3339(s).ok();
    let Some(pr_ts) = parse_ts(pr_created_at) else {
        return true;
    };
    let Some(run_ts) = parse_ts(run_created_at) else {
        return true;
    };

    run_ts >= pr_ts
}

pub(crate) fn is_pr_merge_state_dirty(merge_state_status: &str) -> bool {
    merge_state_status.trim().eq_ignore_ascii_case("dirty")
}
