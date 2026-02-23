//! Core copy operations.
//!
//! This module provides functions for copying files and directories with
//! safety guarantees including atomic writes, TOCTOU protection, and
//! parallel operations.

mod dir;
mod file;
mod reflink;
mod utils;

// Re-export public API
pub use dir::{CopyStats, copy_dir};
pub use file::copy_file;
