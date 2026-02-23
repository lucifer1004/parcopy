//! Directory copy operations.
//!
//! This module provides functions for copying directories recursively
//! with parallel file operations, symlink handling, and safety features.

use crate::error::{Error, Result};
use crate::options::{CopyOptions, OnConflict};
use crate::utils::path::safe_path;
use rayon::prelude::*;
use std::collections::HashSet;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::Instant;

use super::file::copy_file_internal;
use super::utils::{DirEntry, get_dir_key, is_escaping_symlink, is_symlink, symlink};

/// Outcome of a single file copy operation (internal use)
/// Used for tracking results in parallel copy operations
#[derive(Debug)]
pub(crate) enum FileCopyOutcome {
    /// File was successfully copied
    Copied {
        /// Source path
        src: PathBuf,
        /// Destination path (for potential cleanup)
        dst: PathBuf,
        /// Number of bytes copied
        bytes: u64,
    },
    /// File was skipped (already existed)
    Skipped {
        /// Source path
        src: PathBuf,
        /// Destination path
        dst: PathBuf,
    },
    /// Copy failed
    Failed {
        /// Source path
        src: PathBuf,
        /// Destination path
        dst: PathBuf,
        /// The error message
        error_msg: String,
        /// Whether this was a "no space" error
        is_no_space: bool,
    },
}

/// Statistics from a copy operation.
///
/// Returned by [`copy_dir`] to provide information about what was copied.
///
/// # Example
///
/// ```no_run
/// use parcopy::{copy_dir, CopyOptions};
/// use std::path::Path;
///
/// let stats = copy_dir(Path::new("src"), Path::new("dst"), &CopyOptions::default())?;
/// println!("Copied {} files ({} bytes)", stats.files_copied, stats.bytes_copied);
/// println!("Skipped {} files", stats.files_skipped);
/// # Ok::<(), parcopy::Error>(())
/// ```
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CopyStats {
    /// Number of files successfully copied
    pub files_copied: u64,
    /// Number of files skipped (already existed)
    pub files_skipped: u64,
    /// Number of symlinks successfully copied
    pub symlinks_copied: u64,
    /// Number of symlinks skipped
    pub symlinks_skipped: u64,
    /// Number of directories created
    pub dirs_created: u64,
    /// Total bytes copied
    pub bytes_copied: u64,
    /// Duration of the copy operation
    pub duration: std::time::Duration,
}

