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

/// Stable semantic error codes for machine-readable integrations.
///
/// These codes are intended to be stable across minor versions.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[cfg_attr(feature = "serde", serde(rename_all = "snake_case"))]
pub enum ErrorCode {
    InvalidInput,
    SourceNotFound,
    AlreadyExists,
    PermissionDenied,
    NoSpace,
    Cancelled,
    PartialCopy,
    SymlinkLoop,
    IoError,
    Internal,
}

/// Human-readable metadata for one stable error code.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ErrorCodeSpec {
    /// Stable semantic code.
    pub code: ErrorCode,
    /// Semantic meaning of the code.
    pub meaning: &'static str,
    /// Typical runtime triggers.
    pub typical_triggers: &'static str,
    /// Recommended operator/user remediation.
    pub remediation: &'static str,
}

const ERROR_CODE_SPECS: [ErrorCodeSpec; 10] = [
    ErrorCodeSpec {
        code: ErrorCode::InvalidInput,
        meaning: "User input or invocation is invalid.",
        typical_triggers: "Missing destination operand, unsupported source/target shape.",
        remediation: "Correct CLI arguments or input paths and retry.",
    },
    ErrorCodeSpec {
        code: ErrorCode::SourceNotFound,
        meaning: "Source path does not exist.",
        typical_triggers: "Missing file/directory or stale path.",
        remediation: "Verify the source path and retry.",
    },
    ErrorCodeSpec {
        code: ErrorCode::AlreadyExists,
        meaning: "Destination conflict under selected policy.",
        typical_triggers: "Conflict policy is error and destination exists.",
        remediation: "Choose overwrite/update policy or remove destination.",
    },
    ErrorCodeSpec {
        code: ErrorCode::PermissionDenied,
        meaning: "OS denied filesystem access.",
        typical_triggers: "Read/write blocked by permissions or ACL rules.",
        remediation: "Adjust permissions/ownership and retry.",
    },
    ErrorCodeSpec {
        code: ErrorCode::NoSpace,
        meaning: "Destination storage is full.",
        typical_triggers: "Disk quota exceeded or filesystem out of free space.",
        remediation: "Free space and rerun; copy is resumable by default.",
    },
    ErrorCodeSpec {
        code: ErrorCode::Cancelled,
        meaning: "Operation cancelled by user or cancellation token.",
        typical_triggers: "Ctrl+C or explicit cancellation request.",
        remediation: "Rerun with the same command to resume.",
    },
    ErrorCodeSpec {
        code: ErrorCode::PartialCopy,
        meaning: "Some items copied, some failed.",
        typical_triggers: "Batch copy with mixed per-item outcomes.",
        remediation: "Inspect item-level failures and retry remaining items.",
    },
    ErrorCodeSpec {
        code: ErrorCode::SymlinkLoop,
        meaning: "Symlink traversal would recurse infinitely.",
        typical_triggers: "Circular symlink graph detected.",
        remediation: "Remove/fix the loop or adjust symlink policy.",
    },
    ErrorCodeSpec {
        code: ErrorCode::IoError,
        meaning: "Generic I/O error.",
        typical_triggers: "Transient filesystem/network I/O failures.",
        remediation: "Retry and inspect optional low-level error details.",
    },
    ErrorCodeSpec {
        code: ErrorCode::Internal,
        meaning: "Unexpected internal failure.",
        typical_triggers: "Invariant breakage or uncategorized internal path.",
        remediation: "Collect context/logs and file a bug report.",
    },
];

impl ErrorCode {
    /// Stable wire-format value used in machine-readable outputs.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::InvalidInput => "invalid_input",
            Self::SourceNotFound => "source_not_found",
            Self::AlreadyExists => "already_exists",
            Self::PermissionDenied => "permission_denied",
            Self::NoSpace => "no_space",
            Self::Cancelled => "cancelled",
            Self::PartialCopy => "partial_copy",
            Self::SymlinkLoop => "symlink_loop",
            Self::IoError => "io_error",
            Self::Internal => "internal",
        }
    }

    /// Returns stable reference metadata for this error code.
    #[must_use]
    pub const fn spec(self) -> ErrorCodeSpec {
        match self {
            Self::InvalidInput => ERROR_CODE_SPECS[0],
            Self::SourceNotFound => ERROR_CODE_SPECS[1],
            Self::AlreadyExists => ERROR_CODE_SPECS[2],
            Self::PermissionDenied => ERROR_CODE_SPECS[3],
            Self::NoSpace => ERROR_CODE_SPECS[4],
            Self::Cancelled => ERROR_CODE_SPECS[5],
            Self::PartialCopy => ERROR_CODE_SPECS[6],
            Self::SymlinkLoop => ERROR_CODE_SPECS[7],
            Self::IoError => ERROR_CODE_SPECS[8],
            Self::Internal => ERROR_CODE_SPECS[9],
        }
    }

    /// Returns all stable error codes in canonical reference order.
    #[must_use]
    pub const fn all() -> [ErrorCode; 10] {
        [
            ErrorCode::InvalidInput,
            ErrorCode::SourceNotFound,
            ErrorCode::AlreadyExists,
            ErrorCode::PermissionDenied,
            ErrorCode::NoSpace,
            ErrorCode::Cancelled,
            ErrorCode::PartialCopy,
            ErrorCode::SymlinkLoop,
            ErrorCode::IoError,
            ErrorCode::Internal,
        ]
    }
}

