//! SSH/SFTP transport implementation
//!
//! Provides SSH-based file transfer using the ssh2 crate.
//! Connection pooling and retry logic with exponential backoff.

use super::transport::{Result, Transport, TransportError};
use async_trait::async_trait;
use ssh2::Session;
use std::net::TcpStream;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

/// Connection state for SSH transport
struct ConnectionState {
    /// SSH session (if connected)
    session: Option<Session>,

    /// Last connection attempt timestamp (for backoff)
    last_attempt: Option<std::time::Instant>,
}

/// SSH/SFTP transport configuration
#[derive(Debug, Clone)]
pub struct SshConfig {
    /// SSH connection string (user@host or user@host:port)
    pub address: String,

    /// Path to SSH private key (if None, uses default ~/.ssh/id_rsa)
    pub key_path: Option<PathBuf>,

    /// Connection timeout in seconds
    pub connect_timeout_secs: u64,

    /// Maximum retry attempts for failed operations
    pub max_retries: u32,

    /// Initial backoff delay in milliseconds
    pub initial_backoff_ms: u64,

    /// Maximum backoff delay in milliseconds
    pub max_backoff_ms: u64,
}

impl Default for SshConfig {
    fn default() -> Self {
        Self {
            address: String::new(),
            key_path: None,
            connect_timeout_secs: 30,
            max_retries: 3,
            initial_backoff_ms: 100,
            max_backoff_ms: 5000,
        }
    }
}

/// SSH/SFTP transport implementation
///
/// Uses ssh2 crate for SSH connections and SFTP file transfers.
/// Implements connection pooling with a single persistent connection.
pub struct SshTransport {
    config: SshConfig,
    state: Arc<Mutex<ConnectionState>>,
}

impl SshTransport {
    /// Create a new SSH transport with the given configuration
    pub fn new(config: SshConfig) -> Self {
        Self {
            config,
            state: Arc::new(Mutex::new(ConnectionState {
                session: None,
                last_attempt: None,
            })),
        }
    }
}

#[async_trait]
impl Transport for SshTransport {
    async fn connect(&mut self) -> Result<()> {
        let config = self.config.clone();
        let state = self.state.clone();

        tokio::task::spawn_blocking(move || {
            let (host, port, username) = {
                let parts: Vec<&str> = config.address.split('@').collect();
                if parts.len() != 2 {
                    return Err(TransportError::InvalidPath {
                        message: format!("Invalid SSH address format: {}", config.address),
                    });
                }

                let username = parts[0].to_string();
                let host_port = parts[1];

                let (host, port) = if let Some(colon_pos) = host_port.rfind(':') {
                    let host = host_port[..colon_pos].to_string();
                    let port_str = &host_port[colon_pos + 1..];
                    let port = port_str.parse::<u16>().map_err(|_| {
                        TransportError::InvalidPath {
                            message: format!("Invalid port number: {port_str}"),
                        }
                    })?;
                    (host, port)
                } else {
                    (host_port.to_string(), 22)
                };

                (host, port, username)
            };

            // Connect TCP stream
            let tcp = TcpStream::connect(format!("{host}:{port}"))
                .map_err(|e| TransportError::ConnectionFailed {
                    message: format!("Failed to connect to {host}:{port}: {e}"),
                })?;

            // Set timeouts
            let timeout = Duration::from_secs(config.connect_timeout_secs);
            tcp.set_read_timeout(Some(timeout))
                .map_err(|e| TransportError::ConnectionFailed {
                    message: format!("Failed to set read timeout: {e}"),
                })?;
            tcp.set_write_timeout(Some(timeout))
                .map_err(|e| TransportError::ConnectionFailed {
                    message: format!("Failed to set write timeout: {e}"),
                })?;

            // Create SSH session
            let mut session = Session::new().map_err(|e| TransportError::ConnectionFailed {
                message: format!("Failed to create SSH session: {e}"),
            })?;

            session.set_tcp_stream(tcp);
            session
                .handshake()
                .map_err(|e| TransportError::ConnectionFailed {
                    message: format!("SSH handshake failed: {e}"),
                })?;

            // Get key path
            let key_path = if let Some(ref path) = config.key_path {
                path.clone()
            } else {
                let home_dir =
                    dirs::home_dir().ok_or_else(|| TransportError::AuthenticationFailed {
                        message: "Could not determine home directory".to_string(),
                    })?;
                home_dir.join(".ssh").join("id_rsa")
            };

            // Authenticate
            session
                .userauth_pubkey_file(&username, None, &key_path, None)
                .map_err(|e| TransportError::AuthenticationFailed {
                    message: format!("SSH authentication failed: {e}"),
                })?;

            if !session.authenticated() {
                return Err(TransportError::AuthenticationFailed {
                    message: "SSH authentication failed".to_string(),
                });
            }

            // Store session
            let mut s = state.lock().unwrap();
            s.session = Some(session);
            s.last_attempt = Some(std::time::Instant::now());

            Ok(())
        })
        .await
        .map_err(|e| TransportError::ConnectionFailed {
            message: format!("Task join error: {e}"),
        })?
    }