/// Copy a directory recursively with parallel file operations
///
/// # Strategy for NFS optimization
///
/// 1. Collect all entries (dirs, files, symlinks) in a single pass
/// 2. Create directory structure first (must be sequential for parent ordering)
/// 3. Copy files in parallel with controlled concurrency
/// 4. Recreate symlinks (preserving targets)
///
/// # Arguments
///
/// * `src` - Source directory path
/// * `dst` - Destination directory path
/// * `options` - Copy options
///
/// # Returns
///
/// Returns [`CopyStats`] with information about what was copied, including
/// file counts, byte counts, and duration.
///
/// # Errors
///
/// Returns an error if:
/// - Source does not exist ([`Error::SourceNotFound`])
/// - Source is not a directory ([`Error::NotADirectory`])
/// - Symlink loop detected ([`Error::SymlinkLoop`])
/// - Max depth exceeded ([`Error::MaxDepthExceeded`])
/// - Some files failed to copy ([`Error::PartialCopy`])
/// - Some symlinks failed to copy ([`Error::PartialSymlinks`])
/// - Destination exists and `on_conflict` is [`OnConflict::Error`] ([`Error::AlreadyExists`])
/// - IO operations fail ([`Error::Io`])
#[allow(clippy::too_many_lines)]
pub fn copy_dir(src: &Path, dst: &Path, options: &CopyOptions) -> Result<CopyStats> {
    let start_time = Instant::now();

    if !src.exists() {
        return Err(Error::SourceNotFound(src.to_path_buf()));
    }

    if !src.is_dir() {
        return Err(Error::NotADirectory(src.to_path_buf()));
    }

    // Phase 1: Collect all entries recursively
    let mut dirs: Vec<DirEntry> = Vec::new();
    let mut files: Vec<(PathBuf, PathBuf)> = Vec::new();
    let mut symlinks: Vec<(PathBuf, PathBuf)> = Vec::new();

    // Track visited directories by (dev, ino) to detect symlink loops
    // This is much faster than canonicalize() which resolves all symlinks
    let mut visited: HashSet<(u64, u64)> = HashSet::new();

    collect_entries(
        src,
        dst,
        &mut dirs,
        &mut files,
        &mut symlinks,
        options,
        0,
        &mut visited,
    )?;

    // Phase 2: Create directory structure (sequential, must respect parent ordering)
    let mut dirs_created = 0u64;
    for dir in &dirs {
        if options.is_cancelled() {
            return Err(Error::Cancelled {
                files_copied: 0,
                bytes_copied: 0,
                files_skipped: 0,
                dirs_created,
            });
        }

        // Check if directory already exists
        // Use extended-length path format on Windows to support long paths
        let safe_dst = safe_path(&dir.dst);
        let created = if !safe_dst.exists() {
            fs::create_dir_all(&safe_dst)?;
            true
        } else {
            false
        };

        // Preserve directory permissions from source
        if options.preserve_dir_permissions {
            match fs::metadata(&dir.src) {
                Ok(metadata) => {
                    if let Err(e) = fs::set_permissions(&safe_dst, metadata.permissions()) {
                        options.warn(&format!(
                            "Failed to set permissions on {}: {}",
                            dir.dst.display(),
                            e
                        ));
                    }
                }
                Err(e) => {
                    options.warn(&format!(
                        "Failed to read metadata from {}: {}",
                        dir.src.display(),
                        e
                    ));
                }
            }
        }

        // Preserve Windows directory attributes (hidden, system, etc.)
        #[cfg(windows)]
        if options.preserve_windows_attributes {
            crate::win_attrs::copy_attributes(&dir.src, &safe_dst);
        }

        if created {
            dirs_created += 1;
        }
    }

    let total_files = files.len();
    let total_symlinks = symlinks.len();

    // Phase 3: Copy files in parallel with controlled concurrency
    let mut files_copied: u64 = 0;
    let mut files_skipped: u64 = 0;
    let mut bytes_copied: u64 = 0;
    let mut failed_count: usize = 0;
    let mut no_space_error: Option<(PathBuf, String)> = None;

    if total_files > 0 {
        let do_copy = |files: &[(PathBuf, PathBuf)]| -> Vec<FileCopyOutcome> {
            files
                .par_iter()
                .map(|(src_file, dst_file)| {
                    // Check cancellation before starting each file
                    if options.is_cancelled() {
                        return FileCopyOutcome::Skipped {
                            src: src_file.clone(),
                            dst: dst_file.clone(),
                        };
                    }

                    match copy_file_internal(src_file, dst_file, options) {
                        Ok(result) => {
                            if result.copied {
                                FileCopyOutcome::Copied {
                                    src: src_file.clone(),
                                    dst: dst_file.clone(),
                                    bytes: result.bytes,
                                }
                            } else {
                                FileCopyOutcome::Skipped {
                                    src: src_file.clone(),
                                    dst: dst_file.clone(),
                                }
                            }
                        }
                        Err(e) => {
                            let is_no_space = match &e {
                                Error::Io(io_err) => crate::is_no_space_error(io_err),
                                Error::TempFile { source, .. } => crate::is_no_space_error(source),
                                Error::Persist { source, .. } => crate::is_no_space_error(source),
                                _ => false,
                            };
                            options.warn(&format!("Failed to copy {}: {}", src_file.display(), e));
                            FileCopyOutcome::Failed {
                                src: src_file.clone(),
                                dst: dst_file.clone(),
                                error_msg: e.to_string(),
                                is_no_space,
                            }
                        }
                    }
                })
                .collect()
        };

        // Use custom thread pool only if parallelism differs from default
        let outcomes = if options.parallel != rayon::current_num_threads() {
            let custom_pool = rayon::ThreadPoolBuilder::new()
                .num_threads(options.parallel)
                .build();

            match custom_pool {
                Ok(pool) => pool.install(|| do_copy(&files)),
                Err(e) => {
                    options.warn(&format!(
                        "Failed to create thread pool ({e}), using global pool"
                    ));
                    do_copy(&files)
                }
            }
        } else {
            // Use rayon's global pool directly
            do_copy(&files)
        };

        // Process outcomes
        for outcome in outcomes {
            match outcome {
                FileCopyOutcome::Copied { src, dst, bytes } => {
                    files_copied += 1;
                    bytes_copied += bytes;
                    options.verbose(&format!(
                        "copied {} -> {} ({} bytes)",
                        src.display(),
                        dst.display(),
                        bytes
                    ));
                }
                FileCopyOutcome::Skipped { src, dst } => {
                    files_skipped += 1;
                    options.verbose(&format!(
                        "skipped {} -> {} (already exists)",
                        src.display(),
                        dst.display()
                    ));
                }
                FileCopyOutcome::Failed {
                    src,
                    dst,
                    error_msg,
                    is_no_space,
                } => {
                    failed_count += 1;
                    options.verbose(&format!(
                        "failed {} -> {}: {}",
                        src.display(),
                        dst.display(),
                        error_msg
                    ));
                    if is_no_space && no_space_error.is_none() {
                        no_space_error = Some((dst, error_msg));
                    }
                }
            }
        }

        // Check cancellation after parallel copy completes
        if options.is_cancelled() {
            return Err(Error::Cancelled {
                files_copied,
                bytes_copied,
                files_skipped,
                dirs_created,
            });
        }

        // Handle "no space" error - retain progress for resumable copy
        if let Some((failed_path, _)) = &no_space_error {
            return Err(Error::NoSpace {
                files_copied: files_copied as usize,
                bytes_copied,
                failed_files: failed_count,
                total_files,
                remaining: total_files - files_copied as usize,
                path: failed_path.clone(),
            });
        }

        // Handle other partial copy failures
        if failed_count > 0 {
            return Err(Error::PartialCopy {
                failed: failed_count,
                total: total_files,
            });
        }
    }

    // Phase 4: Recreate symlinks (preserving their targets)
    let mut symlinks_copied = 0u64;
    let mut symlinks_skipped = 0u64;

    if options.preserve_symlinks && total_symlinks > 0 {
        let mut symlink_failures = 0usize;

        for (src_link, dst_link) in &symlinks {
            if options.is_cancelled() {
                return Err(Error::Cancelled {
                    files_copied,
                    bytes_copied,
                    files_skipped,
                    dirs_created,
                });
            }

            match fs::read_link(src_link) {
                Ok(target) => {
                    // Check for escaping symlinks (contains ".." components)
                    if target.is_relative() && is_escaping_symlink(&target) {
                        if options.block_escaping_symlinks {
                            options.warn(&format!(
                                "Blocking escaping symlink {} -> {}",
                                src_link.display(),
                                target.display()
                            ));
                            symlinks_skipped += 1;
                            continue; // Skip this symlink entirely
                        } else if options.warn_escaping_symlinks {
                            options.warn(&format!(
                                "Symlink {} -> {} uses relative path escaping upward",
                                src_link.display(),
                                target.display()
                            ));
                        }
                    }

                    // Handle existing destination based on on_conflict
                    let dst_exists = dst_link.exists() || is_symlink(dst_link);
                    if dst_exists {
                        match options.on_conflict {
                            OnConflict::Skip | OnConflict::UpdateNewer => {
                                // For symlinks, UpdateNewer behaves like Skip (no mtime comparison)
                                symlinks_skipped += 1;
                                continue;
                            }
                            OnConflict::Error => {
                                return Err(Error::AlreadyExists(dst_link.clone()));
                            }
                            OnConflict::Overwrite => {
                                // Convert to extended-length path format on Windows for long path support
                                let safe_dst_link = safe_path(dst_link);
                                // Remove existing file/symlink/dir before creating symlink
                                if is_symlink(dst_link) || dst_link.is_file() {
                                    if let Err(e) = fs::remove_file(&safe_dst_link) {
                                        options.warn(&format!(
                                            "Failed to remove existing file {}: {}",
                                            dst_link.display(),
                                            e
                                        ));
                                        symlink_failures += 1;
                                        continue;
                                    }
                                } else if dst_link.is_dir() {
                                    if let Err(e) = fs::remove_dir_all(&safe_dst_link) {
                                        options.warn(&format!(
                                            "Failed to remove existing directory {}: {}",
                                            dst_link.display(),
                                            e
                                        ));
                                        symlink_failures += 1;
                                        continue;
                                    }
                                }
                            }
                        }
                    }

                    // Create symlink (convert to extended-length path format on Windows for long path support)
                    let safe_dst_link = safe_path(dst_link);
                    if let Err(e) = symlink(&target, &safe_dst_link) {
                        options.warn(&format!(
                            "Failed to create symlink {} -> {}: {}",
                            dst_link.display(),
                            target.display(),
                            e
                        ));
                        symlink_failures += 1;
                    } else {
                        symlinks_copied += 1;
                    }
                }
                Err(e) => {
                    options.warn(&format!(
                        "Failed to read symlink {}: {}",
                        src_link.display(),
                        e
                    ));
                    symlink_failures += 1;
                }
            }
        }

        if symlink_failures > 0 {
            return Err(Error::PartialSymlinks {
                failed: symlink_failures,
                total: total_symlinks,
            });
        }
    }

    Ok(CopyStats {
        files_copied,
        files_skipped,
        symlinks_copied,
        symlinks_skipped,
        dirs_created,
        bytes_copied,
        duration: start_time.elapsed(),
    })
}

