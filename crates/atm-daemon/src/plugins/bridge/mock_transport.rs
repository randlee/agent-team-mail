//! Mock transport implementation for testing
//!
//! Provides an in-memory transport that simulates file operations
//! without requiring actual network connections.

use super::transport::{Result, Transport, TransportError};
use async_trait::async_trait;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use tokio::time::{sleep, Duration};

/// In-memory state for mock transport
#[derive(Debug, Clone, Default)]
struct MockState {
    /// Files stored in memory: path -> content
    #[allow(clippy::zero_sized_map_values)]
    files: HashMap<PathBuf, Vec<u8>>,

    /// Connection status
    connected: bool,

    /// Simulate connection failures
    fail_connect: bool,

    /// Simulate upload failures
    fail_upload: bool,

    /// Simulate download failures
    fail_download: bool,

    /// Simulated latency in milliseconds
    latency_ms: u64,
}

/// Shared filesystem backend for multiple MockTransport instances
///
/// Simulates a real remote filesystem where multiple transports can read/write
/// the same files. Used for E2E testing with multiple nodes.
#[derive(Debug, Clone, Default)]
pub struct SharedFilesystem {
    files: Arc<Mutex<HashMap<PathBuf, Vec<u8>>>>,
}

impl SharedFilesystem {
    /// Create a new shared filesystem
    pub fn new() -> Self {
        Self {
            files: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    /// Get a file's contents
    pub fn get(&self, path: &Path) -> Option<Vec<u8>> {
        let files = self.files.lock().unwrap();
        files.get(path).cloned()
    }

    /// Write a file
    pub fn put(&self, path: PathBuf, content: Vec<u8>) {
        let mut files = self.files.lock().unwrap();
        files.insert(path, content);
    }

    /// Remove a file
    fn remove(&self, path: &Path) -> Option<Vec<u8>> {
        let mut files = self.files.lock().unwrap();
        files.remove(path)
    }

    /// List files in a directory
    fn list(&self, dir: &Path) -> Vec<PathBuf> {
        let files = self.files.lock().unwrap();
        files.keys()
            .filter(|p| p.parent() == Some(dir))
            .cloned()
            .collect()
    }

    /// Check if file exists
    fn exists(&self, path: &Path) -> bool {
        let files = self.files.lock().unwrap();
        files.contains_key(path)
    }

    /// Clear all files
    fn clear(&self) {
        let mut files = self.files.lock().unwrap();
        files.clear();
    }
}

/// Mock transport implementation for testing
///
/// Stores "files" in memory and simulates network operations.
/// Thread-safe via Arc<Mutex<...>>.
#[derive(Debug, Clone)]
pub struct MockTransport {
    state: Arc<Mutex<MockState>>,
}

impl MockTransport {
    /// Create a new mock transport
    pub fn new() -> Self {
        Self {
            state: Arc::new(Mutex::new(MockState::default())),
        }
    }

    /// Set simulated latency for all operations
    pub fn set_latency(&self, latency_ms: u64) {
        let mut state = self.state.lock().unwrap();
        state.latency_ms = latency_ms;
    }

    /// Enable connection failure simulation
    pub fn set_fail_connect(&self, fail: bool) {
        let mut state = self.state.lock().unwrap();
        state.fail_connect = fail;
    }

    /// Enable upload failure simulation
    pub fn set_fail_upload(&self, fail: bool) {
        let mut state = self.state.lock().unwrap();
        state.fail_upload = fail;
    }

    /// Enable download failure simulation
    pub fn set_fail_download(&self, fail: bool) {
        let mut state = self.state.lock().unwrap();
        state.fail_download = fail;
    }

    /// Get a copy of a file's contents (for test assertions)
    pub fn get_file(&self, path: &Path) -> Option<Vec<u8>> {
        let state = self.state.lock().unwrap();
        state.files.get(path).cloned()
    }

    /// Check if a file exists (for test assertions)
    pub fn file_exists(&self, path: &Path) -> bool {
        let state = self.state.lock().unwrap();
        state.files.contains_key(path)
    }

    /// Clear all files (for test cleanup)
    pub fn clear(&self) {
        let mut state = self.state.lock().unwrap();
        state.files.clear();
    }

    /// Simulate latency if configured
    async fn simulate_latency(&self) {
        let latency_ms = {
            let state = self.state.lock().unwrap();
            state.latency_ms
        };

        if latency_ms > 0 {
            sleep(Duration::from_millis(latency_ms)).await;
        }
    }
}

impl Default for MockTransport {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl Transport for MockTransport {
    async fn connect(&mut self) -> Result<()> {
        self.simulate_latency().await;

        let mut state = self.state.lock().unwrap();

        if state.fail_connect {
            return Err(TransportError::ConnectionFailed {
                message: "Simulated connection failure".to_string(),
            });
        }

        state.connected = true;
        Ok(())
    }

    async fn is_connected(&self) -> bool {
        let state = self.state.lock().unwrap();
        state.connected
    }

    async fn upload(&self, local_path: &Path, remote_path: &Path) -> Result<()> {
        self.simulate_latency().await;

        // Check state before async operation
        {
            let state = self.state.lock().unwrap();

            if !state.connected {
                return Err(TransportError::ConnectionFailed {
                    message: "Not connected".to_string(),
                });
            }

            if state.fail_upload {
                return Err(TransportError::RemoteError {
                    message: "Simulated upload failure".to_string(),
                });
            }
        } // Lock is dropped here

        // Read local file
        let content = tokio::fs::read(local_path).await?;

        // Store in mock filesystem
        {
            let mut state = self.state.lock().unwrap();
            state.files.insert(remote_path.to_path_buf(), content);
        }

        Ok(())
    }

    async fn download(&self, remote_path: &Path, local_path: &Path) -> Result<()> {
        self.simulate_latency().await;

        // Get file content from mock filesystem
        let content = {
            let state = self.state.lock().unwrap();

            if !state.connected {
                return Err(TransportError::ConnectionFailed {
                    message: "Not connected".to_string(),
                });
            }

            if state.fail_download {
                return Err(TransportError::RemoteError {
                    message: "Simulated download failure".to_string(),
                });
            }

            // Get file from mock filesystem
            state.files.get(remote_path).ok_or_else(|| {
                TransportError::RemoteError {
                    message: format!("File not found: {}", remote_path.display()),
                }
            })?.clone()
        }; // Lock is dropped here

        // Write to local file
        tokio::fs::write(local_path, content).await?;

        Ok(())
    }

    async fn list(&self, remote_dir: &Path, pattern: &str) -> Result<Vec<String>> {
        self.simulate_latency().await;

        let matches = {
            let state = self.state.lock().unwrap();

            if !state.connected {
                return Err(TransportError::ConnectionFailed {
                    message: "Not connected".to_string(),
                });
            }

            // Simple glob matching: pattern may contain '*' wildcard
            let mut matches = Vec::new();

            for path in state.files.keys() {
                // Check if path is in the specified directory
                if let Some(parent) = path.parent()
                    && parent == remote_dir
                    && let Some(filename) = path.file_name().and_then(|n| n.to_str())
                    && pattern_matches(pattern, filename)
                {
                    matches.push(filename.to_string());
                }
            }

            matches
        }; // Lock is dropped here

        Ok(matches)
    }

    async fn rename(&self, from: &Path, to: &Path) -> Result<()> {
        self.simulate_latency().await;

        let mut state = self.state.lock().unwrap();

        if !state.connected {
            return Err(TransportError::ConnectionFailed {
                message: "Not connected".to_string(),
            });
        }

        // Get the content
        let content = state.files.remove(from).ok_or_else(|| {
            TransportError::RemoteError {
                message: format!("Source file not found: {}", from.display()),
            }
        })?;

        // Insert with new path
        state.files.insert(to.to_path_buf(), content);

        Ok(())
    }

    async fn disconnect(&mut self) -> Result<()> {
        self.simulate_latency().await;

        let mut state = self.state.lock().unwrap();
        state.connected = false;

        Ok(())
    }
}

/// Shared mock transport for E2E testing
///
/// Multiple instances can share the same underlying filesystem,
/// simulating real network file transfers between nodes.
#[derive(Debug, Clone)]
pub struct SharedMockTransport {
    /// Shared filesystem backend
    filesystem: SharedFilesystem,

    /// Local state (connection status, failure simulation)
    state: Arc<Mutex<MockState>>,
}

impl SharedMockTransport {
    /// Create a new shared mock transport with a shared filesystem
    ///
    /// Transport starts in connected state for convenience in tests.
    pub fn new(filesystem: SharedFilesystem) -> Self {
        Self {
            filesystem,
            state: Arc::new(Mutex::new(MockState {
                connected: true, // Start connected
                ..Default::default()
            })),
        }
    }

    /// Set simulated latency for all operations
    pub fn set_latency(&self, latency_ms: u64) {
        let mut state = self.state.lock().unwrap();
        state.latency_ms = latency_ms;
    }

    /// Enable connection failure simulation
    pub fn set_fail_connect(&self, fail: bool) {
        let mut state = self.state.lock().unwrap();
        state.fail_connect = fail;
    }

    /// Enable upload failure simulation
    pub fn set_fail_upload(&self, fail: bool) {
        let mut state = self.state.lock().unwrap();
        state.fail_upload = fail;
    }

    /// Enable download failure simulation
    pub fn set_fail_download(&self, fail: bool) {
        let mut state = self.state.lock().unwrap();
        state.fail_download = fail;
    }

    /// Get a copy of a file's contents (for test assertions)
    pub fn get_file(&self, path: &Path) -> Option<Vec<u8>> {
        self.filesystem.get(path)
    }

    /// Check if a file exists (for test assertions)
    pub fn file_exists(&self, path: &Path) -> bool {
        self.filesystem.exists(path)
    }

    /// Clear all files (for test cleanup)
    pub fn clear(&self) {
        self.filesystem.clear();
    }

    /// Simulate latency if configured
    async fn simulate_latency(&self) {
        let latency_ms = {
            let state = self.state.lock().unwrap();
            state.latency_ms
        };

        if latency_ms > 0 {
            sleep(Duration::from_millis(latency_ms)).await;
        }
    }
}

#[async_trait]
impl Transport for SharedMockTransport {
    async fn connect(&mut self) -> Result<()> {
        self.simulate_latency().await;

        let mut state = self.state.lock().unwrap();

        if state.fail_connect {
            return Err(TransportError::ConnectionFailed {
                message: "Simulated connection failure".to_string(),
            });
        }

        state.connected = true;
        Ok(())
    }

    async fn is_connected(&self) -> bool {
        let state = self.state.lock().unwrap();
        state.connected
    }

    async fn upload(&self, local_path: &Path, remote_path: &Path) -> Result<()> {
        self.simulate_latency().await;

        // Check state before async operation
        {
            let state = self.state.lock().unwrap();

            if !state.connected {
                return Err(TransportError::ConnectionFailed {
                    message: "Not connected".to_string(),
                });
            }

            if state.fail_upload {
                return Err(TransportError::RemoteError {
                    message: "Simulated upload failure".to_string(),
                });
            }
        } // Lock is dropped here

        // Read local file
        let content = tokio::fs::read(local_path).await?;

        // Store in shared filesystem
        self.filesystem.put(remote_path.to_path_buf(), content);

        Ok(())
    }

    async fn download(&self, remote_path: &Path, local_path: &Path) -> Result<()> {
        self.simulate_latency().await;

        // Get file content from shared filesystem
        let content = {
            let state = self.state.lock().unwrap();

            if !state.connected {
                return Err(TransportError::ConnectionFailed {
                    message: "Not connected".to_string(),
                });
            }

            if state.fail_download {
                return Err(TransportError::RemoteError {
                    message: "Simulated download failure".to_string(),
                });
            }

            // Get file from shared filesystem
            self.filesystem.get(remote_path).ok_or_else(|| {
                TransportError::RemoteError {
                    message: format!("File not found: {}", remote_path.display()),
                }
            })?
        }; // Lock is dropped here

        // Write to local file
        tokio::fs::write(local_path, content).await?;

        Ok(())
    }

    async fn list(&self, remote_dir: &Path, pattern: &str) -> Result<Vec<String>> {
        self.simulate_latency().await;

        let matches = {
            let state = self.state.lock().unwrap();

            if !state.connected {
                return Err(TransportError::ConnectionFailed {
                    message: "Not connected".to_string(),
                });
            }

            // Get files from shared filesystem
            let files = self.filesystem.list(remote_dir);
            let mut matches = Vec::new();

            for path in files {
                if let Some(filename) = path.file_name().and_then(|n| n.to_str())
                    && pattern_matches(pattern, filename)
                {
                    matches.push(filename.to_string());
                }
            }

            matches
        }; // Lock is dropped here

        Ok(matches)
    }

    async fn rename(&self, from: &Path, to: &Path) -> Result<()> {
        self.simulate_latency().await;

        let state = self.state.lock().unwrap();

        if !state.connected {
            return Err(TransportError::ConnectionFailed {
                message: "Not connected".to_string(),
            });
        }

        // Get the content
        let content = self.filesystem.remove(from).ok_or_else(|| {
            TransportError::RemoteError {
                message: format!("Source file not found: {}", from.display()),
            }
        })?;

        // Insert with new path
        self.filesystem.put(to.to_path_buf(), content);

        Ok(())
    }

    async fn disconnect(&mut self) -> Result<()> {
        self.simulate_latency().await;

        let mut state = self.state.lock().unwrap();
        state.connected = false;

        Ok(())
    }
}

/// Simple glob pattern matching (supports * wildcard only)
fn pattern_matches(pattern: &str, filename: &str) -> bool {
    if pattern == "*" {
        return true;
    }

    if !pattern.contains('*') {
        return pattern == filename;
    }

    // Split pattern on '*' and check if all parts appear in order
    let parts: Vec<&str> = pattern.split('*').collect();

    if parts.is_empty() {
        return true;
    }

    let mut pos = 0;

    for (i, part) in parts.iter().enumerate() {
        if part.is_empty() {
            continue;
        }

        if i == 0 && !filename[pos..].starts_with(part) {
            return false;
        }

        if let Some(found_pos) = filename[pos..].find(part) {
            pos += found_pos + part.len();
        } else {
            return false;
        }
    }

    // If pattern ends with '*', we're done
    // Otherwise, check that we've consumed the entire filename
    if let Some(last_part) = parts.last()
        && !last_part.is_empty()
        && !filename.ends_with(last_part)
    {
        return false;
    }

    true
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[tokio::test]
    async fn test_connect_disconnect() {
        let mut transport = MockTransport::new();

        assert!(!transport.is_connected().await);

        transport.connect().await.unwrap();
        assert!(transport.is_connected().await);

        transport.disconnect().await.unwrap();
        assert!(!transport.is_connected().await);
    }

    #[tokio::test]
    async fn test_connect_failure() {
        let mut transport = MockTransport::new();
        transport.set_fail_connect(true);

        let result = transport.connect().await;
        assert!(result.is_err());
        assert!(!transport.is_connected().await);
    }

    #[tokio::test]
    async fn test_upload_download_roundtrip() {
        let temp_dir = TempDir::new().unwrap();
        let local_file = temp_dir.path().join("test.txt");
        let remote_path = Path::new("/remote/test.txt");

        // Write test content
        let test_content = b"Hello, transport!";
        tokio::fs::write(&local_file, test_content).await.unwrap();

        // Upload
        let mut transport = MockTransport::new();
        transport.connect().await.unwrap();
        transport.upload(&local_file, remote_path).await.unwrap();

        // Verify file exists in mock
        assert!(transport.file_exists(remote_path));

        // Download to a different local path
        let download_file = temp_dir.path().join("downloaded.txt");
        transport
            .download(remote_path, &download_file)
            .await
            .unwrap();

        // Verify content matches
        let downloaded_content = tokio::fs::read(&download_file).await.unwrap();
        assert_eq!(downloaded_content, test_content);
    }

    #[tokio::test]
    async fn test_upload_not_connected() {
        let temp_dir = TempDir::new().unwrap();
        let local_file = temp_dir.path().join("test.txt");
        let remote_path = Path::new("/remote/test.txt");

        tokio::fs::write(&local_file, b"content").await.unwrap();

        let transport = MockTransport::new();
        let result = transport.upload(&local_file, remote_path).await;

        assert!(result.is_err());
        assert!(result
            .unwrap_err()
            .to_string()
            .contains("Not connected"));
    }

    #[tokio::test]
    async fn test_upload_failure() {
        let temp_dir = TempDir::new().unwrap();
        let local_file = temp_dir.path().join("test.txt");
        let remote_path = Path::new("/remote/test.txt");

        tokio::fs::write(&local_file, b"content").await.unwrap();

        let mut transport = MockTransport::new();
        transport.connect().await.unwrap();
        transport.set_fail_upload(true);

        let result = transport.upload(&local_file, remote_path).await;
        assert!(result.is_err());
        assert!(result
            .unwrap_err()
            .to_string()
            .contains("Simulated upload failure"));
    }

    #[tokio::test]
    async fn test_download_not_found() {
        let temp_dir = TempDir::new().unwrap();
        let remote_path = Path::new("/remote/nonexistent.txt");
        let local_file = temp_dir.path().join("download.txt");

        let mut transport = MockTransport::new();
        transport.connect().await.unwrap();

        let result = transport.download(remote_path, &local_file).await;
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("File not found"));
    }

    #[tokio::test]
    async fn test_download_failure() {
        let temp_dir = TempDir::new().unwrap();
        let remote_path = Path::new("/remote/test.txt");
        let local_file = temp_dir.path().join("download.txt");

        let mut transport = MockTransport::new();
        transport.connect().await.unwrap();
        transport.set_fail_download(true);

        let result = transport.download(remote_path, &local_file).await;
        assert!(result.is_err());
        assert!(result
            .unwrap_err()
            .to_string()
            .contains("Simulated download failure"));
    }

    #[tokio::test]
    async fn test_list_files() {
        let mut transport = MockTransport::new();
        transport.connect().await.unwrap();

        let remote_dir = Path::new("/remote/inboxes");

        // Manually populate mock filesystem
        {
            let mut state = transport.state.lock().unwrap();
            state
                .files
                .insert(remote_dir.join("agent1.json"), vec![]);
            state
                .files
                .insert(remote_dir.join("agent2.json"), vec![]);
            state
                .files
                .insert(remote_dir.join("agent1.laptop.json"), vec![]);
            state
                .files
                .insert(Path::new("/other/file.txt").to_path_buf(), vec![]);
        }

        // List all JSON files
        let files = transport.list(remote_dir, "*.json").await.unwrap();
        assert_eq!(files.len(), 3);
        assert!(files.contains(&"agent1.json".to_string()));
        assert!(files.contains(&"agent2.json".to_string()));
        assert!(files.contains(&"agent1.laptop.json".to_string()));
        assert!(!files.contains(&"file.txt".to_string()));

        // List with specific pattern
        let files = transport.list(remote_dir, "agent1*").await.unwrap();
        assert_eq!(files.len(), 2);
        assert!(files.contains(&"agent1.json".to_string()));
        assert!(files.contains(&"agent1.laptop.json".to_string()));
    }

    #[tokio::test]
    async fn test_rename() {
        let temp_dir = TempDir::new().unwrap();
        let local_file = temp_dir.path().join("test.txt");
        let from_path = Path::new("/remote/temp.txt");
        let to_path = Path::new("/remote/final.txt");

        tokio::fs::write(&local_file, b"content").await.unwrap();

        let mut transport = MockTransport::new();
        transport.connect().await.unwrap();
        transport.upload(&local_file, from_path).await.unwrap();

        assert!(transport.file_exists(from_path));
        assert!(!transport.file_exists(to_path));

        // Rename
        transport.rename(from_path, to_path).await.unwrap();

        assert!(!transport.file_exists(from_path));
        assert!(transport.file_exists(to_path));
    }

    #[tokio::test]
    async fn test_rename_not_found() {
        let mut transport = MockTransport::new();
        transport.connect().await.unwrap();

        let from_path = Path::new("/remote/nonexistent.txt");
        let to_path = Path::new("/remote/final.txt");

        let result = transport.rename(from_path, to_path).await;
        assert!(result.is_err());
        assert!(result
            .unwrap_err()
            .to_string()
            .contains("Source file not found"));
    }

    #[tokio::test]
    async fn test_latency_simulation() {
        let mut transport = MockTransport::new();
        transport.set_latency(50); // 50ms latency

        let start = std::time::Instant::now();
        transport.connect().await.unwrap();
        let elapsed = start.elapsed();

        // Should take at least 50ms
        assert!(elapsed.as_millis() >= 50);
    }

    #[test]
    fn test_pattern_matches() {
        assert!(pattern_matches("*", "anything.txt"));
        assert!(pattern_matches("*.json", "file.json"));
        assert!(!pattern_matches("*.json", "file.txt"));
        assert!(pattern_matches("agent*", "agent1.json"));
        assert!(pattern_matches("agent*.json", "agent1.json"));
        assert!(pattern_matches("agent*.json", "agent1.laptop.json"));
        assert!(!pattern_matches("agent*.json", "other.json"));
        assert!(pattern_matches("agent1.json", "agent1.json"));
        assert!(!pattern_matches("agent1.json", "agent2.json"));
    }

    #[tokio::test]
    async fn test_clear() {
        let temp_dir = TempDir::new().unwrap();
        let local_file = temp_dir.path().join("test.txt");
        let remote_path = Path::new("/remote/test.txt");

        tokio::fs::write(&local_file, b"content").await.unwrap();

        let mut transport = MockTransport::new();
        transport.connect().await.unwrap();
        transport.upload(&local_file, remote_path).await.unwrap();

        assert!(transport.file_exists(remote_path));

        transport.clear();

        assert!(!transport.file_exists(remote_path));
    }
}
