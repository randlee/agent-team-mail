//! Response capture via log file tailing
//!
//! Captures worker output by tailing log files written by worker backends.
//! CRITICAL: Requires explicit writer contract — backend must tee output to log file.

use crate::plugin::PluginError;
use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::path::Path;
use std::time::{Duration, Instant};
use tracing::{debug, warn};

/// Response capture configuration
#[derive(Debug, Clone)]
pub struct CaptureConfig {
    /// Maximum time to wait for response (milliseconds)
    pub timeout_ms: u64,
    /// Poll interval for log file (milliseconds)
    pub poll_interval_ms: u64,
    /// Maximum response size (bytes)
    pub max_response_bytes: usize,
}

impl Default for CaptureConfig {
    fn default() -> Self {
        Self {
            timeout_ms: 60_000,         // 60 seconds
            poll_interval_ms: 100,       // 100ms polling
            max_response_bytes: 1_048_576, // 1MB
        }
    }
}

/// Response capture result
#[derive(Debug, Clone)]
pub struct CapturedResponse {
    /// Raw output from worker
    pub raw_output: String,
    /// Parsed response text (after stripping prompt echo)
    pub response_text: String,
}

/// Log file tailer for capturing worker responses
pub struct LogTailer {
    config: CaptureConfig,
}

impl LogTailer {
    /// Create a new log tailer with default config
    pub fn new() -> Self {
        Self {
            config: CaptureConfig::default(),
        }
    }

    /// Create a new log tailer with custom config
    ///
    /// # Arguments
    ///
    /// * `config` - Capture configuration
    pub fn with_config(config: CaptureConfig) -> Self {
        Self { config }
    }

    /// Tail a log file for new output after a message is sent
    ///
    /// Opens the log file, seeks to the end, waits for new content, and captures
    /// output until timeout or max size reached.
    ///
    /// # Arguments
    ///
    /// * `log_path` - Path to worker log file
    /// * `prompt_text` - The prompt that was sent (for echo stripping)
    ///
    /// # Returns
    ///
    /// Captured response with raw output and parsed text
    ///
    /// # Errors
    ///
    /// Returns `PluginError` if file doesn't exist, I/O fails, or timeout reached
    pub fn capture_response(
        &self,
        log_path: &Path,
        prompt_text: &str,
    ) -> Result<CapturedResponse, PluginError> {
        // Open log file
        let mut file = File::open(log_path).map_err(|e| PluginError::Runtime {
            message: format!("Failed to open log file: {}", log_path.display()),
            source: Some(Box::new(e)),
        })?;

        // Seek to end to capture only new output
        let start_pos = file.seek(SeekFrom::End(0)).map_err(|e| PluginError::Runtime {
            message: format!("Failed to seek log file: {e}"),
            source: Some(Box::new(e)),
        })?;

        debug!(
            "Tailing log file {} from position {}",
            log_path.display(),
            start_pos
        );

        // Poll for new content
        let start_time = Instant::now();
        let timeout = Duration::from_millis(self.config.timeout_ms);
        let poll_interval = Duration::from_millis(self.config.poll_interval_ms);

        let mut buffer = Vec::new();
        let mut last_size = start_pos;

        loop {
            // Check timeout
            if start_time.elapsed() > timeout {
                if buffer.is_empty() {
                    return Err(PluginError::Runtime {
                        message: format!(
                            "Timeout waiting for response after {} ms",
                            self.config.timeout_ms
                        ),
                        source: None,
                    });
                } else {
                    warn!(
                        "Response capture timed out but got {} bytes, returning partial",
                        buffer.len()
                    );
                    break;
                }
            }

            // Check current file size
            let current_size = file.metadata()
                .map_err(|e| PluginError::Runtime {
                    message: format!("Failed to read log file metadata: {e}"),
                    source: Some(Box::new(e)),
                })?
                .len();

            if current_size > last_size {
                // New content available
                let to_read = (current_size - last_size) as usize;
                let read_size = to_read.min(self.config.max_response_bytes - buffer.len());

                let mut chunk = vec![0u8; read_size];
                file.read_exact(&mut chunk).map_err(|e| PluginError::Runtime {
                    message: format!("Failed to read log file: {e}"),
                    source: Some(Box::new(e)),
                })?;

                buffer.extend_from_slice(&chunk);
                last_size = file.stream_position().map_err(|e| PluginError::Runtime {
                    message: format!("Failed to get file position: {e}"),
                    source: Some(Box::new(e)),
                })?;

                debug!("Captured {} bytes (total {} bytes)", chunk.len(), buffer.len());

                // Check if we've hit max size
                if buffer.len() >= self.config.max_response_bytes {
                    warn!(
                        "Response exceeded max size ({} bytes), truncating",
                        self.config.max_response_bytes
                    );
                    break;
                }

                // Heuristic: if we see a prompt-like pattern, assume response is complete
                // This is backend-specific and should be refined
                let text = String::from_utf8_lossy(&buffer);
                if self.looks_like_complete_response(&text) {
                    debug!("Detected complete response pattern, stopping capture");
                    break;
                }
            }

            // Sleep before next poll
            std::thread::sleep(poll_interval);
        }

        let raw_output = String::from_utf8_lossy(&buffer).to_string();
        let response_text = self.strip_prompt_echo(&raw_output, prompt_text);

        Ok(CapturedResponse {
            raw_output,
            response_text,
        })
    }

