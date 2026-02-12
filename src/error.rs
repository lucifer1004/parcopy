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
//! | Partial | [`Error::PartialCopy`], [`Error::PartialSymlinks`] |
//! | Safety | [`Error::SymlinkLoop`], [`Error::MaxDepthExceeded`] |
//! | Control | [`Error::Cancelled`] |

use std::path::PathBuf;
use thiserror::Error;

/// Result type for parcopy operations.
///
/// This is a type alias for `std::result::Result<T, Error>`.
pub type Result<T> = std::result::Result<T, Error>;

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
