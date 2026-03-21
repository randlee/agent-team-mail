//! Settings resolution helpers

// Re-export canonical home/path helpers from core
#[allow(unused_imports)]
pub use agent_team_mail_core::home::{
    claude_root_dir_for, config_claude_root_dir, config_claude_root_dir_for, config_team_dir,
    config_team_dir_for, config_teams_root_dir, config_teams_root_dir_for, get_home_dir,
    get_os_home_dir, teams_root_dir_for,
};