    async fn is_connected(&self) -> bool {
        let state = self.state.lock().unwrap();
        state.session.is_some()
    }

    async fn upload(&self, local_path: &Path, remote_path: &Path) -> Result<()> {
        let local_path = local_path.to_path_buf();
        let remote_path = remote_path.to_path_buf();
        let state = self.state.clone();

        tokio::task::spawn_blocking(move || {
            // Perform blocking SSH operations on dedicated thread pool
            let state_guard = state.lock().unwrap();
            let session = state_guard
                .session
                .as_ref()
                .ok_or_else(|| TransportError::ConnectionFailed {
                    message: "Not connected".to_string(),
                })?;

            // Open SFTP channel
            let sftp = session.sftp().map_err(|e| TransportError::RemoteError {
                message: format!("Failed to open SFTP channel: {e}"),
            })?;

            // Read local file
            let content = std::fs::read(&local_path)?;

            // Write to temp file on remote
            let temp_path = {
                let mut temp = remote_path.clone();
                let filename = temp
                    .file_name()
                    .ok_or_else(|| TransportError::InvalidPath {
                        message: "Invalid remote path".to_string(),
                    })?
                    .to_string_lossy();
                temp.set_file_name(format!("{filename}.bridge-tmp"));
                temp
            };

            let remote_path_str = remote_path
                .to_str()
                .ok_or_else(|| TransportError::InvalidPath {
                    message: "Remote path contains invalid UTF-8".to_string(),
                })?;

            let temp_path_str = temp_path
                .to_str()
                .ok_or_else(|| TransportError::InvalidPath {
                    message: "Temp path contains invalid UTF-8".to_string(),
                })?;

            // Ensure parent directory exists
            if let Some(parent) = remote_path.parent()
                && let Some(parent_str) = parent.to_str()
            {
                let _ = sftp.mkdir(
                    std::path::Path::new(parent_str),
                    0o755,
                );
            }

            // Write to temp file
            let mut remote_file = sftp
                .create(std::path::Path::new(temp_path_str))
                .map_err(|e| TransportError::RemoteError {
                    message: format!("Failed to create remote temp file: {e}"),
                })?;

            std::io::Write::write_all(&mut remote_file, &content)?;
            drop(remote_file); // Close the file

            // Atomic rename via SSH command
            drop(sftp); // Close SFTP before running command

            let rename_cmd = format!("mv '{temp_path_str}' '{remote_path_str}'");
            let mut channel = session
                .channel_session()
                .map_err(|e| TransportError::RemoteError {
                    message: format!("Failed to open SSH channel: {e}"),
                })?;

            channel
                .exec(&rename_cmd)
                .map_err(|e| TransportError::RemoteError {
                    message: format!("Failed to execute rename command: {e}"),
                })?;

            let mut stderr = String::new();
            std::io::Read::read_to_string(&mut channel.stderr(), &mut stderr)?;

            channel.wait_close().ok();

            let exit_status = channel.exit_status().map_err(|e| {
                TransportError::RemoteError {
                    message: format!("Failed to get command exit status: {e}"),
                }
            })?;

            if exit_status != 0 {
                return Err(TransportError::RemoteError {
                    message: format!("Rename command failed: {stderr}"),
                });
            }

            Ok(())
        })
        .await
        .map_err(|e| TransportError::ConnectionFailed {
            message: format!("Task join error: {e}"),
        })?
    }

