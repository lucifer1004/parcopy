//! Builder API for ergonomic copying operations.
//!
//! The builder pattern provides a fluent interface for configuring and executing
//! copy operations. This is often more convenient than manually constructing
//! [`CopyOptions`].
//!
//! # Examples
//!
//! ## Basic Usage
//!
//! ```no_run
//! use parcopy::CopyBuilder;
//!
//! // Simple copy with defaults
//! let stats = CopyBuilder::new("src", "dst").run()?;
//! println!("Copied {} files", stats.files_copied);
//! # Ok::<(), parcopy::Error>(())
//! ```
//!
//! ## With Options
//!
//! ```no_run
//! use parcopy::CopyBuilder;
//!
//! let stats = CopyBuilder::new("src", "dst")
//!     .parallel(8)           // Use 8 threads
//!     .overwrite()           // Overwrite existing files
//!     .no_timestamps()       // Don't preserve timestamps
//!     .run()?;
//! # Ok::<(), parcopy::Error>(())
//! ```
//!
//! ## Incremental Copy
//!
//! ```no_run
//! use parcopy::CopyBuilder;
//!
//! // Only copy files newer than destination
//! let stats = CopyBuilder::new("src", "dst")
//!     .update_newer()
//!     .run()?;
//!
//! if stats.files_skipped > 0 {
//!     println!("Skipped {} up-to-date files", stats.files_skipped);
//! }
//! # Ok::<(), parcopy::Error>(())
//! ```

use crate::copy::{CopyStats, copy_dir, copy_file};
use crate::error::Result;
use crate::options::{CopyOptions, OnConflict};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::AtomicBool;

/// A builder for configuring and executing copy operations.
///
/// `CopyBuilder` provides a fluent interface that is often more ergonomic than
/// constructing [`CopyOptions`] manually. It automatically detects whether
/// the source is a file or directory and calls the appropriate function.
///
/// # Example
///
/// ```no_run
/// use parcopy::CopyBuilder;
///
/// // Copy a directory with custom options
/// let stats = CopyBuilder::new("/data/project", "/backup/project")
///     .parallel(16)
///     .update_newer()
///     .run()?;
/// # Ok::<(), parcopy::Error>(())
/// ```
#[derive(Debug, Clone)]
pub struct CopyBuilder {
    src: PathBuf,
    dst: PathBuf,
    options: CopyOptions,
}

impl CopyBuilder {
    /// Create a new `CopyBuilder` with the given source and destination paths.
    ///
    /// Uses default options (parallel=16, skip existing, preserve timestamps).
    ///
    /// # Example
    ///
    /// ```no_run
    /// use parcopy::CopyBuilder;
    ///
    /// let builder = CopyBuilder::new("src", "dst");
    /// ```
    pub fn new<P: AsRef<Path>, Q: AsRef<Path>>(src: P, dst: Q) -> Self {
        Self {
            src: src.as_ref().to_path_buf(),
            dst: dst.as_ref().to_path_buf(),
            options: CopyOptions::default(),
        }
    }

    /// Set the number of parallel threads to use.
    ///
    /// Default is 16. Set to 1 for sequential copying.
    ///
    /// # Example
    ///
    /// ```no_run
    /// use parcopy::CopyBuilder;
    ///
    /// let stats = CopyBuilder::new("src", "dst")
    ///     .parallel(4)
    ///     .run()?;
    /// # Ok::<(), parcopy::Error>(())
    /// ```
    #[must_use]
    pub fn parallel(mut self, threads: usize) -> Self {
        self.options = self.options.with_parallel(threads);
        self
    }

    /// Skip files that already exist at the destination (default behavior).
    ///
    /// # Example
    ///
    /// ```no_run
    /// use parcopy::CopyBuilder;
    ///
    /// let stats = CopyBuilder::new("src", "dst")
    ///     .skip_existing()
    ///     .run()?;
    /// # Ok::<(), parcopy::Error>(())
    /// ```
    #[must_use]
    pub fn skip_existing(mut self) -> Self {
        self.options = self.options.with_on_conflict(OnConflict::Skip);
        self
    }