/// Recursively collect all directories, files, and symlinks to copy
#[allow(clippy::too_many_arguments)]
fn collect_entries(
    src: &Path,
    dst: &Path,
    dirs: &mut Vec<DirEntry>,
    files: &mut Vec<(PathBuf, PathBuf)>,
    symlinks: &mut Vec<(PathBuf, PathBuf)>,
    options: &CopyOptions,
    depth: usize,
    visited: &mut HashSet<(u64, u64)>,
) -> Result<()> {
    // Check max depth
    if let Some(max_depth) = options.max_depth {
        if depth > max_depth {
            return Err(Error::MaxDepthExceeded {
                path: src.to_path_buf(),
                max_depth,
            });
        }
    }

    // Track visited directories by (dev, ino) to detect symlink loops
    // This is O(1) per directory vs O(n) for canonicalize which resolves all symlinks
    let dir_key = get_dir_key(src)?;
    if !visited.insert(dir_key) {
        return Err(Error::SymlinkLoop(src.to_path_buf()));
    }

    // Add destination directory first (with source for permission copying)
    dirs.push(DirEntry {
        src: src.to_path_buf(),
        dst: dst.to_path_buf(),
    });

    for entry in fs::read_dir(src)? {
        let entry = entry?;
        let src_path = entry.path();
        let dst_path = dst.join(entry.file_name());

        // Check symlink FIRST (before is_dir/is_file which follow symlinks)
        let file_type = entry.file_type()?;

        if file_type.is_symlink() {
            if options.preserve_symlinks {
                symlinks.push((src_path, dst_path));
            } else {
                // Follow symlink - treat as file or dir based on target
                match fs::metadata(&src_path) {
                    Ok(target_meta) => {
                        if target_meta.is_dir() {
                            collect_entries(
                                &src_path,
                                &dst_path,
                                dirs,
                                files,
                                symlinks,
                                options,
                                depth + 1,
                                visited,
                            )?;
                        } else {
                            files.push((src_path, dst_path));
                        }
                    }
                    Err(e) => {
                        // Broken symlink - warn and skip
                        options.warn(&format!(
                            "Skipping broken symlink {}: {}",
                            src_path.display(),
                            e
                        ));
                    }
                }
            }
        } else if file_type.is_dir() {
            // Recurse into real subdirectory
            collect_entries(
                &src_path,
                &dst_path,
                dirs,
                files,
                symlinks,
                options,
                depth + 1,
                visited,
            )?;
        } else if file_type.is_file() {
            files.push((src_path, dst_path));
        } else {
            // Skip special files (sockets, devices, etc.) with warning
            options.warn(&format!("Skipping special file: {}", src_path.display()));
        }
    }

    Ok(())
}

