//! System and repository context detection
//!
//! This module provides runtime context about the system, Claude installation,
//! and current repository. All detection is local (no network calls).

mod platform;
mod repo;
mod system;

pub use platform::Platform;
pub use repo::{GitProvider, RepoContext};
pub use system::SystemContext;