    /// Overwrite existing files at the destination.
    ///
    /// # Example
    ///
    /// ```no_run
    /// use parcopy::CopyBuilder;
    ///
    /// let stats = CopyBuilder::new("src", "dst")
    ///     .overwrite()
    ///     .run()?;
    /// # Ok::<(), parcopy::Error>(())
    /// ```
    #[must_use]
    pub fn overwrite(mut self) -> Self {
        self.options = self.options.with_on_conflict(OnConflict::Overwrite);
        self
    }

    /// Only copy files that are newer than the destination.
    ///
    /// This is useful for incremental backups or syncing.
    ///
    /// # Example
    ///
    /// ```no_run
    /// use parcopy::CopyBuilder;
    ///
    /// // Sync changes - only copy modified files
    /// let stats = CopyBuilder::new("src", "dst")
    ///     .update_newer()
    ///     .run()?;
    /// # Ok::<(), parcopy::Error>(())
    /// ```
    #[must_use]
    pub fn update_newer(mut self) -> Self {
        self.options = self.options.with_on_conflict(OnConflict::UpdateNewer);
        self
    }

    /// Return an error if a destination file already exists.
    ///
    /// # Example
    ///
    /// ```no_run
    /// use parcopy::CopyBuilder;
    ///
    /// let result = CopyBuilder::new("src", "dst")
    ///     .error_on_conflict()
    ///     .run();
    ///
    /// if result.is_err() {
    ///     eprintln!("Some files already exist!");
    /// }
    /// ```
    #[must_use]
    pub fn error_on_conflict(mut self) -> Self {
        self.options = self.options.with_on_conflict(OnConflict::Error);
        self
    }

    /// Disable fsync after writing files.
    ///
    /// This improves performance but reduces durability guarantees.
    /// Files may be lost if the system crashes before the OS flushes buffers.
    ///
    /// # Example
    ///
    /// ```no_run
    /// use parcopy::CopyBuilder;
    ///
    /// let stats = CopyBuilder::new("src", "dst")
    ///     .no_fsync()
    ///     .run()?;
    /// # Ok::<(), parcopy::Error>(())
    /// ```
    #[must_use]
    pub fn no_fsync(mut self) -> Self {
        self.options = self.options.without_fsync();
        self
    }

    /// Disable timestamp preservation.
    ///
    /// By default, file modification and access times are preserved.
    ///
    /// # Example
    ///
    /// ```no_run
    /// use parcopy::CopyBuilder;
    ///
    /// let stats = CopyBuilder::new("src", "dst")
    ///     .no_timestamps()
    ///     .run()?;
    /// # Ok::<(), parcopy::Error>(())
    /// ```
    #[must_use]
    pub fn no_timestamps(mut self) -> Self {
        self.options = self.options.without_timestamps();
        self
    }

    /// Disable permission preservation.
    ///
    /// By default, file permissions are copied from source to destination.
    ///
    /// # Example
    ///
    /// ```no_run
    /// use parcopy::CopyBuilder;
    ///
    /// let stats = CopyBuilder::new("src", "dst")
    ///     .no_permissions()
    ///     .run()?;
    /// # Ok::<(), parcopy::Error>(())
    /// ```
    #[must_use]
    pub fn no_permissions(mut self) -> Self {
        self.options = self.options.without_permissions();
        self
    }

    /// Disable Windows file attribute preservation.
    ///
    /// By default on Windows, file attributes like Hidden, System, and Archive
    /// are copied from source to destination. This option disables that behavior.
    ///
    /// This option has no effect on non-Windows platforms.
    ///
    /// # Example
    ///
    /// ```no_run
    /// use parcopy::CopyBuilder;
    ///
    /// let stats = CopyBuilder::new("src", "dst")
    ///     .no_windows_attributes()
    ///     .run()?;
    /// # Ok::<(), parcopy::Error>(())
    /// ```
    #[must_use]
    pub fn no_windows_attributes(mut self) -> Self {
        self.options = self.options.without_windows_attributes();
        self
    }