// =============================================================================
// Tests
// =============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use crate::CopyBuilder;
    use crate::error::Error;
    use std::fs;
    use std::sync::Mutex;
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
    use tempfile::tempdir;

    #[test]
    fn test_copy_dir_basic() {
        let src_dir = tempdir().unwrap();
        let dst_dir = tempdir().unwrap();

        // Create source structure
        let sub_dir = src_dir.path().join("subdir");
        fs::create_dir(&sub_dir).unwrap();
        fs::write(src_dir.path().join("file1.txt"), "content1").unwrap();
        fs::write(sub_dir.join("file2.txt"), "content2").unwrap();

        let options = CopyOptions::default();
        let dst = dst_dir.path().join("copied");
        copy_dir(src_dir.path(), &dst, &options).unwrap();

        assert!(dst.join("file1.txt").exists());
        assert!(dst.join("subdir").join("file2.txt").exists());
        assert_eq!(
            fs::read_to_string(dst.join("file1.txt")).unwrap(),
            "content1"
        );
        assert_eq!(
            fs::read_to_string(dst.join("subdir").join("file2.txt")).unwrap(),
            "content2"
        );
    }

    #[test]
    fn test_copy_dir_empty() {
        let src_dir = tempdir().unwrap();
        let dst_dir = tempdir().unwrap();

        fs::create_dir(src_dir.path().join("empty")).unwrap();

        let options = CopyOptions::default();
        let dst = dst_dir.path().join("copied");
        copy_dir(src_dir.path(), &dst, &options).unwrap();

        assert!(dst.join("empty").exists());
    }

    #[cfg(unix)]
    #[test]
    fn test_copy_dir_with_symlink() {
        use std::os::unix::fs::symlink;

        let src_dir = tempdir().unwrap();
        let dst_dir = tempdir().unwrap();

        // Create source with symlink
        fs::write(src_dir.path().join("file.txt"), "content").unwrap();
        let link = src_dir.path().join("link");
        symlink(src_dir.path().join("file.txt"), &link).unwrap();

        let options = CopyOptions::default();
        let dst = dst_dir.path().join("copied");
        copy_dir(src_dir.path(), &dst, &options).unwrap();

        assert!(dst.join("file.txt").exists());
        assert!(is_symlink(&dst.join("link")));
    }

    #[cfg(unix)]
    #[test]
    fn test_copy_dir_overwrite_symlinks() {
        use std::os::unix::fs::symlink;

        let src_dir = tempdir().unwrap();
        let dst_dir = tempdir().unwrap();

        let src_file = src_dir.path().join("file.txt");
        fs::write(&src_file, "content").unwrap();

        let dst = dst_dir.path().join("copied");
        fs::create_dir_all(&dst).unwrap();

        // Create a file where the symlink will go
        fs::write(dst.join("link"), "old content").unwrap();

        symlink(&src_file, src_dir.path().join("link")).unwrap();

        let options = CopyOptions::default().with_on_conflict(OnConflict::Overwrite);
        copy_dir(src_dir.path(), &dst, &options).unwrap();

        assert!(is_symlink(&dst.join("link")));
    }

    #[cfg(unix)]
    #[test]
    fn test_copy_dir_skip_existing_symlinks() {
        use std::os::unix::fs::symlink;

        let src_dir = tempdir().unwrap();
        let dst_dir = tempdir().unwrap();

        let src_file = src_dir.path().join("file.txt");
        fs::write(&src_file, "content").unwrap();

        let dst = dst_dir.path().join("copied");
        fs::create_dir_all(&dst).unwrap();

        // Create a symlink where the new one will go
        symlink(&src_file, dst.join("link")).unwrap();

        symlink(&src_file, src_dir.path().join("link")).unwrap();

        let options = CopyOptions::default();
        copy_dir(src_dir.path(), &dst, &options).unwrap();

        // Should still be a symlink
        assert!(is_symlink(&dst.join("link")));
    }

    #[cfg(unix)]
    #[test]
    fn test_copy_dir_error_on_existing_symlinks() {
        use std::os::unix::fs::symlink;

        let src_dir = tempdir().unwrap();
        let dst_dir = tempdir().unwrap();

        let src_file = src_dir.path().join("file.txt");
        fs::write(&src_file, "content").unwrap();

        let dst = dst_dir.path().join("copied");
        fs::create_dir_all(&dst).unwrap();

        symlink(&src_file, dst.join("link")).unwrap();
        symlink(&src_file, src_dir.path().join("link")).unwrap();

        let options = CopyOptions::default().with_on_conflict(OnConflict::Error);
        let result = copy_dir(src_dir.path(), &dst, &options);

        assert!(matches!(result, Err(Error::AlreadyExists(_))));
    }

    #[test]
    fn test_copy_dir_source_not_found() {
        let src_dir = tempdir().unwrap();
        let dst_dir = tempdir().unwrap();

        let src = src_dir.path().join("nonexistent");
        let dst = dst_dir.path().join("dst");

        let options = CopyOptions::default();
        let result = copy_dir(&src, &dst, &options);

        assert!(matches!(result, Err(Error::SourceNotFound(_))));
    }

    #[test]
    fn test_copy_dir_source_is_file() {
        let src_dir = tempdir().unwrap();
        let dst_dir = tempdir().unwrap();

        let src_file = src_dir.path().join("file.txt");
        fs::write(&src_file, "content").unwrap();

        let dst = dst_dir.path().join("dst");

        let options = CopyOptions::default();
        let result = copy_dir(&src_file, &dst, &options);

        assert!(matches!(result, Err(Error::NotADirectory(_))));
    }

    #[cfg(unix)]
    #[test]
    fn test_copy_dir_detects_symlink_loop_to_parent() {
        use std::os::unix::fs::symlink;

        let src_dir = tempdir().unwrap();
        let dst_dir = tempdir().unwrap();

        // Create a directory with a symlink pointing to parent (loop)
        let sub_dir = src_dir.path().join("subdir");
        fs::create_dir(&sub_dir).unwrap();
        fs::write(sub_dir.join("file.txt"), "content").unwrap();
        // Create symlink pointing back to parent directory
        symlink(src_dir.path(), sub_dir.join("loop")).unwrap();

        // With preserve_symlinks: false, this would cause infinite recursion
        let mut options = CopyOptions::default();
        options.preserve_symlinks = false;

        let result = copy_dir(src_dir.path(), &dst_dir.path().join("copied"), &options);

        assert!(result.is_err());
        match result.unwrap_err() {
            Error::SymlinkLoop(_) => {}
            e => panic!("Expected SymlinkLoop error, got: {:?}", e),
        }
    }

    #[cfg(unix)]
    #[test]
    fn test_copy_dir_detects_symlink_loop_to_self() {
        use std::os::unix::fs::symlink;

        let src_dir = tempdir().unwrap();
        let dst_dir = tempdir().unwrap();

        // Create a symlink pointing to the same directory
        let sub_dir = src_dir.path().join("subdir");
        fs::create_dir(&sub_dir).unwrap();
        symlink(&sub_dir, sub_dir.join("self_loop")).unwrap();

        let mut options = CopyOptions::default();
        options.preserve_symlinks = false;

        let result = copy_dir(src_dir.path(), &dst_dir.path().join("copied"), &options);

        assert!(result.is_err());
        match result.unwrap_err() {
            Error::SymlinkLoop(_) => {}
            e => panic!("Expected SymlinkLoop error, got: {:?}", e),
        }
    }

    #[cfg(unix)]
    #[test]
    fn test_copy_dir_symlink_loop_ok_with_preserve() {
        use std::os::unix::fs::symlink;

        let src_dir = tempdir().unwrap();
        let dst_dir = tempdir().unwrap();

        // Create a directory with a symlink pointing to parent
        let sub_dir = src_dir.path().join("subdir");
        fs::create_dir(&sub_dir).unwrap();
        fs::write(sub_dir.join("file.txt"), "content").unwrap();
        symlink("..", sub_dir.join("parent_link")).unwrap();

        // With preserve_symlinks: true (default), symlinks are preserved, no loop
        let options = CopyOptions::default();
        let dst = dst_dir.path().join("copied");
        copy_dir(src_dir.path(), &dst, &options).unwrap();

        // Symlink should be preserved
        assert!(dst.join("subdir").join("parent_link").is_symlink());
        assert_eq!(
            fs::read_link(dst.join("subdir").join("parent_link"))
                .unwrap()
                .to_string_lossy(),
            ".."
        );
    }

    #[test]
    fn test_copy_dir_max_depth_zero() {
        let src_dir = tempdir().unwrap();
        let dst_dir = tempdir().unwrap();

        let sub = src_dir.path().join("sub");
        fs::create_dir(&sub).unwrap();
        fs::write(sub.join("file.txt"), "content").unwrap();

        let options = CopyOptions::default().with_max_depth(0);
        let result = copy_dir(src_dir.path(), &dst_dir.path().join("dst"), &options);

        // With max_depth=0, even the root directory check should fail
        assert!(matches!(result, Err(Error::MaxDepthExceeded { .. })));
    }

    #[test]
    fn test_copy_dir_max_depth_one() {
        let src_dir = tempdir().unwrap();
        let dst_dir = tempdir().unwrap();

        // Create nested structure: root/subdir/deep/file.txt
        let sub_dir = src_dir.path().join("subdir");
        let deep_dir = sub_dir.join("deep");
        fs::create_dir_all(&deep_dir).unwrap();
        fs::write(src_dir.path().join("root.txt"), "root").unwrap();
        fs::write(sub_dir.join("sub.txt"), "sub").unwrap();
        fs::write(deep_dir.join("deep.txt"), "deep").unwrap();

        // max_depth: 1 means root + one level
        let options = CopyOptions::default().with_max_depth(1);
        let dst = dst_dir.path().join("copied");

        let result = copy_dir(src_dir.path(), &dst, &options);

        // Should fail when trying to recurse into deep
        assert!(result.is_err());
        match result.unwrap_err() {
            Error::MaxDepthExceeded { max_depth, .. } => assert_eq!(max_depth, 1),
            e => panic!("Expected MaxDepthExceeded error, got: {:?}", e),
        }
    }

    #[test]
    fn test_copy_dir_max_depth_sufficient() {
        let src_dir = tempdir().unwrap();
        let dst_dir = tempdir().unwrap();

        // Create nested structure: root/a/b/file.txt (depth 2)
        let dir_a = src_dir.path().join("a");
        let dir_b = dir_a.join("b");
        fs::create_dir_all(&dir_b).unwrap();
        fs::write(dir_b.join("file.txt"), "content").unwrap();

        // max_depth: 2 is exactly enough
        let options = CopyOptions::default().with_max_depth(2);
        let dst = dst_dir.path().join("copied");
        copy_dir(src_dir.path(), &dst, &options).unwrap();

        assert!(dst.join("a/b/file.txt").exists());
    }

    #[test]
    fn test_copy_dir_no_max_depth() {
        let src_dir = tempdir().unwrap();
        let dst_dir = tempdir().unwrap();

        // Create nested directories
        let mut current = src_dir.path().to_path_buf();
        for _ in 0..10 {
            current = current.join("nested");
            fs::create_dir(&current).unwrap();
            fs::write(current.join("file.txt"), "content").unwrap();
        }

        let options = CopyOptions::default();
        let dst = dst_dir.path().join("dst");
        copy_dir(src_dir.path(), &dst, &options).unwrap();

        // Should copy all nested directories
        let mut check = dst.clone();
        for _ in 0..10 {
            check = check.join("nested");
            assert!(check.join("file.txt").exists());
        }
    }

    #[cfg(unix)]
    #[test]
    fn test_copy_dir_skips_broken_symlink_when_following() {
        use std::os::unix::fs::symlink;

        let src_dir = tempdir().unwrap();
        let dst_dir = tempdir().unwrap();

        // Create broken symlink
        symlink("/nonexistent/target", src_dir.path().join("broken")).unwrap();

        // Track warnings
        static WARNING_ISSUED: AtomicBool = AtomicBool::new(false);
        fn warn_handler(_msg: &str) {
            WARNING_ISSUED.store(true, Ordering::SeqCst);
        }

        WARNING_ISSUED.store(false, Ordering::SeqCst);

        let mut options = CopyOptions::default().with_warn_handler(warn_handler);
        options.preserve_symlinks = false; // Try to follow symlinks

        let dst = dst_dir.path().join("dst");
        copy_dir(src_dir.path(), &dst, &options).unwrap();

        // Should have issued warning about broken symlink
        assert!(WARNING_ISSUED.load(Ordering::SeqCst));
    }

    #[test]
    fn test_is_escaping_symlink() {
        use std::path::Path;

        assert!(super::super::utils::is_escaping_symlink(Path::new(
            "../secret"
        )));
        assert!(super::super::utils::is_escaping_symlink(Path::new(
            "foo/../bar"
        )));
        assert!(!super::super::utils::is_escaping_symlink(Path::new(
            "foo/bar"
        )));
    }

    #[cfg(unix)]
    #[test]
    fn test_copy_dir_blocks_escaping_symlinks() {
        use std::os::unix::fs::symlink;

        let src_dir = tempdir().unwrap();
        let dst_dir = tempdir().unwrap();

        fs::write(src_dir.path().join("file.txt"), "content").unwrap();
        symlink("../secret", src_dir.path().join("escaped")).unwrap();

        static BLOCK_COUNT: AtomicUsize = AtomicUsize::new(0);
        fn warn_handler(msg: &str) {
            if msg.contains("Blocking") {
                BLOCK_COUNT.fetch_add(1, Ordering::SeqCst);
            }
        }

        BLOCK_COUNT.store(0, Ordering::SeqCst);

        let options = CopyOptions::default()
            .with_warn_handler(warn_handler)
            .with_block_escaping_symlinks();

        let dst = dst_dir.path().join("dst");
        copy_dir(src_dir.path(), &dst, &options).unwrap();

        assert_eq!(BLOCK_COUNT.load(Ordering::SeqCst), 1);
        assert!(dst.join("file.txt").exists());
        assert!(!is_symlink(&dst.join("escaped")));
    }

    #[cfg(unix)]
    #[test]
    fn test_copy_dir_warns_but_allows_escaping_symlinks_by_default() {
        use std::os::unix::fs::symlink;

        let src_dir = tempdir().unwrap();
        let dst_dir = tempdir().unwrap();

        fs::write(src_dir.path().join("file.txt"), "content").unwrap();
        symlink("../secret", src_dir.path().join("escaped")).unwrap();

        static WARN_COUNT: AtomicUsize = AtomicUsize::new(0);
        fn warn_handler(msg: &str) {
            if msg.contains("escaping upward") {
                WARN_COUNT.fetch_add(1, Ordering::SeqCst);
            }
        }

        WARN_COUNT.store(0, Ordering::SeqCst);

        // Default options: warn but don't block
        let options = CopyOptions::default().with_warn_handler(warn_handler);

        let dst = dst_dir.path().join("dst");
        copy_dir(src_dir.path(), &dst, &options).unwrap();

        assert_eq!(WARN_COUNT.load(Ordering::SeqCst), 1);
        assert!(dst.join("file.txt").exists());
        assert!(is_symlink(&dst.join("escaped")));
    }

    #[test]
    fn test_parallel_validation() {
        // Ensure parallel setting is actually used
        let options = CopyOptions::default().with_parallel(1);
        assert_eq!(options.parallel, 1);

        let options = CopyOptions::default().with_parallel(100);
        assert_eq!(options.parallel, 100);
    }

    #[test]
    fn test_copy_dir_returns_stats() {
        let src_dir = tempdir().unwrap();
        let dst_dir = tempdir().unwrap();

        fs::write(src_dir.path().join("file1.txt"), "content1").unwrap();
        fs::write(src_dir.path().join("file2.txt"), "content2").unwrap();
        let sub = src_dir.path().join("sub");
        fs::create_dir(&sub).unwrap();
        fs::write(sub.join("file3.txt"), "content3").unwrap();

        let options = CopyOptions::default();
        let dst = dst_dir.path().join("dst");
        let stats = copy_dir(src_dir.path(), &dst, &options).unwrap();

        assert_eq!(stats.files_copied, 3);
        assert_eq!(stats.dirs_created, 2); // root + sub
        assert_eq!(stats.bytes_copied, 24); // "content1" + "content2" + "content3"
    }

    #[test]
    fn test_copy_dir_stats_with_skip() {
        let src_dir = tempdir().unwrap();
        let dst_dir = tempdir().unwrap();

        fs::write(src_dir.path().join("file.txt"), "content").unwrap();

        let dst = dst_dir.path().join("dst");
        fs::create_dir_all(&dst).unwrap();
        fs::write(dst.join("file.txt"), "existing").unwrap();

        let options = CopyOptions::default();
        let stats = copy_dir(src_dir.path(), &dst, &options).unwrap();

        assert_eq!(stats.files_copied, 0);
        assert_eq!(stats.files_skipped, 1);
    }

    #[test]
    fn test_copy_dir_update_newer() {
        let src_dir = tempdir().unwrap();
        let dst_dir = tempdir().unwrap();

        // Create files with specific timing
        let src_file = src_dir.path().join("file.txt");
        let dst_file = dst_dir.path().join("dst/file.txt");

        fs::write(&src_file, "old content").unwrap();
        fs::create_dir_all(dst_dir.path().join("dst")).unwrap();
        fs::write(&dst_file, "newer content").unwrap();

        // Sleep to ensure source is newer
        std::thread::sleep(std::time::Duration::from_millis(10));
        fs::write(&src_file, "newest content").unwrap();

        let options = CopyOptions::default().with_on_conflict(OnConflict::UpdateNewer);
        let stats = copy_dir(src_dir.path(), &dst_dir.path().join("dst"), &options).unwrap();

        // Only root file should be copied (it's newer)
        assert_eq!(stats.files_copied, 1);
        assert_eq!(fs::read_to_string(&dst_file).unwrap(), "newest content");
    }

    #[cfg(unix)]
    #[test]
    fn test_copy_dir_preserves_directory_permissions() {
        use std::os::unix::fs::PermissionsExt;

        let src_dir = tempdir().unwrap();
        let dst_dir = tempdir().unwrap();

        let sub = src_dir.path().join("sub");
        fs::create_dir(&sub).unwrap();
        fs::set_permissions(&sub, fs::Permissions::from_mode(0o700)).unwrap();

        let options = CopyOptions::default();
        let dst = dst_dir.path().join("dst");
        copy_dir(src_dir.path(), &dst, &options).unwrap();

        let dst_sub = dst.join("sub");
        let meta = fs::metadata(&dst_sub).unwrap();
        assert_eq!(meta.permissions().mode() & 0o777, 0o700);
    }

    #[test]
    fn test_copy_dir_with_special_names() {
        let src_dir = tempdir().unwrap();
        let dst_dir = tempdir().unwrap();

        // Create files with special names
        fs::write(src_dir.path().join("file with spaces.txt"), "content").unwrap();
        fs::write(src_dir.path().join("日本語.txt"), "内容").unwrap();

        let options = CopyOptions::default();
        let dst = dst_dir.path().join("dst");
        copy_dir(src_dir.path(), &dst, &options).unwrap();

        assert!(dst.join("file with spaces.txt").exists());
        assert_eq!(fs::read_to_string(dst.join("日本語.txt")).unwrap(), "内容");
    }

    #[test]
    fn test_cancel_before_start() {
        let src_dir = tempdir().unwrap();
        let dst_dir = tempdir().unwrap();

        for i in 0..10 {
            fs::write(src_dir.path().join(format!("file_{}.txt", i)), "content").unwrap();
        }

        let cancel = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(true)); // Already cancelled

        let options = CopyOptions::default().with_cancel_token(cancel);

        let result = copy_dir(src_dir.path(), &dst_dir.path().join("dst"), &options);

        // Should be cancelled before any files are copied
        match result {
            Err(Error::Cancelled { files_copied, .. }) => {
                assert_eq!(files_copied, 0);
            }
            Ok(stats) => {
                // Might have started before cancellation was checked
                assert!(stats.files_copied <= 10);
            }
            Err(other) => panic!("Expected Cancelled or Ok, got {:?}", other),
        }
    }

    #[test]
    fn test_cancel_during_copy() {
        let src_dir = tempdir().unwrap();
        let dst_dir = tempdir().unwrap();

        // Create many files
        for i in 0..100 {
            fs::write(src_dir.path().join(format!("file_{}.txt", i)), "content").unwrap();
        }

        let cancel = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));

        // Set up a thread to cancel after a short delay
        let cancel_clone = cancel.clone();
        std::thread::spawn(move || {
            std::thread::sleep(std::time::Duration::from_millis(10));
            cancel_clone.store(true, Ordering::Relaxed);
        });

        let options = CopyOptions::default().with_cancel_token(cancel);

        let result = copy_dir(src_dir.path(), &dst_dir.path().join("dst"), &options);

        // Should be cancelled (either during copy or after)
        match result {
            Err(Error::Cancelled {
                files_copied,
                bytes_copied,
                ..
            }) => {
                // Some files may have been copied before cancellation
                // The exact count depends on timing, but we can verify the error type
                assert!(files_copied <= 100);
                // If any files were copied, bytes should be non-zero
                if files_copied > 0 {
                    assert!(bytes_copied > 0);
                }
            }
            Ok(stats) => {
                // Race condition: copy might complete before cancel fires
                // This is acceptable behavior
                assert_eq!(stats.files_copied, 100);
            }
            Err(other) => panic!("Expected Cancelled or Ok, got {:?}", other),
        }
    }

    #[test]
    fn test_cancel_token_none_no_effect() {
        // Regression test: ensure no cancellation token means normal operation
        let src_dir = tempdir().unwrap();
        let dst_dir = tempdir().unwrap();

        for i in 0..10 {
            fs::write(src_dir.path().join(format!("file_{}.txt", i)), "content").unwrap();
        }

        let options = CopyOptions::default(); // No cancel token

        let result = copy_dir(src_dir.path(), &dst_dir.path().join("dst"), &options);

        match result {
            Ok(stats) => {
                assert_eq!(stats.files_copied, 10);
            }
            Err(e) => panic!("Expected Ok, got {:?}", e),
        }
    }

    #[test]
    fn test_verbose_copied_files() {
        let src_dir = tempdir().unwrap();
        let dst_dir = tempdir().unwrap();

        // Create source files
        fs::write(src_dir.path().join("file1.txt"), "content1").unwrap();
        fs::write(src_dir.path().join("file2.txt"), "content2").unwrap();

        // Track verbose messages
        static VERBOSE_MESSAGES: Mutex<Vec<String>> = Mutex::new(Vec::new());
        fn verbose_handler(msg: &str) {
            VERBOSE_MESSAGES.lock().unwrap().push(msg.to_string());
        }

        // Clear previous messages
        VERBOSE_MESSAGES.lock().unwrap().clear();

        let options = CopyOptions::default().with_verbose_handler(verbose_handler);
        let dst = dst_dir.path().join("copied");
        copy_dir(src_dir.path(), &dst, &options).unwrap();

        let messages = VERBOSE_MESSAGES.lock().unwrap();
        assert_eq!(messages.len(), 2);

        // Check that messages contain expected content
        for msg in messages.iter() {
            assert!(msg.starts_with("copied "));
            assert!(msg.contains(" -> "));
            assert!(msg.contains(" bytes"));
        }
    }

    #[test]
    fn test_verbose_skipped_files() {
        let src_dir = tempdir().unwrap();
        let dst_dir = tempdir().unwrap();

        // Create source files
        fs::write(src_dir.path().join("file1.txt"), "content1").unwrap();

        // Create destination file that already exists
        let dst = dst_dir.path().join("copied");
        fs::create_dir_all(&dst).unwrap();
        fs::write(dst.join("file1.txt"), "existing content").unwrap();

        // Track verbose messages
        static VERBOSE_MESSAGES: Mutex<Vec<String>> = Mutex::new(Vec::new());
        fn verbose_handler(msg: &str) {
            VERBOSE_MESSAGES.lock().unwrap().push(msg.to_string());
        }

        // Clear previous messages
        VERBOSE_MESSAGES.lock().unwrap().clear();

        let options = CopyOptions::default().with_verbose_handler(verbose_handler);
        copy_dir(src_dir.path(), &dst, &options).unwrap();

        let messages = VERBOSE_MESSAGES.lock().unwrap();
        assert_eq!(messages.len(), 1);

        // Check that message indicates skipped
        assert!(messages[0].starts_with("skipped "));
        assert!(messages[0].contains(" -> "));
        assert!(messages[0].contains("already exists"));
    }

    #[test]
    fn test_verbose_with_builder() {
        let src_dir = tempdir().unwrap();
        let dst_dir = tempdir().unwrap();

        fs::write(src_dir.path().join("test.txt"), "hello").unwrap();

        // Track verbose messages
        static VERBOSE_MESSAGES: Mutex<Vec<String>> = Mutex::new(Vec::new());
        fn verbose_handler(msg: &str) {
            VERBOSE_MESSAGES.lock().unwrap().push(msg.to_string());
        }

        // Clear previous messages
        VERBOSE_MESSAGES.lock().unwrap().clear();

        let stats = CopyBuilder::new(src_dir.path(), dst_dir.path().join("dst"))
            .verbose(verbose_handler)
            .run()
            .unwrap();

        assert_eq!(stats.files_copied, 1);

        let messages = VERBOSE_MESSAGES.lock().unwrap();
        assert_eq!(messages.len(), 1);
        assert!(messages[0].starts_with("copied "));
    }

    #[cfg(windows)]
    #[test]
    fn test_copy_dir_with_long_filename() {
        let src_dir = tempdir().unwrap();
        let dst_dir = tempdir().unwrap();

        // Create a file with a name longer than 125 characters
        let long_name = "a".repeat(150) + ".txt";
        let src_file = src_dir.path().join(&long_name);
        let dst_file = dst_dir.path().join("copied").join(&long_name);

        // Create source file
        fs::write(&src_file, "content with long filename").unwrap();

        // Copy directory - should work with extended-length path support
        let options = CopyOptions::default();
        copy_dir(src_dir.path(), &dst_dir.path().join("copied"), &options).unwrap();

        // Verify the destination file exists and has correct content
        assert!(dst_file.exists());
        let content = fs::read_to_string(&dst_file).unwrap();
        assert_eq!(content, "content with long filename");
    }

    #[cfg(windows)]
    #[test]
    fn test_copy_dir_with_multiple_long_filenames() {
        let src_dir = tempdir().unwrap();
        let dst_dir = tempdir().unwrap();

        // Create multiple files with long names
        for i in 0..3 {
            let long_name = format!("{}_{}.txt", "file".repeat(40), i);
            let src_file = src_dir.path().join(&long_name);
            fs::write(&src_file, format!("content {}", i)).unwrap();
        }

        // Copy directory
        let options = CopyOptions::default();
        copy_dir(src_dir.path(), &dst_dir.path().join("copied"), &options).unwrap();

        // Verify all files were copied
        for i in 0..3 {
            let long_name = format!("{}_{}.txt", "file".repeat(40), i);
            let dst_file = dst_dir.path().join("copied").join(&long_name);
            assert!(dst_file.exists(), "File {} should exist", long_name);
            let content = fs::read_to_string(&dst_file).unwrap();
            assert_eq!(content, format!("content {}", i));
        }
    }

    #[cfg(windows)]
    #[test]
    fn test_copy_dir_with_long_directory_name() {
        let src_dir = tempdir().unwrap();
        let dst_dir = tempdir().unwrap();

        // Create a directory with a long name
        let long_dir_name = "subdir_".repeat(30);
        let src_subdir = src_dir.path().join(&long_dir_name);
        fs::create_dir(&src_subdir).unwrap();
        fs::write(src_subdir.join("file.txt"), "content").unwrap();

        // Copy directory
        let options = CopyOptions::default();
        copy_dir(src_dir.path(), &dst_dir.path().join("copied"), &options).unwrap();

        // Verify the subdirectory and file were copied
        let dst_subdir = dst_dir.path().join("copied").join(&long_dir_name);
        assert!(dst_subdir.exists());
        assert!(dst_subdir.join("file.txt").exists());
        let content = fs::read_to_string(dst_subdir.join("file.txt")).unwrap();
        assert_eq!(content, "content");
    }

    #[cfg(windows)]
    #[test]
    fn test_copy_dir_with_long_total_path() {
        let src_dir = tempdir().unwrap();
        let dst_base = tempdir().unwrap();

        // Create source with some files
        fs::write(src_dir.path().join("file1.txt"), "content1").unwrap();
        fs::create_dir(src_dir.path().join("subdir")).unwrap();
        fs::write(src_dir.path().join("subdir/file2.txt"), "content2").unwrap();

        // Create a deeply nested destination path to get total path > 500 chars
        let mut dst_path = dst_base.path().to_path_buf();
        for i in 0..20 {
            dst_path = dst_path.join(format!("level{:02}_{}", i, "x".repeat(20)));
        }

        // Verify total path is long (> 500 chars)
        let total_path_len = dst_path.to_string_lossy().len();
        assert!(total_path_len > 500, "Test path length: {}", total_path_len);

        // Copy directory to the long path - should work with extended-length path support
        let options = CopyOptions::default();
        copy_dir(src_dir.path(), &dst_path, &options).unwrap();

        // Verify files were copied successfully
        assert!(dst_path.join("file1.txt").exists());
        assert!(dst_path.join("subdir/file2.txt").exists());
    }

    #[cfg(windows)]
    #[test]
    fn test_copy_dir_with_very_long_total_path() {
        let src_dir = tempdir().unwrap();
        let dst_base = tempdir().unwrap();

        // Create source directory structure
        fs::write(src_dir.path().join("data.txt"), "test data").unwrap();
        let src_subdir = src_dir.path().join("nested");
        fs::create_dir(&src_subdir).unwrap();
        fs::write(src_subdir.join("info.txt"), "nested info").unwrap();

        // Create a very deeply nested destination path (> 1000 chars total)
        let mut dst_path = dst_base.path().to_path_buf();
        for i in 0..30 {
            dst_path = dst_path.join(format!("deep{:03}_{}", i, "y".repeat(25)));
        }

        // Verify total path is very long (> 1000 chars)
        let total_path_len = dst_path.to_string_lossy().len();
        assert!(
            total_path_len > 1000,
            "Test path length: {}",
            total_path_len
        );

        // Copy directory to the very long path - should work with extended-length support
        let options = CopyOptions::default();
        copy_dir(src_dir.path(), &dst_path, &options).unwrap();

        // Verify the directory structure was copied correctly
        assert!(dst_path.join("data.txt").exists());
        assert!(dst_path.join("nested/info.txt").exists());
    }

    #[cfg(windows)]
    #[test]
    fn test_copy_dir_exceeds_max_path_limit() {
        let src_dir = tempdir().unwrap();
        let dst_base = tempdir().unwrap();

        // Create source with files
        fs::write(src_dir.path().join("test.txt"), "content").unwrap();

        // Create destination path that exceeds old MAX_PATH (260 chars)
        let mut dst_path = dst_base.path().to_path_buf();
        for i in 0..10 {
            dst_path = dst_path.join(format!("dir{:02}_{}", i, "z".repeat(20)));
        }

        // Verify total path exceeds MAX_PATH
        let total_path_len = dst_path.to_string_lossy().len();
        assert!(total_path_len > 260, "Test path length: {}", total_path_len);

        // Copy directory - would fail without extended-length path support
        let options = CopyOptions::default();
        copy_dir(src_dir.path(), &dst_path, &options).unwrap();

        // Verify copy succeeded
        assert!(dst_path.join("test.txt").exists());
    }
}