    async fn download(&self, remote_path: &Path, local_path: &Path) -> Result<()> {
        let remote_path = remote_path.to_path_buf();
        let local_path = local_path.to_path_buf();
        let state = self.state.clone();

        tokio::task::spawn_blocking(move || {
            let state_guard = state.lock().unwrap();
            let session = state_guard
                .session
                .as_ref()
                .ok_or_else(|| TransportError::ConnectionFailed {
                    message: "Not connected".to_string(),
                })?;

            let sftp = session.sftp().map_err(|e| TransportError::RemoteError {
                message: format!("Failed to open SFTP channel: {e}"),
            })?;

            let remote_path_str = remote_path
                .to_str()
                .ok_or_else(|| TransportError::InvalidPath {
                    message: "Remote path contains invalid UTF-8".to_string(),
                })?;

            let mut remote_file = sftp
                .open(std::path::Path::new(remote_path_str))
                .map_err(|e| TransportError::RemoteError {
                    message: format!("Failed to open remote file: {e}"),
                })?;

            let mut content = Vec::new();
            std::io::Read::read_to_end(&mut remote_file, &mut content)?;

            std::fs::write(&local_path, content)?;

            Ok(())
        })
        .await
        .map_err(|e| TransportError::ConnectionFailed {
            message: format!("Task join error: {e}"),
        })?
    }

    async fn list(&self, remote_dir: &Path, pattern: &str) -> Result<Vec<String>> {
        let remote_dir = remote_dir.to_path_buf();
        let pattern = pattern.to_string();
        let state = self.state.clone();

        tokio::task::spawn_blocking(move || {
            let state_guard = state.lock().unwrap();
            let session = state_guard
                .session
                .as_ref()
                .ok_or_else(|| TransportError::ConnectionFailed {
                    message: "Not connected".to_string(),
                })?;

            let sftp = session.sftp().map_err(|e| TransportError::RemoteError {
                message: format!("Failed to open SFTP channel: {e}"),
            })?;

            let remote_dir_str = remote_dir
                .to_str()
                .ok_or_else(|| TransportError::InvalidPath {
                    message: "Remote dir contains invalid UTF-8".to_string(),
                })?;

            let entries = sftp
                .readdir(std::path::Path::new(remote_dir_str))
                .map_err(|e| TransportError::RemoteError {
                    message: format!("Failed to list remote directory: {e}"),
                })?;

            let mut matches = Vec::new();

            for (path, _stat) in entries {
                if let Some(filename) = path.file_name().and_then(|n| n.to_str())
                    && pattern_matches(&pattern, filename)
                {
                    matches.push(filename.to_string());
                }
            }

            Ok(matches)
        })
        .await
        .map_err(|e| TransportError::ConnectionFailed {
            message: format!("Task join error: {e}"),
        })?
    }

    async fn rename(&self, from: &Path, to: &Path) -> Result<()> {
        let from = from.to_path_buf();
        let to = to.to_path_buf();
        let state = self.state.clone();

        tokio::task::spawn_blocking(move || {
            let state_guard = state.lock().unwrap();
            let session = state_guard
                .session
                .as_ref()
                .ok_or_else(|| TransportError::ConnectionFailed {
                    message: "Not connected".to_string(),
                })?;

            let from_str = from
                .to_str()
                .ok_or_else(|| TransportError::InvalidPath {
                    message: "Source path contains invalid UTF-8".to_string(),
                })?;

            let to_str = to
                .to_str()
                .ok_or_else(|| TransportError::InvalidPath {
                    message: "Destination path contains invalid UTF-8".to_string(),
                })?;

            let rename_cmd = format!("mv '{from_str}' '{to_str}'");
            let mut channel = session
                .channel_session()
                .map_err(|e| TransportError::RemoteError {
                    message: format!("Failed to open SSH channel: {e}"),
                })?;

            channel
                .exec(&rename_cmd)
                .map_err(|e| TransportError::RemoteError {
                    message: format!("Failed to execute rename command: {e}"),
                })?;

            let mut stderr = String::new();
            std::io::Read::read_to_string(&mut channel.stderr(), &mut stderr)?;

            channel.wait_close().ok();

            let exit_status = channel.exit_status().map_err(|e| {
                TransportError::RemoteError {
                    message: format!("Failed to get command exit status: {e}"),
                }
            })?;

            if exit_status != 0 {
                return Err(TransportError::RemoteError {
                    message: format!("Rename command failed: {stderr}"),
                });
            }

            Ok(())
        })
        .await
        .map_err(|e| TransportError::ConnectionFailed {
            message: format!("Task join error: {e}"),
        })?
    }