    /// Limit the maximum directory depth to traverse.
    ///
    /// A depth of 0 means only copy the top-level directory contents.
    /// `None` means unlimited depth (default).
    ///
    /// # Example
    ///
    /// ```no_run
    /// use parcopy::CopyBuilder;
    ///
    /// // Only copy top 3 levels
    /// let stats = CopyBuilder::new("src", "dst")
    ///     .max_depth(3)
    ///     .run()?;
    /// # Ok::<(), parcopy::Error>(())
    /// ```
    #[must_use]
    pub fn max_depth(mut self, depth: usize) -> Self {
        self.options = self.options.with_max_depth(depth);
        self
    }

    /// Block symlinks that point outside the source directory.
    ///
    /// This is a security measure to prevent symlinks from escaping
    /// the source tree and potentially exposing sensitive data.
    ///
    /// # Example
    ///
    /// ```no_run
    /// use parcopy::CopyBuilder;
    ///
    /// let stats = CopyBuilder::new("untrusted_src", "dst")
    ///     .block_escaping_symlinks()
    ///     .run()?;
    /// # Ok::<(), parcopy::Error>(())
    /// ```
    #[must_use]
    pub fn block_escaping_symlinks(mut self) -> Self {
        self.options = self.options.with_block_escaping_symlinks();
        self
    }

    /// Set a cancellation token for cooperative cancellation.
    ///
    /// When the token is set to `true`, the copy operation stops starting new
    /// files and returns [`Error::Cancelled`](crate::Error::Cancelled) with
    /// partial statistics. In-flight files always finish to maintain atomicity.
    ///
    /// # Example
    ///
    /// ```no_run
    /// use parcopy::CopyBuilder;
    /// use std::sync::Arc;
    /// use std::sync::atomic::AtomicBool;
    ///
    /// let cancel = Arc::new(AtomicBool::new(false));
    /// // Pass clone to a signal handler or another thread
    /// let stats = CopyBuilder::new("src", "dst")
    ///     .cancel_token(cancel)
    ///     .run();
    /// ```
    #[must_use]
    pub fn cancel_token(mut self, token: Arc<AtomicBool>) -> Self {
        self.options = self.options.with_cancel_token(token);
        self
    }

    /// Set a warning handler for non-fatal issues.
    ///
    /// The handler is called with warning messages for issues like
    /// permission errors on individual files that don't stop the overall copy.
    ///
    /// # Example
    ///
    /// ```no_run
    /// use parcopy::CopyBuilder;
    ///
    /// let stats = CopyBuilder::new("src", "dst")
    ///     .on_warning(|msg| eprintln!("Warning: {}", msg))
    ///     .run()?;
    /// # Ok::<(), parcopy::Error>(())
    /// ```
    #[must_use]
    pub fn on_warning(mut self, handler: fn(&str)) -> Self {
        self.options = self.options.with_warn_handler(handler);
        self
    }

    /// Enable verbose output for detailed file operation information.
    ///
    /// The handler is called with messages for each file operation:
    /// - Copied files (src -> dst, bytes)
    /// - Skipped files
    /// - Failed files with error messages
    ///
    /// # Example
    ///
    /// ```no_run
    /// use parcopy::CopyBuilder;
    ///
    /// let stats = CopyBuilder::new("src", "dst")
    ///     .verbose(|msg| println!("{}", msg))
    ///     .run()?;
    /// # Ok::<(), parcopy::Error>(())
    /// ```
    #[must_use]
    pub fn verbose(mut self, handler: fn(&str)) -> Self {
        self.options = self.options.with_verbose_handler(handler);
        self
    }

    /// Get a reference to the current options.
    ///
    /// Useful for inspection or passing to other functions.
    pub fn options(&self) -> &CopyOptions {
        &self.options
    }

