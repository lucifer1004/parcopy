//! Configuration options for copy operations.
//!
//! This module provides [`CopyOptions`] for configuring copy behavior and
//! [`OnConflict`] for handling destination conflicts.
//!
//! # Example
//!
//! ```
//! use parcopy::{CopyOptions, OnConflict};
//!
//! // Create options with builder pattern
//! let options = CopyOptions::default()
//!     .with_parallel(8)
//!     .with_on_conflict(OnConflict::Overwrite)
//!     .with_max_depth(100);
//! ```

/// Behavior when destination file already exists.
///
/// This enum controls what happens when a file or symlink already exists
/// at the destination path.
///
/// # Default
///
/// The default is [`OnConflict::Skip`], which enables resumable copies.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub enum OnConflict {
    /// Skip existing files (default, enables resumability).
    ///
    /// If a file already exists at the destination, it is left unchanged
    /// and the copy operation continues with the next file.
    #[default]
    Skip,
    /// Overwrite existing files.
    ///
    /// If a file already exists at the destination, it is replaced with
    /// the source file content.
    Overwrite,
    /// Return an error if any file exists.
    ///
    /// The copy operation fails immediately if any destination file
    /// already exists.
    Error,
    /// Update only if source is newer than destination.
    ///
    /// Compares modification times and only copies if the source file
    /// has a more recent mtime than the destination. This is similar
    /// to `rsync --update` or `cp --update`.
    ///
    /// If the destination doesn't exist, the file is copied.
    /// If mtimes are equal, the file is skipped.
    UpdateNewer,
}

/// Options for copy operations.
///
/// Use [`Default::default()`] to get sensible defaults, then customize
/// using the builder methods.
///
/// # Default Values
///
/// | Field | Default | Description |
/// |-------|---------|-------------|
/// | `parallel` | 16 | Concurrent operations |
/// | `on_conflict` | `Skip` | Skip existing files |
/// | `preserve_permissions` | `true` | Copy file permissions |
/// | `preserve_dir_permissions` | `true` | Copy directory permissions |
/// | `preserve_symlinks` | `true` | Recreate symlinks (don't follow) |
/// | `preserve_timestamps` | `true` | Copy file timestamps (mtime/atime) |
/// | `fsync` | `true` | Sync to disk after write |
/// | `warn_escaping_symlinks` | `true` | Warn about `..` in symlinks |
/// | `block_escaping_symlinks` | `false` | Block symlinks with `..` |
/// | `max_depth` | `None` | No depth limit |
///
/// # Example
///
/// ```
/// use parcopy::CopyOptions;
///
/// let options = CopyOptions::default()
///     .with_parallel(32)      // More parallelism for local SSD
///     .without_fsync();       // Skip fsync for speed
/// ```
#[derive(Debug, Clone)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[allow(clippy::struct_excessive_bools)]
pub struct CopyOptions {
    /// Number of parallel copy operations (default: 16)
    ///
    /// This is optimized for NFS where too many parallel operations
    /// can overwhelm the server. Adjust based on your storage backend.
    pub parallel: usize,

    /// Behavior when destination file already exists
    pub on_conflict: OnConflict,

    /// Whether to preserve file permissions (default: true)
    pub preserve_permissions: bool,

    /// Whether to preserve directory permissions (default: true)
    pub preserve_dir_permissions: bool,

    /// Whether to preserve symlinks (default: true)
    ///
    /// If false, symlinks are followed and the target content is copied.
    pub preserve_symlinks: bool,

    /// Whether to sync files to disk after writing (default: true)
    ///
    /// This ensures durability but may slow down copies.
    pub fsync: bool,

    /// Warn about relative symlinks that escape upward (default: true)
    ///
    /// Symlinks like `../../../etc/passwd` or `foo/../../bar` may point to different
    /// locations in the destination directory structure.
    pub warn_escaping_symlinks: bool,

    /// Block (skip) symlinks that escape upward (default: false)
    ///
    /// When true, symlinks containing `..` components are skipped entirely
    /// instead of just warning. This provides stronger security but may
    /// break legitimate use cases.
    pub block_escaping_symlinks: bool,

    /// Maximum directory depth to traverse (default: None = unlimited)
    ///
    /// Set this to prevent stack overflow from extremely deep directory
    /// structures or symlink loops when `preserve_symlinks` is false.
    pub max_depth: Option<usize>,

    /// Whether to preserve file timestamps (default: true)
    ///
    /// When enabled, the modification time (mtime) and access time (atime)
    /// of copied files are set to match the source files. This is essential
    /// for backup and sync scenarios.
    pub preserve_timestamps: bool,

    /// Callback for warnings (optional)
    ///
    /// If not set and `tracing` feature is enabled, warnings are logged via tracing.
    /// Otherwise, warnings are silently ignored.
    #[cfg_attr(feature = "serde", serde(skip))]
    pub warn_handler: Option<fn(&str)>,
}

impl Default for CopyOptions {
    fn default() -> Self {
        Self {
            parallel: 16,
            on_conflict: OnConflict::Skip,
            preserve_permissions: true,
            preserve_dir_permissions: true,
            preserve_symlinks: true,
            fsync: true,
            warn_escaping_symlinks: true,
            block_escaping_symlinks: false,
            max_depth: None,
            preserve_timestamps: true,
            warn_handler: None,
        }
    }
}

impl CopyOptions {
    /// Create options with a warning handler
    #[must_use]
    pub fn with_warn_handler(mut self, handler: fn(&str)) -> Self {
        self.warn_handler = Some(handler);
        self
    }

    /// Set the number of parallel operations
    ///
    /// Value is clamped to at least 1 to prevent panics.
    #[must_use]
    pub fn with_parallel(mut self, n: usize) -> Self {
        self.parallel = n.max(1);
        self
    }

    /// Set the conflict behavior
    #[must_use]
    pub fn with_on_conflict(mut self, on_conflict: OnConflict) -> Self {
        self.on_conflict = on_conflict;
        self
    }

    /// Disable fsync for faster (but less durable) copies
    #[must_use]
    pub fn without_fsync(mut self) -> Self {
        self.fsync = false;
        self
    }

    /// Set maximum directory depth
    #[must_use]
    pub fn with_max_depth(mut self, depth: usize) -> Self {
        self.max_depth = Some(depth);
        self
    }

    /// Block symlinks that escape upward (instead of just warning)
    #[must_use]
    pub fn with_block_escaping_symlinks(mut self) -> Self {
        self.block_escaping_symlinks = true;
        self
    }

    /// Disable timestamp preservation for faster copies
    ///
    /// By default, file timestamps (mtime/atime) are preserved. Disable
    /// this if you don't need timestamps and want slightly faster copies.
    #[must_use]
    pub fn without_timestamps(mut self) -> Self {
        self.preserve_timestamps = false;
        self
    }

    /// Disable permission preservation
    ///
    /// By default, file permissions are copied from source to destination.
    /// Disable this if you want files to use the default umask permissions.
    #[must_use]
    pub fn without_permissions(mut self) -> Self {
        self.preserve_permissions = false;
        self
    }

    pub(crate) fn warn(&self, msg: &str) {
        if let Some(handler) = self.warn_handler {
            handler(msg);
        } else {
            #[cfg(feature = "tracing")]
            tracing::warn!("{}", msg);
        }
    }
}