    /// Strip the prompt echo from the response
    ///
    /// When a prompt is sent via tmux send-keys, it may be echoed in the log.
    /// This removes the echo to get the actual response.
    ///
    /// # Arguments
    ///
    /// * `output` - Raw output captured from log
    /// * `prompt` - The prompt that was sent
    fn strip_prompt_echo(&self, output: &str, prompt: &str) -> String {
        // Simple approach: if output starts with prompt, strip it
        if let Some(stripped) = output.strip_prefix(prompt) {
            stripped.trim().to_string()
        } else {
            // Try line-by-line stripping (prompt may be on first line)
            let lines: Vec<&str> = output.lines().collect();
            if !lines.is_empty() && lines[0].contains(prompt) {
                lines[1..].join("\n").trim().to_string()
            } else {
                output.trim().to_string()
            }
        }
    }

    /// Heuristic to detect if response looks complete
    ///
    /// This is backend-specific. For Codex, we might look for specific markers.
    /// For now, use a simple heuristic: multiple lines with content.
    ///
    /// # Arguments
    ///
    /// * `text` - Captured text so far
    fn looks_like_complete_response(&self, text: &str) -> bool {
        // Simple heuristic: at least 3 non-empty lines suggest complete response
        let non_empty_lines = text.lines().filter(|l| !l.trim().is_empty()).count();
        non_empty_lines >= 3
    }
}

impl Default for LogTailer {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use tempfile::NamedTempFile;

    #[test]
    fn test_strip_prompt_echo_exact_prefix() {
        let tailer = LogTailer::new();
        let prompt = "Hello, agent!";
        let output = "Hello, agent!\nThis is the response.";

        let stripped = tailer.strip_prompt_echo(output, prompt);
        assert_eq!(stripped, "This is the response.");
    }

    #[test]
    fn test_strip_prompt_echo_line_by_line() {
        let tailer = LogTailer::new();
        let prompt = "What is 2+2?";
        let output = "User: What is 2+2?\nThe answer is 4.";

        let stripped = tailer.strip_prompt_echo(output, prompt);
        assert_eq!(stripped, "The answer is 4.");
    }

    #[test]
    fn test_strip_prompt_echo_no_echo() {
        let tailer = LogTailer::new();
        let prompt = "Calculate factorial of 5";
        let output = "The factorial of 5 is 120.";

        let stripped = tailer.strip_prompt_echo(output, prompt);
        assert_eq!(stripped, "The factorial of 5 is 120.");
    }

    #[test]
    fn test_looks_like_complete_response() {
        let tailer = LogTailer::new();

        // Incomplete
        assert!(!tailer.looks_like_complete_response("Line 1\n"));
        assert!(!tailer.looks_like_complete_response("Line 1\nLine 2\n"));

        // Complete
        assert!(tailer.looks_like_complete_response("Line 1\nLine 2\nLine 3\n"));
        assert!(tailer.looks_like_complete_response("A\nB\nC\nD\n"));
    }

    #[test]
    fn test_capture_response_from_file() {
        // Create a temp log file
        let mut temp_file = NamedTempFile::new().unwrap();
        let log_path = temp_file.path().to_path_buf();

        // Write initial content
        writeln!(temp_file, "Worker starting...").unwrap();
        temp_file.flush().unwrap();

        // Spawn a thread to write response after a delay
        let log_path_clone = log_path.clone();
        std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(50));
            let mut file = std::fs::OpenOptions::new()
                .append(true)
                .open(&log_path_clone)
                .unwrap();
            writeln!(file, "Received prompt").unwrap();
            writeln!(file, "Processing...").unwrap();
            writeln!(file, "Response complete.").unwrap();
            file.flush().unwrap();
        });

        // Capture response
        let tailer = LogTailer::with_config(CaptureConfig {
            timeout_ms: 2000,
            poll_interval_ms: 10,
            max_response_bytes: 1024,
        });

        let result = tailer.capture_response(&log_path, "test prompt");
        assert!(result.is_ok());

        let captured = result.unwrap();
        assert!(!captured.raw_output.is_empty());
        assert!(captured.raw_output.contains("Received prompt"));
        assert!(captured.raw_output.contains("Response complete"));
    }
}