    /// Execute the copy operation.
    ///
    /// Automatically detects whether the source is a file or directory
    /// and calls the appropriate function.
    ///
    /// Returns [`CopyStats`] with information about what was copied.
    ///
    /// # Errors
    ///
    /// Returns an error if:
    /// - Source doesn't exist
    /// - Permission denied
    /// - Destination already exists (with `error_on_conflict()`)
    /// - I/O error during copy
    ///
    /// # Example
    ///
    /// ```no_run
    /// use parcopy::CopyBuilder;
    ///
    /// let stats = CopyBuilder::new("src", "dst").run()?;
    /// println!("Copied {} files ({} bytes)", stats.files_copied, stats.bytes_copied);
    /// # Ok::<(), parcopy::Error>(())
    /// ```
    pub fn run(self) -> Result<CopyStats> {
        if self.src.is_dir() {
            copy_dir(&self.src, &self.dst, &self.options)
        } else {
            let start = std::time::Instant::now();
            let copied = copy_file(&self.src, &self.dst, &self.options)?;
            let file_len = self.src.metadata().map(|m| m.len()).unwrap_or(0);

            Ok(CopyStats {
                files_copied: if copied { 1 } else { 0 },
                files_skipped: if copied { 0 } else { 1 },
                symlinks_copied: 0,
                symlinks_skipped: 0,
                dirs_created: 0,
                bytes_copied: if copied { file_len } else { 0 },
                duration: start.elapsed(),
            })
        }
    }

    /// Execute the copy operation for a directory only.
    ///
    /// Returns an error if the source is not a directory.
    ///
    /// # Example
    ///
    /// ```no_run
    /// use parcopy::CopyBuilder;
    ///
    /// let stats = CopyBuilder::new("src_dir", "dst_dir").run_dir()?;
    /// # Ok::<(), parcopy::Error>(())
    /// ```
    pub fn run_dir(self) -> Result<CopyStats> {
        copy_dir(&self.src, &self.dst, &self.options)
    }

