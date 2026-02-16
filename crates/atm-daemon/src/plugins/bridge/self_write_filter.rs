//! Self-write filter to prevent feedback loops
//!
//! When the bridge writes a file, it needs to filter out the subsequent
//! watcher event to avoid an infinite loop (write → watch → write → ...).

use std::collections::HashMap;
use std::path::PathBuf;
use std::time::{Duration, Instant};

/// Self-write filter with TTL
///
/// Tracks paths recently written by the bridge with an expiration time.
/// Used to filter out watcher events triggered by the bridge's own writes.
#[derive(Debug)]
pub struct SelfWriteFilter {
    /// Map of path to expiration time
    entries: HashMap<PathBuf, Instant>,

    /// Time-to-live for entries (how long to filter events after a write)
    ttl: Duration,
}

impl SelfWriteFilter {
    /// Create a new filter with the specified TTL
    pub fn new(ttl: Duration) -> Self {
        Self {
            entries: HashMap::new(),
            ttl,
        }
    }

    /// Register a path as recently written
    ///
    /// Events for this path will be filtered for the TTL duration.
    pub fn register(&mut self, path: PathBuf) {
        let expiration = Instant::now() + self.ttl;
        self.entries.insert(path, expiration);
    }

    /// Check if an event for this path should be filtered
    ///
    /// Returns `true` if the path was recently written by the bridge.
    /// Automatically cleans up expired entries.
    pub fn should_filter(&mut self, path: &PathBuf) -> bool {
        let now = Instant::now();

        // Check if path is registered
        if let Some(&expiration) = self.entries.get(path) {
            if now < expiration {
                // Still within TTL - filter this event
                return true;
            }
            // Expired - remove and don't filter
            self.entries.remove(path);
        }

        false
    }

    /// Clean up all expired entries
    ///
    /// Called periodically to prevent memory growth.
    pub fn cleanup_expired(&mut self) {
        let now = Instant::now();
        self.entries.retain(|_, &mut expiration| now < expiration);
    }

    /// Get the number of entries (for testing)
    #[cfg(test)]
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// Check if filter is empty (for testing)
    #[cfg(test)]
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }
}

impl Default for SelfWriteFilter {
    fn default() -> Self {
        // Default TTL: 5 seconds
        Self::new(Duration::from_secs(5))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::thread;

    #[test]
    fn test_filter_new() {
        let filter = SelfWriteFilter::new(Duration::from_secs(5));
        assert!(filter.is_empty());
    }

    #[test]
    fn test_filter_register_and_check() {
        let mut filter = SelfWriteFilter::new(Duration::from_secs(5));
        let path = PathBuf::from("/tmp/test.json");

        // Path not registered - should not filter
        assert!(!filter.should_filter(&path));

        // Register path
        filter.register(path.clone());

        // Should filter now
        assert!(filter.should_filter(&path));
    }

    #[test]
    fn test_filter_ttl_expiration() {
        let mut filter = SelfWriteFilter::new(Duration::from_millis(100));
        let path = PathBuf::from("/tmp/test.json");

        // Register path
        filter.register(path.clone());
        assert!(filter.should_filter(&path));

        // Wait for TTL to expire
        thread::sleep(Duration::from_millis(150));

        // Should not filter anymore (expired)
        assert!(!filter.should_filter(&path));
    }

    #[test]
    fn test_filter_multiple_paths() {
        let mut filter = SelfWriteFilter::new(Duration::from_secs(5));
        let path1 = PathBuf::from("/tmp/file1.json");
        let path2 = PathBuf::from("/tmp/file2.json");
        let path3 = PathBuf::from("/tmp/file3.json");

        // Register path1 and path2
        filter.register(path1.clone());
        filter.register(path2.clone());

        // Should filter path1 and path2, but not path3
        assert!(filter.should_filter(&path1));
        assert!(filter.should_filter(&path2));
        assert!(!filter.should_filter(&path3));
    }

    #[test]
    fn test_filter_cleanup_expired() {
        let mut filter = SelfWriteFilter::new(Duration::from_millis(100));
        let path1 = PathBuf::from("/tmp/file1.json");
        let path2 = PathBuf::from("/tmp/file2.json");

        // Register both paths
        filter.register(path1.clone());
        filter.register(path2.clone());
        assert_eq!(filter.len(), 2);

        // Wait for TTL to expire
        thread::sleep(Duration::from_millis(150));

        // Cleanup expired entries
        filter.cleanup_expired();
        assert_eq!(filter.len(), 0);
    }

    #[test]
    fn test_filter_reregister() {
        let mut filter = SelfWriteFilter::new(Duration::from_millis(500));
        let path = PathBuf::from("/tmp/test.json");

        // Register path
        filter.register(path.clone());
        assert!(filter.should_filter(&path));

        // Wait less than TTL
        thread::sleep(Duration::from_millis(150));

        // Re-register (extends TTL)
        filter.register(path.clone());

        // Wait another 200ms (total 350ms from first register, but only 200ms from second)
        thread::sleep(Duration::from_millis(200));

        // Should still filter (TTL extended by second register, 300ms margin)
        assert!(filter.should_filter(&path));
    }

    #[test]
    fn test_filter_default_ttl() {
        let filter = SelfWriteFilter::default();
        assert_eq!(filter.ttl, Duration::from_secs(5));
    }

    #[test]
    fn test_filter_automatic_cleanup_on_check() {
        let mut filter = SelfWriteFilter::new(Duration::from_millis(100));
        let path = PathBuf::from("/tmp/test.json");

        filter.register(path.clone());
        assert_eq!(filter.len(), 1);

        // Wait for expiration
        thread::sleep(Duration::from_millis(150));

        // Checking should_filter removes expired entry
        assert!(!filter.should_filter(&path));
        assert_eq!(filter.len(), 0);
    }
}
