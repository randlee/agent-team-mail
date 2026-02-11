//! Platform detection

use serde::{Deserialize, Serialize};

/// Operating system platform
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Platform {
    /// macOS (Darwin)
    MacOS,
    /// Linux
    Linux,
    /// Windows
    Windows,
}

impl Platform {
    /// Detect the current platform
    pub fn detect() -> Self {
        match std::env::consts::OS {
            "macos" => Platform::MacOS,
            "linux" => Platform::Linux,
            "windows" => Platform::Windows,
            os => {
                // Default to Linux for unknown Unix-like systems
                eprintln!("Warning: Unknown OS '{os}', defaulting to Linux");
                Platform::Linux
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_platform_detection() {
        let platform = Platform::detect();

        // Should match the current OS
        #[cfg(target_os = "macos")]
        assert_eq!(platform, Platform::MacOS);

        #[cfg(target_os = "linux")]
        assert_eq!(platform, Platform::Linux);

        #[cfg(target_os = "windows")]
        assert_eq!(platform, Platform::Windows);
    }
}