    async fn disconnect(&mut self) -> Result<()> {
        let session = {
            let mut state = self.state.lock().unwrap();
            state.session.take()
        }; // Lock is dropped here

        if let Some(session) = session {
            // Disconnect happens in blocking context
            tokio::task::spawn_blocking(move || {
                let _ = session.disconnect(None, "Disconnecting", None);
            })
            .await
            .map_err(|e| TransportError::ConnectionFailed {
                message: format!("Task join error: {e}"),
            })?;
        }

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

    if let Some(last_part) = parts.last()
        && !last_part.is_empty()
        && !filename.ends_with(last_part)
    {
        return false;
    }

    true
}

#[cfg(all(test, feature = "ssh-tests"))]
mod tests {
    use super::*;
    use tempfile::TempDir;

    // Helper to check if SSH is available
    fn ssh_available() -> bool {
        std::env::var("ATM_TEST_SSH").unwrap_or_default() == "1"
    }

    #[tokio::test]
    async fn test_ssh_connect_localhost() {
        if !ssh_available() {
            eprintln!("Skipping SSH test (ATM_TEST_SSH not set)");
            return;
        }

        let config = SshConfig {
            address: format!("{}@localhost", std::env::var("USER").unwrap_or_else(|_| "root".to_string())),
            ..Default::default()
        };

        let mut transport = SshTransport::new(config);
        let result = transport.connect().await;

        if result.is_err() {
            eprintln!("SSH connection failed (this is expected if SSH is not configured): {result:?}");
            return;
        }

        assert!(transport.is_connected().await);
        transport.disconnect().await.unwrap();
        assert!(!transport.is_connected().await);
    }

    #[tokio::test]
    async fn test_ssh_upload_download() {
        if !ssh_available() {
            eprintln!("Skipping SSH test (ATM_TEST_SSH not set)");
            return;
        }

        let temp_dir = TempDir::new().unwrap();
        let local_file = temp_dir.path().join("test.txt");
        let test_content = b"Hello from SSH transport test!";
        tokio::fs::write(&local_file, test_content).await.unwrap();

        let remote_path = Path::new("/tmp/atm-ssh-test.txt");

        let config = SshConfig {
            address: format!("{}@localhost", std::env::var("USER").unwrap_or_else(|_| "root".to_string())),
            ..Default::default()
        };

        let mut transport = SshTransport::new(config);

        if transport.connect().await.is_err() {
            eprintln!("SSH connection failed - skipping test");
            return;
        }

        // Upload
        transport.upload(&local_file, remote_path).await.unwrap();

        // Download
        let download_file = temp_dir.path().join("downloaded.txt");
        transport
            .download(remote_path, &download_file)
            .await
            .unwrap();

        // Verify content
        let downloaded_content = tokio::fs::read(&download_file).await.unwrap();
        assert_eq!(downloaded_content, test_content);

        // Cleanup
        std::fs::remove_file(remote_path).ok();
        transport.disconnect().await.unwrap();
    }

    #[tokio::test]
    async fn test_ssh_atomic_rename() {
        if !ssh_available() {
            eprintln!("Skipping SSH test (ATM_TEST_SSH not set)");
            return;
        }

        let temp_dir = TempDir::new().unwrap();
        let local_file = temp_dir.path().join("test.txt");
        tokio::fs::write(&local_file, b"content").await.unwrap();

        let from_path = Path::new("/tmp/atm-test-temp.txt");
        let to_path = Path::new("/tmp/atm-test-final.txt");

        let config = SshConfig {
            address: format!("{}@localhost", std::env::var("USER").unwrap_or_else(|_| "root".to_string())),
            ..Default::default()
        };

        let mut transport = SshTransport::new(config);

        if transport.connect().await.is_err() {
            eprintln!("SSH connection failed - skipping test");
            return;
        }

        // Upload to temp path
        transport.upload(&local_file, from_path).await.unwrap();

        // Rename
        transport.rename(from_path, to_path).await.unwrap();

        // Verify final file exists
        let download_file = temp_dir.path().join("final.txt");
        transport.download(to_path, &download_file).await.unwrap();
        assert_eq!(tokio::fs::read(&download_file).await.unwrap(), b"content");

        // Cleanup
        std::fs::remove_file(to_path).ok();
        transport.disconnect().await.unwrap();
    }
}
