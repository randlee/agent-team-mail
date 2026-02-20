//! Dynamic library loading for external issue providers

use super::registry::ProviderFactory;
use crate::plugin::PluginError;
use libloading::{Library, Symbol};
use std::path::{Path, PathBuf};
use tracing::{debug, warn};

/// Provider loader for dynamically loaded libraries
///
/// Scans directories for provider libraries (.dylib/.so/.dll) and loads them.
/// Each library must export a C-ABI function named `atm_create_provider_factory`.
pub struct ProviderLoader {
    /// Keep loaded libraries alive (they must not be dropped while factories exist)
    libraries: Vec<Library>,
}

impl ProviderLoader {
    /// Create a new loader
    pub fn new() -> Self {
        Self {
            libraries: Vec::new(),
        }
    }

    /// Scan a directory and load all provider libraries
    ///
    /// # Arguments
    ///
    /// * `dir` - Directory to scan for provider libraries
    ///
    /// # Returns
    ///
    /// A vector of successfully loaded provider factories.
    /// Errors for individual libraries are logged as warnings and do not fail the entire operation.
    ///
    /// # Errors
    ///
    /// Returns `PluginError::Provider` if the directory cannot be read.
    pub fn load_from_directory(&mut self, dir: &Path) -> Result<Vec<ProviderFactory>, PluginError> {
        if !dir.exists() {
            debug!(
                "Provider directory does not exist, skipping: {}",
                dir.display()
            );
            return Ok(Vec::new());
        }

        if !dir.is_dir() {
            return Err(PluginError::Provider {
                message: format!("Provider path is not a directory: {}", dir.display()),
                source: None,
            });
        }

        let entries = std::fs::read_dir(dir).map_err(|e| PluginError::Provider {
            message: format!("Failed to read provider directory: {}", dir.display()),
            source: Some(Box::new(e)),
        })?;

        let mut factories = Vec::new();

        for entry in entries {
            let entry = match entry {
                Ok(e) => e,
                Err(e) => {
                    warn!("Failed to read directory entry: {}", e);
                    continue;
                }
            };

            let path = entry.path();

            // Only load files with provider library extensions
            if !Self::is_provider_library(&path) {
                continue;
            }

            match self.load_library(&path) {
                Ok(factory) => {
                    debug!("Loaded provider '{}' from {}", factory.name, path.display());
                    factories.push(factory);
                }
                Err(e) => {
                    warn!("Failed to load provider from {}: {}", path.display(), e);
                }
            }
        }

        Ok(factories)
    }

    /// Load a specific provider library
    ///
    /// # Arguments
    ///
    /// * `paths` - Paths to provider libraries to load
    ///
    /// # Returns
    ///
    /// A vector of successfully loaded provider factories.
    /// Errors for individual libraries are logged as warnings.
    pub fn load_libraries(&mut self, paths: &[PathBuf]) -> Vec<ProviderFactory> {
        let mut factories = Vec::new();

        for path in paths {
            match self.load_library(path) {
                Ok(factory) => {
                    debug!("Loaded provider '{}' from {}", factory.name, path.display());
                    factories.push(factory);
                }
                Err(e) => {
                    warn!("Failed to load provider from {}: {}", path.display(), e);
                }
            }
        }

        factories
    }

    /// Load a single provider library
    ///
    /// # Safety
    ///
    /// This function loads dynamic libraries, which is inherently unsafe.
    /// The library must export a valid `atm_create_provider_factory` function.
    fn load_library(&mut self, path: &Path) -> Result<ProviderFactory, PluginError> {
        // Safety: We trust that the library at `path` is a valid provider library
        // that exports the required symbol. In production, this should only load
        // trusted libraries from secure locations.
        let lib = unsafe {
            Library::new(path).map_err(|e| PluginError::Provider {
                message: format!("Failed to load library: {}", path.display()),
                source: Some(Box::new(e)),
            })?
        };

        // Safety: We expect the library to export this C-ABI function.
        // The function signature must match exactly.
        let factory = unsafe {
            let symbol: Symbol<extern "C" fn() -> *mut ProviderFactory> = lib
                .get(b"atm_create_provider_factory")
                .map_err(|e| PluginError::Provider {
                    message: format!(
                        "Library missing 'atm_create_provider_factory' symbol: {}",
                        path.display()
                    ),
                    source: Some(Box::new(e)),
                })?;

            let factory_ptr = symbol();
            if factory_ptr.is_null() {
                return Err(PluginError::Provider {
                    message: format!("Factory function returned null: {}", path.display()),
                    source: None,
                });
            }

            // Take ownership of the factory (caller must have allocated it with Box::into_raw)
            Box::from_raw(factory_ptr)
        };

        // Keep the library alive so the factory function remains valid
        self.libraries.push(lib);

        Ok(*factory)
    }

    /// Check if a path is a provider library based on file extension
    fn is_provider_library(path: &Path) -> bool {
        if !path.is_file() {
            return false;
        }

        let extension = path.extension().and_then(|e| e.to_str());

        match extension {
            Some("dylib") => true, // macOS
            Some("so") => true,    // Linux
            Some("dll") => true,   // Windows
            _ => false,
        }
    }
}

impl Default for ProviderLoader {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_is_provider_library() {
        let temp_dir = tempfile::tempdir().unwrap();

        // Create test files with different extensions
        let dylib_path = temp_dir.path().join("test.dylib");
        let so_path = temp_dir.path().join("test.so");
        let dll_path = temp_dir.path().join("test.dll");
        let txt_path = temp_dir.path().join("test.txt");
        let rs_path = temp_dir.path().join("test.rs");

        std::fs::write(&dylib_path, "test").unwrap();
        std::fs::write(&so_path, "test").unwrap();
        std::fs::write(&dll_path, "test").unwrap();
        std::fs::write(&txt_path, "test").unwrap();
        std::fs::write(&rs_path, "test").unwrap();

        assert!(ProviderLoader::is_provider_library(&dylib_path));
        assert!(ProviderLoader::is_provider_library(&so_path));
        assert!(ProviderLoader::is_provider_library(&dll_path));
        assert!(!ProviderLoader::is_provider_library(&txt_path));
        assert!(!ProviderLoader::is_provider_library(&rs_path));

        // Non-existent files should return false
        assert!(!ProviderLoader::is_provider_library(Path::new(
            "/nonexistent/test.dylib"
        )));
    }

    #[test]
    fn test_load_from_directory_missing() {
        let mut loader = ProviderLoader::new();
        let result = loader.load_from_directory(Path::new("/nonexistent/path"));
        assert!(result.is_ok());
        assert_eq!(result.unwrap().len(), 0);
    }

    #[test]
    fn test_load_from_directory_empty() {
        let temp_dir = tempfile::tempdir().unwrap();
        let mut loader = ProviderLoader::new();
        let result = loader.load_from_directory(temp_dir.path());
        assert!(result.is_ok());
        assert_eq!(result.unwrap().len(), 0);
    }

    #[test]
    fn test_load_from_directory_not_a_directory() {
        let temp_dir = tempfile::tempdir().unwrap();
        let file_path = temp_dir.path().join("test.txt");
        std::fs::write(&file_path, "test").unwrap();

        let mut loader = ProviderLoader::new();
        let result = loader.load_from_directory(&file_path);
        assert!(result.is_err());
        assert!(result
            .unwrap_err()
            .to_string()
            .contains("not a directory"));
    }

    #[test]
    fn test_load_libraries_empty() {
        let mut loader = ProviderLoader::new();
        let factories = loader.load_libraries(&[]);
        assert_eq!(factories.len(), 0);
    }
}
