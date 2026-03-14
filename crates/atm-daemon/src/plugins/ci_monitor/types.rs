pub use agent_team_mail_ci_monitor::{
    CiFilter, CiJob, CiMonitorControlRequest, CiMonitorHealth, CiMonitorLifecycleAction,
    CiMonitorRequest, CiMonitorStatus, CiMonitorStatusRequest, CiMonitorTargetKind,
    CiProviderError, CiPullRequest, CiRun, CiRunConclusion, CiRunStatus, CiStep,
};

use serde::{Deserialize, Serialize};

#[cfg(unix)]
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub(crate) struct GhMonitorStateFile {
    pub(crate) records: Vec<CiMonitorStatus>,
}

#[cfg(unix)]
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub(crate) struct GhMonitorHealthFile {
    pub(crate) records: Vec<CiMonitorHealth>,
}

#[cfg(unix)]
#[derive(Debug, Clone, Default)]
pub(crate) struct GhMonitorHealthUpdate<'a> {
    pub(crate) lifecycle_state: Option<&'a str>,
    pub(crate) availability_state: Option<&'a str>,
    pub(crate) in_flight: Option<u64>,
    pub(crate) message: Option<String>,
    pub(crate) config_state: Option<&'a GhMonitorConfigState>,
    pub(crate) config_cwd: Option<&'a str>,
}

#[cfg(unix)]
#[derive(Debug, Clone)]
pub(crate) struct GhMonitorConfigState {
    pub(crate) configured: bool,
    pub(crate) enabled: bool,
    pub(crate) config_source: Option<String>,
    pub(crate) config_path: Option<String>,
    pub(crate) configured_team: Option<String>,
    pub(crate) owner_repo: Option<String>,
    pub(crate) error: Option<String>,
}
