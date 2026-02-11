//! Platform-specific atomic swap operations

use crate::io::error::InboxError;
use std::path::Path;

#[cfg(target_os = "macos")]
use std::ffi::CString;

#[cfg(target_os = "linux")]
use std::ffi::CString;

/// Atomically swap two files using platform-specific syscalls
///
/// This operation exchanges the contents of `path1` and `path2` atomically.
/// After the swap, `path1` contains what was in `path2` and vice versa.
///
/// # Platform Support
///
/// - **macOS**: Uses `renamex_np(from, to, RENAME_SWAP)` (macOS 10.12+)
/// - **Linux**: Uses `renameat2(AT_FDCWD, from, AT_FDCWD, to, RENAME_EXCHANGE)` (kernel 3.15+)
/// - **Windows**: Best-effort implementation using temporary file
///
/// # Errors
///
/// Returns `InboxError::AtomicSwapUnsupported` if the platform doesn't support atomic swap,
/// or `InboxError::Io` if the underlying syscall fails.
pub fn atomic_swap(path1: &Path, path2: &Path) -> Result<(), InboxError> {
    #[cfg(target_os = "macos")]
    {
        macos_atomic_swap(path1, path2)
    }

    #[cfg(target_os = "linux")]
    {
        linux_atomic_swap(path1, path2)
    }

    #[cfg(not(any(target_os = "macos", target_os = "linux")))]
    {
        windows_best_effort_swap(path1, path2)
    }
}

#[cfg(target_os = "macos")]
fn macos_atomic_swap(path1: &Path, path2: &Path) -> Result<(), InboxError> {
    use libc::{c_char, c_int, c_uint};

    // renamex_np flags
    const RENAME_SWAP: c_uint = 0x00000002;

    unsafe extern "C" {
        fn renamex_np(from: *const c_char, to: *const c_char, flags: c_uint) -> c_int;
    }

    let path1_cstr = CString::new(path1.as_os_str().to_string_lossy().as_bytes())
        .map_err(|_| InboxError::InvalidPath {
            path: path1.to_path_buf(),
        })?;

    let path2_cstr = CString::new(path2.as_os_str().to_string_lossy().as_bytes())
        .map_err(|_| InboxError::InvalidPath {
            path: path2.to_path_buf(),
        })?;

    let result = unsafe { renamex_np(path1_cstr.as_ptr(), path2_cstr.as_ptr(), RENAME_SWAP) };

    if result == 0 {
        Ok(())
    } else {
        Err(InboxError::Io {
            path: path1.to_path_buf(),
            source: std::io::Error::last_os_error(),
        })
    }
}

#[cfg(target_os = "linux")]
fn linux_atomic_swap(path1: &Path, path2: &Path) -> Result<(), InboxError> {
    use libc::{c_char, c_int, AT_FDCWD};

    // renameat2 flags
    const RENAME_EXCHANGE: c_int = 1 << 1;

    unsafe extern "C" {
        fn renameat2(
            olddirfd: c_int,
            oldpath: *const c_char,
            newdirfd: c_int,
            newpath: *const c_char,
            flags: c_int,
        ) -> c_int;
    }

    let path1_cstr = CString::new(path1.as_os_str().to_string_lossy().as_bytes())
        .map_err(|_| InboxError::InvalidPath {
            path: path1.to_path_buf(),
        })?;

    let path2_cstr = CString::new(path2.as_os_str().to_string_lossy().as_bytes())
        .map_err(|_| InboxError::InvalidPath {
            path: path2.to_path_buf(),
        })?;

    let result = unsafe {
        renameat2(
            AT_FDCWD,
            path1_cstr.as_ptr(),
            AT_FDCWD,
            path2_cstr.as_ptr(),
            RENAME_EXCHANGE,
        )
    };

    if result == 0 {
        Ok(())
    } else {
        Err(InboxError::Io {
            path: path1.to_path_buf(),
            source: std::io::Error::last_os_error(),
        })
    }
}

#[cfg(not(any(target_os = "macos", target_os = "linux")))]
fn windows_best_effort_swap(path1: &Path, path2: &Path) -> Result<(), InboxError> {
    use std::fs;

    // Windows doesn't have native atomic swap, so we use a best-effort approach
    // with a temporary file. This is not truly atomic but should work for most cases.

    let temp_path = path1.with_extension("swap_temp");

    // Move path1 to temp
    fs::rename(path1, &temp_path).map_err(|e| InboxError::Io {
        path: path1.to_path_buf(),
        source: e,
    })?;

    // Move path2 to path1
    fs::rename(path2, path1).map_err(|e| {
        // Try to restore path1 from temp
        let _ = fs::rename(&temp_path, path1);
        InboxError::Io {
            path: path2.to_path_buf(),
            source: e,
        }
    })?;

    // Move temp to path2
    fs::rename(&temp_path, path2).map_err(|e| InboxError::Io {
        path: temp_path.clone(),
        source: e,
    })?;

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::TempDir;

    #[test]
    fn test_atomic_swap_basic() {
        let temp_dir = TempDir::new().unwrap();
        let path1 = temp_dir.path().join("file1.txt");
        let path2 = temp_dir.path().join("file2.txt");

        fs::write(&path1, b"content 1").unwrap();
        fs::write(&path2, b"content 2").unwrap();

        atomic_swap(&path1, &path2).unwrap();

        let content1 = fs::read(&path1).unwrap();
        let content2 = fs::read(&path2).unwrap();

        assert_eq!(content1, b"content 2");
        assert_eq!(content2, b"content 1");
    }

    #[test]
    fn test_atomic_swap_json_files() {
        let temp_dir = TempDir::new().unwrap();
        let inbox = temp_dir.path().join("inbox.json");
        let tmp = temp_dir.path().join("inbox.tmp");

        let inbox_content = r#"[{"from":"team-lead","text":"old","timestamp":"2026-02-11T14:30:00Z","read":false}]"#;
        let tmp_content = r#"[{"from":"team-lead","text":"old","timestamp":"2026-02-11T14:30:00Z","read":false},{"from":"ci-agent","text":"new","timestamp":"2026-02-11T14:31:00Z","read":false}]"#;

        fs::write(&inbox, inbox_content).unwrap();
        fs::write(&tmp, tmp_content).unwrap();

        atomic_swap(&inbox, &tmp).unwrap();

        let inbox_after = fs::read_to_string(&inbox).unwrap();
        let tmp_after = fs::read_to_string(&tmp).unwrap();

        assert_eq!(inbox_after, tmp_content);
        assert_eq!(tmp_after, inbox_content);
    }

    #[test]
    fn test_atomic_swap_nonexistent_file() {
        let temp_dir = TempDir::new().unwrap();
        let path1 = temp_dir.path().join("file1.txt");
        let path2 = temp_dir.path().join("file2.txt");

        fs::write(&path1, b"content 1").unwrap();
        // path2 doesn't exist

        let result = atomic_swap(&path1, &path2);
        assert!(result.is_err());
    }

    #[test]
    fn test_atomic_swap_empty_files() {
        let temp_dir = TempDir::new().unwrap();
        let path1 = temp_dir.path().join("file1.txt");
        let path2 = temp_dir.path().join("file2.txt");

        fs::write(&path1, b"").unwrap();
        fs::write(&path2, b"").unwrap();

        atomic_swap(&path1, &path2).unwrap();

        let content1 = fs::read(&path1).unwrap();
        let content2 = fs::read(&path2).unwrap();

        assert_eq!(content1, b"");
        assert_eq!(content2, b"");
    }
}
