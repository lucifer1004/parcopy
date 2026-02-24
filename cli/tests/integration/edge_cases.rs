//! Edge case integration tests for pcp CLI.
//!
//! These tests cover edge cases and advanced features:
//! - Symlink handling (--follow-symlinks, --block-escaping-symlinks)
//! - Timestamp preservation
//! - File type conflicts
//! - Special filenames and boundary conditions
//! - Large files and performance scenarios

#[path = "edge_cases/boundary_cases.rs"]
mod boundary_cases;

#[path = "edge_cases/file_type_conflict.rs"]
mod file_type_conflict;

#[path = "edge_cases/symlink_handling.rs"]
mod symlink_handling;

#[path = "edge_cases/timestamp_preservation.rs"]
mod timestamp_preservation;
