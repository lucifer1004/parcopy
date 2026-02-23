//! # parcopy
//!
//! Parallel, atomic, and safe file/directory copying for Rust.
//!
//! ## Core Features
//!
//! - **Parallel copying**: Uses rayon for concurrent file operations, optimized for NFS/network storage
//! - **Atomic writes**: Uses temp file + rename pattern to ensure no partial files
//! - **TOCTOU safe**: Uses `persist_noclobber` to prevent race conditions
//! - **Symlink aware**: Correctly handles symlinks without following them
//! - **Symlink loop detection**: Prevents infinite recursion from circular symlinks
//! - **Resumable**: Skips already-existing files for interrupted operations
//! - **Permission preserving**: Copies file and directory permissions
//! - **Timestamp preserving**: Copies file modification and access times
//! - **Incremental copy**: Only copy files newer than destination (`UpdateNewer`)
//! - **Reflink support**: Instant copy-on-write on btrfs/XFS/APFS
//! - **Security hardened**: Detects and optionally blocks escaping symlinks
//! - **Windows attribute preserving**: Copies hidden, system, archive attributes on Windows
//!
//! ## Quick Start with Builder API
//!
//! The easiest way to use parcopy is with the [`CopyBuilder`]:
//!
//! ```no_run
//! use parcopy::CopyBuilder;
//!
//! // Simple copy with smart defaults
//! let stats = CopyBuilder::new("src", "dst").run()?;
//! println!("Copied {} files ({} bytes)", stats.files_copied, stats.bytes_copied);
//! # Ok::<(), parcopy::Error>(())
//! ```
//!
//! ### Incremental Backup
//!
//! ```no_run
//! use parcopy::CopyBuilder;
//!
//! // Only copy files that have changed
//! let stats = CopyBuilder::new("project", "backup")
//!     .update_newer()
//!     .run()?;
//!
//! println!("Updated {} files, {} already up-to-date",
//!     stats.files_copied, stats.files_skipped);
//! # Ok::<(), parcopy::Error>(())
//! ```
//!
//! ### High-Performance Copy
//!
//! ```no_run
//! use parcopy::CopyBuilder;
//!
//! let stats = CopyBuilder::new("data", "backup")
//!     .parallel(32)      // More threads for NFS
//!     .overwrite()       // Replace existing
//!     .no_fsync()        // Skip fsync for speed
//!     .run()?;
//! # Ok::<(), parcopy::Error>(())
//! ```
//!
//! ## Function API
//!
//! For more control, use the function API with [`CopyOptions`]:
//!
//! ```no_run
//! use parcopy::{copy_dir, CopyOptions, OnConflict};
//! use std::path::Path;
//!
//! let options = CopyOptions::default()
//!     .with_parallel(8)                         // Limit parallelism
//!     .with_on_conflict(OnConflict::Overwrite)  // Overwrite existing
//!     .with_max_depth(100)                      // Limit recursion depth
//!     .with_block_escaping_symlinks()           // Block dangerous symlinks
//!     .without_fsync();                         // Faster but less durable
//!
//! let stats = copy_dir(Path::new("src"), Path::new("dst"), &options)?;
//! println!("Copied {} files, skipped {}", stats.files_copied, stats.files_skipped);
//! # Ok::<(), parcopy::Error>(())
//! ```
//!
//! ## Safety Guarantees
//!
//! ### Atomic Writes
//!
//! Files are written to a temporary file in the destination directory, then
//! renamed atomically. This ensures no partial files exist if interrupted.
//!
//! ### TOCTOU Protection
//!
//! Uses `persist_noclobber` (backed by `renameat2(RENAME_NOREPLACE)` on Linux)
//! to atomically fail if destination was created between existence check and rename.
//!
//! ### Symlink Safety
//!
//! - Symlinks are never followed during directory traversal
//! - Symlink loops are detected and reported as [`Error::SymlinkLoop`]
//! - Escaping symlinks (`../`) are warned or blocked based on configuration
//!
//! ## Optional Features
//!
//! | Feature | Description |
//! |---------|-------------|
//! | `progress` | Progress bar support with indicatif |
//! | `tracing` | Structured logging with tracing crate |
//! | `serde` | Serialize/Deserialize for [`CopyOptions`] |
//! | `full` | Enable all optional features |
//!
//! ## NFS Optimization
//!
//! This crate is specifically optimized for NFS and network filesystems where
//! many small files cause metadata storms. By parallelizing file operations,
//! we can have multiple NFS RPCs in-flight simultaneously, significantly
//! improving throughput. Default parallelism is 16 concurrent operations.

#![cfg_attr(docsrs, feature(doc_cfg))]

mod builder;
mod copy;
mod error;
mod options;
mod utils;

#[cfg(feature = "progress")]
mod progress;

#[cfg(windows)]
mod win_attrs;

pub use builder::CopyBuilder;
pub use copy::{CopyStats, copy_dir, copy_file};
pub use error::{Error, Result, is_no_space_error};
pub use options::{CopyOptions, OnConflict};

#[cfg(feature = "progress")]
#[cfg_attr(docsrs, doc(cfg(feature = "progress")))]
pub use progress::{ProgressCallback, create_progress_bar};
