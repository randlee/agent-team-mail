//! File locking with backoff retry

use crate::io::error::InboxError;
use fs2::FileExt;
use std::fs::File;
use std::path::Path;
use std::time::Duration;

/// File lock guard that automatically releases on drop
pub struct FileLock {
    file: File,
}

impl Drop for FileLock {
    fn drop(&mut self) {
        // Release the lock - fs2 handles platform-specific unlock
        // Use fully qualified syntax to avoid unstable name collision warning
        let _ = FileExt::unlock(&self.file);
    }
}

/// Acquire an exclusive lock on a file with backoff retry
///
/// Attempts to acquire a lock with exponential backoff:
/// - Attempt 0: No wait
/// - Attempt 1: 50ms wait
/// - Attempt 2: 100ms wait
/// - Attempt 3: 200ms wait
/// - Attempt 4: 400ms wait
/// - Attempt 5: 800ms wait
///
/// # Arguments
///
/// * `path` - Path to the file to lock
/// * `max_retries` - Maximum number of retry attempts (default: 5)
///
/// # Returns
///
/// Returns a `FileLock` guard that automatically releases the lock on drop.
/// Returns `InboxError::LockTimeout` if unable to acquire lock after all retries.
///
/// # Implementation
///
/// Uses the `fs2` crate for cross-platform file locking:
/// - Unix: flock()
/// - Windows: LockFileEx()
pub fn acquire_lock(path: &Path, max_retries: u32) -> Result<FileLock, InboxError> {
    use std::fs::OpenOptions;

    // Open (or create) the lock file
    let file = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(path)
        .map_err(|e| InboxError::Io {
            path: path.to_path_buf(),
            source: e,
        })?;

    // Try to acquire lock with exponential backoff
    for attempt in 0..=max_retries {
        match file.try_lock_exclusive() {
            Ok(()) => {
                // Lock acquired successfully
                return Ok(FileLock { file });
            }
            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                // Someone else has the lock, retry with backoff
                if attempt < max_retries {
                    // Exponential backoff: 50ms, 100ms, 200ms, 400ms, 800ms
                    let wait_ms = 50u64 * (1 << attempt);
                    std::thread::sleep(Duration::from_millis(wait_ms));
                }
            }
            Err(e) => {
                // Some other error occurred
                return Err(InboxError::Io {
                    path: path.to_path_buf(),
                    source: e,
                });
            }
        }
    }

    Err(InboxError::LockTimeout {
        path: path.to_path_buf(),
        retries: max_retries,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, Barrier};
    use std::thread;
    use tempfile::TempDir;

    #[test]
    fn test_acquire_lock_success() {
        let temp_dir = TempDir::new().unwrap();
        let lock_path = temp_dir.path().join("test.lock");

        let lock = acquire_lock(&lock_path, 5).unwrap();
        assert!(lock_path.exists());
        drop(lock);
    }

    #[test]
    fn test_acquire_lock_sequential() {
        let temp_dir = TempDir::new().unwrap();
        let lock_path = temp_dir.path().join("test.lock");

        // First lock
        {
            let _lock1 = acquire_lock(&lock_path, 5).unwrap();
            // Lock is held
        } // Lock released

        // Second lock should succeed
        let _lock2 = acquire_lock(&lock_path, 5).unwrap();
    }

    #[test]
    fn test_acquire_lock_concurrent() {
        let temp_dir = TempDir::new().unwrap();
        let lock_path = Arc::new(temp_dir.path().join("test.lock"));
        let barrier = Arc::new(Barrier::new(2));

        let lock_path_clone = Arc::clone(&lock_path);
        let barrier_clone = Arc::clone(&barrier);

        // Thread 1: Hold lock for a short time
        let handle1 = thread::spawn(move || {
            let _lock = acquire_lock(&lock_path_clone, 5).unwrap();
            barrier_clone.wait();
            thread::sleep(Duration::from_millis(100));
        });

        // Thread 2: Try to acquire after thread 1
        let handle2 = thread::spawn(move || {
            barrier.wait();
            // Should succeed after thread 1 releases (with backoff)
            let result = acquire_lock(&lock_path, 5);
            result.is_ok()
        });

        handle1.join().unwrap();
        let success = handle2.join().unwrap();
        assert!(success);
    }

    #[test]
    fn test_acquire_lock_timeout() {
        let temp_dir = TempDir::new().unwrap();
        let lock_path = Arc::new(temp_dir.path().join("test.lock"));

        let lock_path_clone = Arc::clone(&lock_path);

        // Thread 1: Hold lock for longer than retry period
        let handle1 = thread::spawn(move || {
            let _lock = acquire_lock(&lock_path_clone, 5).unwrap();
            thread::sleep(Duration::from_secs(2)); // Hold longer than retry timeout
        });

        // Give thread 1 time to acquire lock
        thread::sleep(Duration::from_millis(50));

        // Thread 2: Should timeout
        let result = acquire_lock(&lock_path, 3); // Fewer retries for faster test
        assert!(matches!(result, Err(InboxError::LockTimeout { .. })));

        handle1.join().unwrap();
    }

    #[test]
    fn test_lock_auto_release() {
        let temp_dir = TempDir::new().unwrap();
        let lock_path = temp_dir.path().join("test.lock");

        {
            let _lock = acquire_lock(&lock_path, 5).unwrap();
            // Lock held here
        } // Lock automatically released on drop

        // Should be able to acquire again immediately
        let _lock2 = acquire_lock(&lock_path, 5).unwrap();
    }
}
