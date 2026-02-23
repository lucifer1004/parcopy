//! Error types for parcopy.
//!
//! This module provides the [`Error`] enum containing all possible errors
//! that can occur during copy operations, and the [`Result`] type alias.
//!
//! # Error Categories
//!
//! | Category | Errors |
//! |----------|--------|
//! | IO | [`Error::Io`], [`Error::TempFile`], [`Error::Persist`] |
//! | Validation | [`Error::SourceNotFound`], [`Error::NotADirectory`], [`Error::IsADirectory`] |
//! | Conflict | [`Error::AlreadyExists`] |
//! | Partial | [`Error::PartialCopy`], [`Error::PartialSymlinks`], [`Error::NoSpace`] |
//! | Safety | [`Error::SymlinkLoop`], [`Error::MaxDepthExceeded`] |
//! | Control | [`Error::Cancelled`] |

use std::io;
use std::path::PathBuf;
use thiserror::Error;

/// Result type for parcopy operations.
///
/// This is a type alias for `std::result::Result<T, Error>`.
pub type Result<T> = std::result::Result<T, Error>;

/// Check if an IO error indicates "no space left on device".
///
/// This helper function detects storage-full conditions across platforms.
///
/// # Platform Support
///
/// | Platform | Error Detection |
/// |----------|-----------------|
/// | Unix | `ENOSPC` (errno 28) |
/// | Windows | `ERROR_DISK_FULL` (0x70) |
///
/// # Example
///
/// ```no_run
/// use std::io;
/// use parcopy::is_no_space_error;
///
/// let error = io::Error::new(io::ErrorKind::StorageFull, "disk full");
/// if is_no_space_error(&error) {
///     println!("Destination has no space!");
/// }
/// ```
pub fn is_no_space_error(error: &io::Error) -> bool {
    // Check standard StorageFull kind first
    if error.kind() == io::ErrorKind::StorageFull {
        return true;
    }

    // Platform-specific checks
    #[cfg(unix)]
    {
        // On Unix, check for ENOSPC (errno 28)
        // The raw OS error might be available even if kind() isn't StorageFull
        if let Some(raw_error) = error.raw_os_error() {
            // ENOSPC = 28 on most Unix systems
            const ENOSPC: i32 = 28;
            return raw_error == ENOSPC;
        }
    }

    #[cfg(windows)]
    {
        // On Windows, check for ERROR_DISK_FULL (0x70 = 112)
        if let Some(raw_error) = error.raw_os_error() {
            const ERROR_DISK_FULL: i32 = 112;
            return raw_error == ERROR_DISK_FULL;
        }
    }

    false
}

/// Errors that can occur during copy operations.
///
/// All errors include relevant path information to aid debugging.
/// Use the [`std::error::Error`] trait methods to access underlying
/// causes where applicable.
#[derive(Error, Debug)]
#[non_exhaustive]
pub enum Error {
    /// IO error during file operations
    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),

    /// Failed to copy one or more files
    #[error("Failed to copy {failed} of {total} files")]
    PartialCopy {
        /// Number of files that failed to copy
        failed: usize,
        /// Total number of files
        total: usize,
    },

    /// Failed to copy one or more symlinks
    #[error("Failed to copy {failed} of {total} symlinks")]
    PartialSymlinks {
        /// Number of symlinks that failed to copy
        failed: usize,
        /// Total number of symlinks
        total: usize,
    },

    /// No space left on device during copy operation
    ///
    /// This error indicates that the destination storage ran out of space.
    /// Files that were successfully copied before the error occurred have
    /// been cleaned up (deleted) to avoid leaving partial data.
    ///
    /// # Recovery
    ///
    /// Free up space on the destination and re-run the copy operation.
    /// Since successfully copied files were removed, this is a clean state.
    #[error("No space left on device: {files_copied} of {total_files} files copied before failure, cleaned up {cleaned_up} files")]
    NoSpace {
        /// Number of files that were successfully copied before the error
        files_copied: usize,
        /// Total bytes that were copied before the error
        bytes_copied: u64,
        /// Number of files that failed to copy
        failed_files: usize,
        /// Total number of files attempted
        total_files: usize,
        /// Number of files that were cleaned up after the error
        cleaned_up: usize,
        /// The path where the no-space error occurred
        path: PathBuf,
    },

    /// Source path does not exist
    #[error("Source path does not exist: {0}")]
    SourceNotFound(PathBuf),

    /// Source is not a directory
    #[error("Source is not a directory: {0}")]
    NotADirectory(PathBuf),

    /// Destination already exists
    #[error("Destination already exists: {0}")]
    AlreadyExists(PathBuf),

    /// Source is a directory, use `copy_dir` instead
    #[error("Source is a directory, use copy_dir instead: {0}")]
    IsADirectory(PathBuf),

    /// Failed to create temporary file
    #[error("Failed to create temporary file in {path}: {source}")]
    TempFile {
        /// Directory where temp file creation was attempted
        path: PathBuf,
        /// Underlying error
        source: std::io::Error,
    },

    /// Failed to persist temporary file
    #[error("Failed to persist temporary file to {path}: {source}")]
    Persist {
        /// Target path
        path: PathBuf,
        /// Underlying error
        source: std::io::Error,
    },

    /// Symlink loop detected (would cause infinite recursion)
    #[error("Symlink loop detected: {0}")]
    SymlinkLoop(PathBuf),

    /// Maximum directory depth exceeded
    #[error("Maximum depth {max_depth} exceeded at: {path}")]
    MaxDepthExceeded {
        /// The path where max depth was exceeded
        path: PathBuf,
        /// The configured maximum depth
        max_depth: usize,
    },

    /// Operation was cancelled via cancellation token
    ///
    /// This error carries partial statistics so the caller knows what
    /// was completed before cancellation. Re-running with
    /// [`OnConflict::Skip`](crate::OnConflict::Skip) (the default)
    /// will resume where the cancelled operation left off.
    #[error("Operation cancelled ({files_copied} files copied, {bytes_copied} bytes)")]
    Cancelled {
        /// Number of files successfully copied before cancellation
        files_copied: u64,
        /// Total bytes copied before cancellation
        bytes_copied: u64,
        /// Number of files skipped before cancellation
        files_skipped: u64,
        /// Number of directories created before cancellation
        dirs_created: u64,
    },
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_is_no_space_error_storage_full_kind() {
        let error = io::Error::new(io::ErrorKind::StorageFull, "disk full");
        assert!(is_no_space_error(&error));
    }

    #[test]
    fn test_is_no_space_error_other_kind() {
        let error = io::Error::new(io::ErrorKind::NotFound, "not found");
        assert!(!is_no_space_error(&error));
    }

    #[cfg(unix)]
    #[test]
    fn test_is_no_space_error_enospc() {
        // Create an error with ENOSPC raw error code
        let error = io::Error::from_raw_os_error(28); // ENOSPC
        assert!(is_no_space_error(&error));
    }

    #[cfg(unix)]
    #[test]
    fn test_is_no_space_error_other_errno() {
        let error = io::Error::from_raw_os_error(2); // ENOENT
        assert!(!is_no_space_error(&error));
    }

    #[test]
    fn test_no_space_error_display() {
        let error = Error::NoSpace {
            files_copied: 5,
            bytes_copied: 1024,
            failed_files: 3,
            total_files: 8,
            cleaned_up: 5,
            path: PathBuf::from("/dest/file.txt"),
        };
        let msg = format!("{}", error);
        assert!(msg.contains("No space left on device"));
        assert!(msg.contains("5 of 8 files copied before failure"));
        assert!(msg.contains("cleaned up 5 files"));
    }
}