    /// Execute the copy operation for a single file only.
    ///
    /// Returns an error if the source is not a file.
    ///
    /// # Example
    ///
    /// ```no_run
    /// use parcopy::CopyBuilder;
    ///
    /// let stats = CopyBuilder::new("file.txt", "backup.txt").run_file()?;
    /// # Ok::<(), parcopy::Error>(())
    /// ```
    pub fn run_file(self) -> Result<CopyStats> {
        let start = std::time::Instant::now();
        let copied = copy_file(&self.src, &self.dst, &self.options)?;
        let file_len = self.src.metadata().map(|m| m.len()).unwrap_or(0);

        Ok(CopyStats {
            files_copied: if copied { 1 } else { 0 },
            files_skipped: if copied { 0 } else { 1 },
            symlinks_copied: 0,
            symlinks_skipped: 0,
            dirs_created: 0,
            bytes_copied: if copied { file_len } else { 0 },
            duration: start.elapsed(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::tempdir;

    #[test]
    fn test_builder_basic() {
        let src_dir = tempdir().unwrap();
        let dst_dir = tempdir().unwrap();

        // Create a test file
        fs::write(src_dir.path().join("test.txt"), "hello").unwrap();

        let stats = CopyBuilder::new(src_dir.path(), dst_dir.path().join("copy"))
            .run()
            .unwrap();

        assert_eq!(stats.files_copied, 1);
        assert!(dst_dir.path().join("copy/test.txt").exists());
    }

    #[test]
    fn test_builder_overwrite() {
        let src_dir = tempdir().unwrap();
        let dst_dir = tempdir().unwrap();

        fs::write(src_dir.path().join("test.txt"), "new content").unwrap();
        fs::create_dir_all(dst_dir.path().join("copy")).unwrap();
        fs::write(dst_dir.path().join("copy/test.txt"), "old content").unwrap();

        let stats = CopyBuilder::new(src_dir.path(), dst_dir.path().join("copy"))
            .overwrite()
            .run()
            .unwrap();

        assert_eq!(stats.files_copied, 1);
        let content = fs::read_to_string(dst_dir.path().join("copy/test.txt")).unwrap();
        assert_eq!(content, "new content");
    }

    #[test]
    fn test_builder_skip_existing() {
        let src_dir = tempdir().unwrap();
        let dst_dir = tempdir().unwrap();

        fs::write(src_dir.path().join("test.txt"), "new content").unwrap();
        fs::create_dir_all(dst_dir.path().join("copy")).unwrap();
        fs::write(dst_dir.path().join("copy/test.txt"), "old content").unwrap();

        let stats = CopyBuilder::new(src_dir.path(), dst_dir.path().join("copy"))
            .skip_existing()
            .run()
            .unwrap();

        assert_eq!(stats.files_copied, 0);
        assert_eq!(stats.files_skipped, 1);
        // Original content preserved
        let content = fs::read_to_string(dst_dir.path().join("copy/test.txt")).unwrap();
        assert_eq!(content, "old content");
    }

    #[test]
    fn test_builder_single_file() {
        let src_dir = tempdir().unwrap();
        let dst_dir = tempdir().unwrap();

        let src_file = src_dir.path().join("source.txt");
        let dst_file = dst_dir.path().join("dest.txt");

        fs::write(&src_file, "file content").unwrap();

        let stats = CopyBuilder::new(&src_file, &dst_file).run().unwrap();

        assert_eq!(stats.files_copied, 1);
        assert!(dst_file.exists());
        assert_eq!(fs::read_to_string(&dst_file).unwrap(), "file content");
    }

    #[test]
    fn test_builder_parallel() {
        let src_dir = tempdir().unwrap();
        let dst_dir = tempdir().unwrap();

        // Create multiple files
        for i in 0..10 {
            fs::write(
                src_dir.path().join(format!("file_{}.txt", i)),
                format!("content {}", i),
            )
            .unwrap();
        }

        let stats = CopyBuilder::new(src_dir.path(), dst_dir.path().join("copy"))
            .parallel(4)
            .run()
            .unwrap();

        assert_eq!(stats.files_copied, 10);
    }

    #[test]
    fn test_builder_options_accessor() {
        let builder = CopyBuilder::new("src", "dst")
            .parallel(8)
            .overwrite()
            .no_fsync();

        let options = builder.options();
        assert_eq!(options.parallel, 8);
        assert_eq!(options.on_conflict, OnConflict::Overwrite);
        assert!(!options.fsync);
    }

    #[test]
    fn test_builder_update_newer() {
        let src_dir = tempdir().unwrap();
        let dst_dir = tempdir().unwrap();

        // Create source file
        fs::write(src_dir.path().join("test.txt"), "content").unwrap();

        // Create older destination file
        fs::create_dir_all(dst_dir.path().join("copy")).unwrap();
        fs::write(dst_dir.path().join("copy/test.txt"), "old").unwrap();

        // Make source newer by touching it
        std::thread::sleep(std::time::Duration::from_millis(10));
        fs::write(src_dir.path().join("test.txt"), "newer content").unwrap();

        let stats = CopyBuilder::new(src_dir.path(), dst_dir.path().join("copy"))
            .update_newer()
            .run()
            .unwrap();

        assert_eq!(stats.files_copied, 1);
        let content = fs::read_to_string(dst_dir.path().join("copy/test.txt")).unwrap();
        assert_eq!(content, "newer content");
    }

    #[test]
    fn test_builder_update_newer_skips_older() {
        let src_dir = tempdir().unwrap();
        let dst_dir = tempdir().unwrap();

        // Create destination file first (so it's newer)
        fs::create_dir_all(dst_dir.path().join("copy")).unwrap();
        fs::write(dst_dir.path().join("copy/test.txt"), "newer in dst").unwrap();

        // Wait and create source file (older)
        std::thread::sleep(std::time::Duration::from_millis(10));
        // Touch dst to make it definitely newer
        fs::write(dst_dir.path().join("copy/test.txt"), "newer in dst").unwrap();

        // Create source - it will have older mtime since we touched dst after
        std::thread::sleep(std::time::Duration::from_millis(10));
        fs::write(src_dir.path().join("test.txt"), "older in src").unwrap();

        // Make dst newer again
        std::thread::sleep(std::time::Duration::from_millis(10));
        fs::write(dst_dir.path().join("copy/test.txt"), "definitely newer").unwrap();

        let stats = CopyBuilder::new(src_dir.path(), dst_dir.path().join("copy"))
            .update_newer()
            .run()
            .unwrap();

        assert_eq!(stats.files_copied, 0);
        assert_eq!(stats.files_skipped, 1);
    }

    #[test]
    fn test_builder_error_on_conflict() {
        let src_dir = tempdir().unwrap();
        let dst_dir = tempdir().unwrap();

        fs::write(src_dir.path().join("test.txt"), "content").unwrap();
        fs::create_dir_all(dst_dir.path().join("copy")).unwrap();
        fs::write(dst_dir.path().join("copy/test.txt"), "existing").unwrap();

        let result = CopyBuilder::new(src_dir.path(), dst_dir.path().join("copy"))
            .error_on_conflict()
            .run();

        assert!(result.is_err());
    }

    #[test]
    fn test_builder_no_timestamps() {
        let src_dir = tempdir().unwrap();
        let dst_dir = tempdir().unwrap();

        fs::write(src_dir.path().join("test.txt"), "content").unwrap();

        let _stats = CopyBuilder::new(src_dir.path(), dst_dir.path().join("copy"))
            .no_timestamps()
            .run()
            .unwrap();

        // File should be copied
        assert!(dst_dir.path().join("copy/test.txt").exists());
    }

    #[test]
    fn test_builder_max_depth() {
        let src_dir = tempdir().unwrap();
        let dst_dir = tempdir().unwrap();

        // Create nested structure (not too deep to avoid MaxDepthExceeded error)
        fs::create_dir_all(src_dir.path().join("a")).unwrap();
        fs::write(src_dir.path().join("root.txt"), "root").unwrap();
        fs::write(src_dir.path().join("a/level1.txt"), "level1").unwrap();

        let stats = CopyBuilder::new(src_dir.path(), dst_dir.path().join("copy"))
            .max_depth(2)
            .run()
            .unwrap();

        // Should copy both files
        assert!(dst_dir.path().join("copy/root.txt").exists());
        assert!(dst_dir.path().join("copy/a/level1.txt").exists());
        assert_eq!(stats.files_copied, 2);
    }

    #[test]
    fn test_builder_max_depth_exceeded() {
        let src_dir = tempdir().unwrap();
        let dst_dir = tempdir().unwrap();

        // Create deeply nested structure
        fs::create_dir_all(src_dir.path().join("a/b/c")).unwrap();
        fs::write(src_dir.path().join("a/b/c/deep.txt"), "deep").unwrap();

        // With max_depth=1, going into a/b should exceed the limit
        let result = CopyBuilder::new(src_dir.path(), dst_dir.path().join("copy"))
            .max_depth(1)
            .run();

        // Should return MaxDepthExceeded error
        assert!(result.is_err());
    }

    #[test]
    fn test_builder_run_file() {
        let src_dir = tempdir().unwrap();
        let dst_dir = tempdir().unwrap();

        let src_file = src_dir.path().join("source.txt");
        let dst_file = dst_dir.path().join("dest.txt");

        fs::write(&src_file, "file content").unwrap();

        let stats = CopyBuilder::new(&src_file, &dst_file).run_file().unwrap();

        assert_eq!(stats.files_copied, 1);
        assert_eq!(stats.bytes_copied, 12); // "file content" is 12 bytes
        assert!(dst_file.exists());
    }

    #[test]
    fn test_builder_chained_options() {
        let builder = CopyBuilder::new("src", "dst")
            .parallel(4)
            .overwrite()
            .no_fsync()
            .no_timestamps()
            .no_permissions()
            .max_depth(5);

        let options = builder.options();
        assert_eq!(options.parallel, 4);
        assert_eq!(options.on_conflict, OnConflict::Overwrite);
        assert!(!options.fsync);
        assert!(!options.preserve_timestamps);
        assert!(!options.preserve_permissions);
        assert_eq!(options.max_depth, Some(5));
    }

    #[test]
    fn test_builder_cancel_token() {
        use std::sync::atomic::AtomicBool;

        let cancel = Arc::new(AtomicBool::new(false));
        let builder = CopyBuilder::new("src", "dst").cancel_token(cancel.clone());

        let options = builder.options();
        assert!(options.cancel_token.is_some());
        assert!(!options.is_cancelled());

        // Set the token
        cancel.store(true, std::sync::atomic::Ordering::Relaxed);
        assert!(options.is_cancelled());
    }
}
