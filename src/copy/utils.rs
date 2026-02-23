//! Utility functions for file copy operations.
//!
//! This module contains helper functions used by the file and directory
//! copy operations, including symlink handling, timestamp preservation,
//! and platform-specific utilities.

use filetime::{set_file_times, FileTime};
use std::fs::{self, Metadata};
use std::io;
use std::path::Path;

// =============================================================================
// File content copying
// =============================================================================

/// Efficiently copy file contents using the best available method.
///
/// On Linux 4.5+, uses `copy_file_range` for zero-copy kernel-to-kernel transfer.
/// Falls back to `std::io::copy` on other platforms or on error.
pub(crate) fn copy_file_contents(
    src: &std::fs::File,
    dst: &std::fs::File,
    len: u64,
) -> io::Result<u64> {
    #[cfg(target_os = "linux")]
    {
        copy_file_range_all(src, dst, len)
    }
    #[cfg(not(target_os = "linux"))]
    {
        use std::io::BufReader;
        let _ = len; // unused on non-Linux
        io::copy(&mut BufReader::new(src), &mut &*dst)
    }
}

/// Linux-specific: copy using copy_file_range(2) syscall.
///
/// This is a zero-copy operation - data never enters userspace.
/// Falls back to io::copy if copy_file_range fails (e.g., cross-filesystem).
#[cfg(target_os = "linux")]
fn copy_file_range_all(src: &std::fs::File, dst: &std::fs::File, len: u64) -> io::Result<u64> {
    use std::os::unix::io::AsRawFd;

    let src_fd = src.as_raw_fd();
    let dst_fd = dst.as_raw_fd();
    let mut remaining = len;
    let mut copied: u64 = 0;

    while remaining > 0 {
        // copy_file_range can copy up to 2GB at a time, but we use smaller chunks
        // to allow progress reporting and avoid holding kernel resources too long
        let chunk_size = remaining.min(128 * 1024 * 1024) as usize; // 128MB chunks

        // SAFETY: We're passing valid file descriptors and null offsets (use current position)
        let result = unsafe {
            libc::copy_file_range(
                src_fd,
                std::ptr::null_mut(), // use current offset
                dst_fd,
                std::ptr::null_mut(), // use current offset
                chunk_size,
                0, // flags (reserved, must be 0)
            )
        };

        if result < 0 {
            let err = io::Error::last_os_error();
            // EXDEV: cross-device, ENOSYS: not supported, EINVAL: fs doesn't support it
            // Fall back to userspace copy
            if copied == 0
                && matches!(
                    err.raw_os_error(),
                    Some(libc::EXDEV)
                        | Some(libc::ENOSYS)
                        | Some(libc::EINVAL)
                        | Some(libc::EOPNOTSUPP)
                )
            {
                use std::io::BufReader;
                return io::copy(&mut BufReader::new(src), &mut &*dst);
            }
            return Err(err);
        }

        if result == 0 {
            // EOF reached (file may have been truncated)
            break;
        }

        let bytes_copied = result as u64;
        copied += bytes_copied;
        remaining = remaining.saturating_sub(bytes_copied);
    }

    Ok(copied)
}

// =============================================================================
// Symlink utilities
// =============================================================================

/// Helper to check if path is a symlink without following it
#[inline]
pub(crate) fn is_symlink(path: &Path) -> bool {
    fs::symlink_metadata(path)
        .map(|m| m.file_type().is_symlink())
        .unwrap_or(false)
}

/// Check if a symlink target contains ".." components that could escape upward
///
/// Returns true if any path component is ".."
#[inline]
pub(crate) fn is_escaping_symlink(target: &Path) -> bool {
    use std::path::Component;
    target
        .components()
        .any(|c| matches!(c, Component::ParentDir))
}

#[cfg(unix)]
pub(crate) use std::os::unix::fs::symlink;

#[cfg(not(unix))]
pub(crate) fn symlink(_target: &Path, _link: &Path) -> std::io::Result<()> {
    Err(std::io::Error::new(
        std::io::ErrorKind::Unsupported,
        "Symlinks not supported on this platform",
    ))
}

// =============================================================================
// Directory utilities
// =============================================================================

/// Get a unique key for a directory based on device and inode.
///
/// This is used for cycle detection and is much faster than canonicalize().
/// On Unix, returns (dev, ino). On non-Unix, falls back to a hash of the path.
#[cfg(unix)]
pub(crate) fn get_dir_key(path: &Path) -> io::Result<(u64, u64)> {
    use std::os::unix::fs::MetadataExt;
    let meta = fs::metadata(path)?;
    Ok((meta.dev(), meta.ino()))
}

