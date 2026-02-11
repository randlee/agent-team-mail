//! File locking with backoff retry

use crate::io::error::InboxError;
use std::fs::File;
use std::path::Path;
use std::time::Duration;

#[cfg(unix)]
use std::os::unix::io::AsRawFd;

/// File lock guard that automatically releases on drop
pub struct FileLock {
    #[allow(dead_code)]
    file: File,
    #[cfg(unix)]
    fd: i32,
}

impl Drop for FileLock {
    fn drop(&mut self) {
        #[cfg(unix)]
        {
            // Release the lock
            unsafe {
                libc::flock(self.fd, libc::LOCK_UN);
            }
        }
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
pub fn acquire_lock(path: &Path, max_retries: u32) -> Result<FileLock, InboxError> {
    #[cfg(unix)]
    {
        unix_acquire_lock(path, max_retries)
    }

    #[cfg(not(unix))]
    {
        windows_acquire_lock(path, max_retries)
    }
}

#[cfg(unix)]
fn unix_acquire_lock(path: &Path, max_retries: u32) -> Result<FileLock, InboxError> {
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

    let fd = file.as_raw_fd();

    // Try to acquire lock with exponential backoff
    for attempt in 0..=max_retries {
        let result = unsafe { libc::flock(fd, libc::LOCK_EX | libc::LOCK_NB) };

        if result == 0 {
            // Lock acquired successfully
            return Ok(FileLock { file, fd });
        }

        let err = std::io::Error::last_os_error();
        let would_block = err.raw_os_error() == Some(libc::EWOULDBLOCK)
            || err.raw_os_error() == Some(libc::EAGAIN);

        if !would_block {
            // Some other error occurred
            return Err(InboxError::Io {
                path: path.to_path_buf(),
                source: err,
            });
        }

        // EWOULDBLOCK - someone else has the lock
        if attempt < max_retries {
            // Exponential backoff: 50ms, 100ms, 200ms, 400ms, 800ms
            let wait_ms = 50u64 * (1 << attempt);
            std::thread::sleep(Duration::from_millis(wait_ms));
        }
    }

    Err(InboxError::LockTimeout {
        path: path.to_path_buf(),
        retries: max_retries,
    })
}

#[cfg(not(unix))]
fn windows_acquire_lock(path: &Path, max_retries: u32) -> Result<FileLock, InboxError> {
    use std::fs::OpenOptions;

    // Windows doesn't have flock, so we use file creation as a lock mechanism
    // This is a simplified implementation
    for attempt in 0..=max_retries {
        match OpenOptions::new()
            .read(true)
            .write(true)
            .create_new(true)
            .open(path)
        {
            Ok(file) => {
                return Ok(FileLock { file });
            }
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
                // Lock file exists, retry with backoff
                if attempt < max_retries {
                    let wait_ms = 50u64 * (1 << attempt);
                    std::thread::sleep(Duration::from_millis(wait_ms));
                }
            }
            Err(e) => {
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