impl std::fmt::Display for ErrorCode {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Returns stable reference metadata for all known error codes.
#[must_use]
pub const fn error_code_specs() -> &'static [ErrorCodeSpec; 10] {
    &ERROR_CODE_SPECS
}

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
    /// Files that were successfully copied are retained at the destination.
    /// This allows for resumable copy operations.
    ///
    /// # Recovery
    ///
    /// Free up space on the destination and re-run the copy operation.
    /// Successfully copied files will be skipped by default (OnConflict::Skip),
    /// and the copy will resume from where it left off.
    #[error(
        "No space left on device: {files_copied} of {total_files} files copied, {remaining} remaining. Re-run to resume."
    )]
    NoSpace {
        /// Number of files that were successfully copied before the error
        files_copied: usize,
        /// Total bytes that were copied before the error
        bytes_copied: u64,
        /// Number of files that failed to copy
        failed_files: usize,
        /// Total number of files attempted
        total_files: usize,
        /// Number of files remaining (not yet copied)
        remaining: usize,
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

impl Error {
    /// Classify this error into a stable semantic error code.
    #[must_use]
    pub fn code(&self) -> ErrorCode {
        fn io_code(error: &io::Error) -> ErrorCode {
            if is_no_space_error(error) {
                return ErrorCode::NoSpace;
            }
            if error.kind() == io::ErrorKind::PermissionDenied {
                return ErrorCode::PermissionDenied;
            }
            ErrorCode::IoError
        }

        match self {
            Self::Io(error) => io_code(error),
            Self::TempFile { source, .. } | Self::Persist { source, .. } => io_code(source),
            Self::PartialCopy { .. } | Self::PartialSymlinks { .. } => ErrorCode::PartialCopy,
            Self::NoSpace { .. } => ErrorCode::NoSpace,
            Self::SourceNotFound(_) => ErrorCode::SourceNotFound,
            Self::NotADirectory(_) | Self::IsADirectory(_) | Self::MaxDepthExceeded { .. } => {
                ErrorCode::InvalidInput
            }
            Self::AlreadyExists(_) => ErrorCode::AlreadyExists,
            Self::SymlinkLoop(_) => ErrorCode::SymlinkLoop,
            Self::Cancelled { .. } => ErrorCode::Cancelled,
        }
    }
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
            remaining: 3,
            path: PathBuf::from("/dest/file.txt"),
        };
        let msg = format!("{}", error);
        assert!(msg.contains("No space left on device"));
        assert!(msg.contains("5 of 8 files copied"));
        assert!(msg.contains("3 remaining"));
    }

    #[test]
    fn test_error_code_mapping_for_validation_and_conflict() {
        let source_not_found = Error::SourceNotFound(PathBuf::from("/missing"));
        assert_eq!(source_not_found.code(), ErrorCode::SourceNotFound);

        let conflict = Error::AlreadyExists(PathBuf::from("/exists"));
        assert_eq!(conflict.code(), ErrorCode::AlreadyExists);

        let invalid_input = Error::IsADirectory(PathBuf::from("/dir"));
        assert_eq!(invalid_input.code(), ErrorCode::InvalidInput);
    }

    #[test]
    fn test_error_code_mapping_for_io_and_no_space() {
        let permission = Error::Io(io::Error::new(io::ErrorKind::PermissionDenied, "denied"));
        assert_eq!(permission.code(), ErrorCode::PermissionDenied);

        let no_space = Error::NoSpace {
            files_copied: 1,
            bytes_copied: 42,
            failed_files: 1,
            total_files: 2,
            remaining: 1,
            path: PathBuf::from("/full"),
        };
        assert_eq!(no_space.code(), ErrorCode::NoSpace);
    }

    #[test]
    fn test_error_code_as_str() {
        assert_eq!(ErrorCode::InvalidInput.as_str(), "invalid_input");
        assert_eq!(ErrorCode::PermissionDenied.as_str(), "permission_denied");
    }

    #[test]
    fn test_error_code_specs_cover_all_codes() {
        assert_eq!(error_code_specs().len(), ErrorCode::all().len());
        for code in ErrorCode::all() {
            assert_eq!(code.spec().code, code);
        }
    }
}