#[cfg(not(unix))]
pub(crate) fn get_dir_key(path: &Path) -> io::Result<(u64, u64)> {
    use std::collections::hash_map::DefaultHasher;
    use std::hash::{Hash, Hasher};
    // Fallback: use canonicalized path hash (less efficient but correct)
    let canonical = path.canonicalize().unwrap_or_else(|_| path.to_path_buf());
    let mut hasher = DefaultHasher::new();
    canonical.hash(&mut hasher);
    let hash = hasher.finish();
    Ok((0, hash))
}

/// Directory entry with source and destination paths
pub(crate) struct DirEntry {
    pub src: std::path::PathBuf,
    pub dst: std::path::PathBuf,
}

// =============================================================================
// Metadata and timestamp utilities
// =============================================================================

/// Check if source is newer than destination based on mtime
#[inline]
pub(crate) fn is_source_newer(src_meta: &Metadata, dst_meta: &Metadata) -> bool {
    // Compare modification times
    match (src_meta.modified(), dst_meta.modified()) {
        (Ok(src_mtime), Ok(dst_mtime)) => src_mtime > dst_mtime,
        // If we can't get mtime, assume source is newer (conservative: do the copy)
        _ => true,
    }
}

/// Preserve file timestamps (mtime and atime)
pub(crate) fn preserve_timestamps(src_meta: &Metadata, dst: &Path) -> io::Result<()> {
    let mtime = FileTime::from_last_modification_time(src_meta);
    let atime = FileTime::from_last_access_time(src_meta);
    set_file_times(dst, atime, mtime)
}

/// Remove an existing file, symlink, or directory at the given path
#[inline]
pub(crate) fn remove_existing(path: &Path, meta: &Metadata) -> io::Result<()> {
    let ft = meta.file_type();
    if ft.is_symlink() || ft.is_file() {
        fs::remove_file(path)
    } else if ft.is_dir() {
        fs::remove_dir_all(path)
    } else {
        Ok(())
    }
}

// =============================================================================
// Tests
// =============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::tempdir;

    #[test]
    fn test_is_symlink() {
        let dir = tempdir().unwrap();

        // Create a regular file
        let file = dir.path().join("file.txt");
        fs::write(&file, "content").unwrap();
        assert!(!is_symlink(&file));

        // Create a symlink
        #[cfg(unix)]
        {
            use std::os::unix::fs::symlink;
            let link = dir.path().join("link");
            symlink(&file, &link).unwrap();
            assert!(is_symlink(&link));
            assert!(!is_symlink(&file));
        }
    }

    #[test]
    fn test_is_escaping_symlink() {
        assert!(is_escaping_symlink(Path::new("../secret")));
        assert!(is_escaping_symlink(Path::new("foo/../bar")));
        assert!(is_escaping_symlink(Path::new("foo/bar/..")));
        assert!(!is_escaping_symlink(Path::new("foo/bar")));
        assert!(!is_escaping_symlink(Path::new("/absolute/path")));
    }

    #[test]
    fn test_is_source_newer() {
        let dir = tempdir().unwrap();

        let file1 = dir.path().join("file1.txt");
        let file2 = dir.path().join("file2.txt");

        fs::write(&file1, "content1").unwrap();
        std::thread::sleep(std::time::Duration::from_millis(10));
        fs::write(&file2, "content2").unwrap();

        let meta1 = fs::metadata(&file1).unwrap();
        let meta2 = fs::metadata(&file2).unwrap();

        // file2 was created after file1, so file2 is newer
        assert!(is_source_newer(&meta2, &meta1));
        assert!(!is_source_newer(&meta1, &meta2));
    }

    #[test]
    fn test_remove_existing_file() {
        let dir = tempdir().unwrap();
        let file = dir.path().join("file.txt");
        fs::write(&file, "content").unwrap();

        let meta = fs::metadata(&file).unwrap();
        assert!(file.exists());
        remove_existing(&file, &meta).unwrap();
        assert!(!file.exists());
    }

    #[test]
    fn test_remove_existing_dir() {
        let dir = tempdir().unwrap();
        let subdir = dir.path().join("subdir");
        fs::create_dir(&subdir).unwrap();
        fs::write(subdir.join("file.txt"), "content").unwrap();

        let meta = fs::metadata(&subdir).unwrap();
        assert!(subdir.exists());
        remove_existing(&subdir, &meta).unwrap();
        assert!(!subdir.exists());
    }

    #[test]
    fn test_get_dir_key() {
        let dir = tempdir().unwrap();
        let key = get_dir_key(dir.path()).unwrap();
        // Key should be consistent
        let key2 = get_dir_key(dir.path()).unwrap();
        assert_eq!(key, key2);
    }
}
