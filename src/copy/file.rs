//! Single file copy operations.
//!
//! This module provides functions for copying individual files with
//! atomic writes, TOCTOU safety, and optional reflink support.

use crate::error::{Error, Result};
use crate::options::{CopyOptions, OnConflict};
use crate::utils::path::safe_path;
use std::fs::{self, File};
use std::io;
use std::path::Path;

#[cfg(all(feature = "reflink", any(target_os = "linux", target_os = "macos")))]
use super::reflink;
use super::utils::{copy_file_contents, is_source_newer, preserve_timestamps, remove_existing};

/// Result of a single file copy operation (internal use)
#[derive(Debug, Clone, Copy)]
pub(crate) struct FileCopyResult {
    /// Whether the file was actually copied (false = skipped)
    pub copied: bool,
    /// Number of bytes copied (0 if skipped)
    pub bytes: u64,
}

impl FileCopyResult {
    pub(crate) fn copied(bytes: u64) -> Self {
        Self {
            copied: true,
            bytes,
        }
    }

    pub(crate) fn skipped() -> Self {
        Self {
            copied: false,
            bytes: 0,
        }
    }
}

/// Copy a single file atomically
///
/// Uses temp file + rename pattern to ensure no partial files on interruption.
/// This is TOCTOU-safe using `persist_noclobber`.
///
/// # Arguments
///
/// * `src` - Source file path
/// * `dst` - Destination file path
/// * `options` - Copy options
///
/// # Returns
///
/// Returns `Ok(true)` if the file was copied, `Ok(false)` if skipped.
///
/// # Errors
///
/// Returns an error if:
/// - Source is a directory ([`Error::IsADirectory`])
/// - Destination exists and `on_conflict` is [`OnConflict::Error`] ([`Error::AlreadyExists`])
/// - IO operations fail ([`Error::Io`])
/// - Temp file creation fails ([`Error::TempFile`])
/// - Atomic rename fails ([`Error::Persist`])
#[must_use = "returns false if file was skipped, check the result"]
pub fn copy_file(src: &Path, dst: &Path, options: &CopyOptions) -> Result<bool> {
    copy_file_internal(src, dst, options).map(|r| r.copied)
}

/// Internal file copy that returns detailed result including bytes copied.
/// Used by copy_dir to collect statistics.
pub(crate) fn copy_file_internal(
    src: &Path,
    dst: &Path,
    options: &CopyOptions,
) -> Result<FileCopyResult> {
    // Get source metadata early - single stat call for all checks
    let src_meta = fs::metadata(src)?;

    // Check if source is a directory - give friendly error
    if src_meta.is_dir() {
        return Err(Error::IsADirectory(src.to_path_buf()));
    }

    let file_len = src_meta.len();

    // Handle conflict based on options
    // Use ONE symlink_metadata call to detect existence and type (avoid redundant stat calls)
    match fs::symlink_metadata(dst) {
        Ok(dst_meta) => {
            // Destination exists
            match options.on_conflict {
                OnConflict::Skip => return Ok(FileCopyResult::skipped()),
                OnConflict::Error => return Err(Error::AlreadyExists(dst.to_path_buf())),
                OnConflict::UpdateNewer => {
                    // Only copy if source is newer than destination
                    if !is_source_newer(&src_meta, &dst_meta) {
                        return Ok(FileCopyResult::skipped());
                    }
                    // Source is newer - remove existing and continue
                    remove_existing(dst, &dst_meta)?;
                }
                OnConflict::Overwrite => {
                    // Remove existing file/symlink/dir before copying
                    remove_existing(dst, &dst_meta)?;
                }
            }
        }
        Err(e) if e.kind() == io::ErrorKind::NotFound => {
            // Destination doesn't exist - proceed with copy
        }
        Err(e) => return Err(e.into()),
    }

    // Try reflink first (instant copy on CoW filesystems like Btrfs, XFS, APFS)
    // Only attempt on Linux/macOS where reflink is supported
    #[cfg(all(feature = "reflink", any(target_os = "linux", target_os = "macos")))]
    if reflink::supports_reflink(dst.parent().unwrap_or(dst))
        && reflink_copy::reflink(src, dst).is_ok()
    {
        // Reflink succeeded - it preserves source attributes by default
        // We need to handle preserve_* options appropriately

        if options.preserve_permissions {
            // Reflink already copied permissions, but re-set to ensure consistency
            let _ = fs::set_permissions(dst, src_meta.permissions());
        } else {
            // Reset to default permissions (apply umask)
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                // Set 0o666 which will automatically apply umask
                let _ = fs::set_permissions(dst, fs::Permissions::from_mode(0o666));
            }
        }

        if options.preserve_timestamps {
            let _ = preserve_timestamps(&src_meta, dst);
        } else {
            // Reset to current time
            let now = filetime::FileTime::now();
            let _ = filetime::set_file_times(dst, now, now);
        }

        return Ok(FileCopyResult::copied(file_len));
    }

    // Open source file
    let src_file = File::open(src)?;

    // Create temp file in destination directory for atomic rename
    let dst_parent = dst.parent().unwrap_or(Path::new("."));

    // On Windows, convert to extended-length path format to support long paths (>260 chars)
    // This is critical for files with long names or deeply nested directories
    let safe_dst_parent = safe_path(dst_parent);

    // Create temp file with appropriate permissions
    let temp_file = if options.preserve_permissions {
        // Use default tempfile creation (0o600), will set source permissions later
        tempfile::NamedTempFile::new_in(&safe_dst_parent).map_err(|e| Error::TempFile {
            path: dst_parent.to_path_buf(),
            source: e,
        })?
    } else {
        // Use tempfile::Builder to set default permissions at creation time
        // This avoids an extra chmod syscall
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            tempfile::Builder::new()
                .permissions(fs::Permissions::from_mode(0o666))
                .tempfile_in(&safe_dst_parent)
                .map_err(|e| Error::TempFile {
                    path: dst_parent.to_path_buf(),
                    source: e,
                })?
        }
        #[cfg(not(unix))]
        {
            tempfile::NamedTempFile::new_in(&safe_dst_parent).map_err(|e| Error::TempFile {
                path: dst_parent.to_path_buf(),
                source: e,
            })?
        }
    };

    // Copy file contents using best available method (zero-copy on Linux)
    let bytes_copied = copy_file_contents(&src_file, temp_file.as_file(), file_len)?;

    // Ensure data is on disk before rename
    if options.fsync {
        temp_file.as_file().sync_all()?;
    }

    // Set source file permissions if preserving
    if options.preserve_permissions {
        let perms = src_meta.permissions();
        fs::set_permissions(temp_file.path(), perms)?;
    }

    // Convert destination to extended-length path format on Windows for the persist operation
    // This is necessary when the destination path is very long (>260 chars on Windows)
    let safe_dst = safe_path(dst);

    // Atomic rename
    // - Overwrite/UpdateNewer mode: use persist() to overwrite any file created in the TOCTOU window
    // - Skip/Error mode: use persist_noclobber() to detect race conditions
    let persist_result = if options.on_conflict == OnConflict::Overwrite
        || options.on_conflict == OnConflict::UpdateNewer
    {
        temp_file.persist(&safe_dst).map_err(|e| Error::Persist {
            path: dst.to_path_buf(),
            source: e.error,
        })
    } else {
        match temp_file.persist_noclobber(&safe_dst) {
            Ok(f) => Ok(f),
            Err(e) => {
                if e.error.kind() == std::io::ErrorKind::AlreadyExists {
                    // Destination was created by another process
                    if options.on_conflict == OnConflict::Skip {
                        return Ok(FileCopyResult::skipped());
                    } else {
                        return Err(Error::AlreadyExists(dst.to_path_buf()));
                    }
                } else {
                    Err(Error::Persist {
                        path: dst.to_path_buf(),
                        source: e.error,
                    })
                }
            }
        }
    };

    persist_result?;

    // Preserve timestamps after successful copy
    if options.preserve_timestamps {
        // Ignore timestamp errors - they're not critical
        let _ = preserve_timestamps(&src_meta, dst);
    }

    // Preserve Windows file attributes (hidden, system, etc.)
    #[cfg(windows)]
    if options.preserve_windows_attributes {
        crate::win_attrs::copy_attributes(src, dst);
    }

    Ok(FileCopyResult::copied(bytes_copied))
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
    fn test_copy_file_basic() {
        let src_dir = tempdir().unwrap();
        let dst_dir = tempdir().unwrap();

        let src_file = src_dir.path().join("test.txt");
        let dst_file = dst_dir.path().join("test.txt");

        fs::write(&src_file, "hello world").unwrap();

        let options = CopyOptions::default();
        let copied = copy_file(&src_file, &dst_file, &options).unwrap();

        assert!(copied);
        assert!(dst_file.exists());
        assert_eq!(fs::read_to_string(&dst_file).unwrap(), "hello world");
    }

    #[test]
    fn test_copy_file_skip_existing() {
        let src_dir = tempdir().unwrap();
        let dst_dir = tempdir().unwrap();

        let src_file = src_dir.path().join("test.txt");
        let dst_file = dst_dir.path().join("test.txt");

        fs::write(&src_file, "new content").unwrap();
        fs::write(&dst_file, "old content").unwrap();

        let options = CopyOptions::default();
        let copied = copy_file(&src_file, &dst_file, &options).unwrap();

        assert!(!copied);
        assert_eq!(fs::read_to_string(&dst_file).unwrap(), "old content");
    }

    #[test]
    fn test_copy_file_overwrite_existing() {
        let src_dir = tempdir().unwrap();
        let dst_dir = tempdir().unwrap();

        let src_file = src_dir.path().join("test.txt");
        let dst_file = dst_dir.path().join("test.txt");

        fs::write(&src_file, "new content").unwrap();
        fs::write(&dst_file, "old content").unwrap();

        let options = CopyOptions::default().with_on_conflict(OnConflict::Overwrite);
        let copied = copy_file(&src_file, &dst_file, &options).unwrap();

        assert!(copied);
        assert_eq!(fs::read_to_string(&dst_file).unwrap(), "new content");
    }

    #[test]
    fn test_copy_file_overwrite_dir_with_file() {
        let src_dir = tempdir().unwrap();
        let dst_dir = tempdir().unwrap();

        let src_file = src_dir.path().join("test.txt");
        let dst_file = dst_dir.path().join("test.txt");

        fs::write(&src_file, "file content").unwrap();
        fs::create_dir(&dst_file).unwrap();

        let options = CopyOptions::default().with_on_conflict(OnConflict::Overwrite);
        let copied = copy_file(&src_file, &dst_file, &options).unwrap();

        assert!(copied);
        assert!(dst_file.is_file());
        assert_eq!(fs::read_to_string(&dst_file).unwrap(), "file content");
    }

    #[cfg(unix)]
    #[test]
    fn test_copy_file_overwrite_symlink() {
        use std::os::unix::fs::symlink;

        let src_dir = tempdir().unwrap();
        let dst_dir = tempdir().unwrap();

        let src_file = src_dir.path().join("test.txt");
        let dst_file = dst_dir.path().join("test.txt");
        let other_file = dst_dir.path().join("other.txt");

        fs::write(&src_file, "real content").unwrap();
        fs::write(&other_file, "other content").unwrap();
        symlink(&other_file, &dst_file).unwrap();

        let options = CopyOptions::default().with_on_conflict(OnConflict::Overwrite);
        let copied = copy_file(&src_file, &dst_file, &options).unwrap();

        assert!(copied);
        assert_eq!(fs::read_to_string(&dst_file).unwrap(), "real content");
    }

    #[test]
    fn test_copy_file_error_on_existing() {
        let src_dir = tempdir().unwrap();
        let dst_dir = tempdir().unwrap();

        let src_file = src_dir.path().join("test.txt");
        let dst_file = dst_dir.path().join("test.txt");

        fs::write(&src_file, "content").unwrap();
        fs::write(&dst_file, "existing").unwrap();

        let options = CopyOptions::default().with_on_conflict(OnConflict::Error);
        let result = copy_file(&src_file, &dst_file, &options);

        assert!(matches!(result, Err(Error::AlreadyExists(_))));
    }

    #[test]
    fn test_copy_file_source_not_found() {
        let src_dir = tempdir().unwrap();
        let dst_dir = tempdir().unwrap();

        let src_file = src_dir.path().join("nonexistent.txt");
        let dst_file = dst_dir.path().join("test.txt");

        let options = CopyOptions::default();
        let result = copy_file(&src_file, &dst_file, &options);

        assert!(result.is_err());
    }

    #[test]
    fn test_copy_file_source_is_directory() {
        let src_dir = tempdir().unwrap();
        let dst_dir = tempdir().unwrap();

        let src_subdir = src_dir.path().join("subdir");
        fs::create_dir(&src_subdir).unwrap();

        let dst_file = dst_dir.path().join("test.txt");

        let options = CopyOptions::default();
        let result = copy_file(&src_subdir, &dst_file, &options);

        assert!(matches!(result, Err(Error::IsADirectory(_))));
    }

    #[cfg(unix)]
    #[test]
    fn test_copy_file_preserves_permissions() {
        use std::os::unix::fs::PermissionsExt;

        let src_dir = tempdir().unwrap();
        let dst_dir = tempdir().unwrap();

        let src_file = src_dir.path().join("test.txt");
        let dst_file = dst_dir.path().join("test.txt");

        fs::write(&src_file, "content").unwrap();
        fs::set_permissions(&src_file, fs::Permissions::from_mode(0o644)).unwrap();

        let options = CopyOptions::default();
        copy_file(&src_file, &dst_file, &options).unwrap();

        let dst_meta = fs::metadata(&dst_file).unwrap();
        let mode = dst_meta.permissions().mode();
        assert_eq!(mode & 0o777, 0o644);
    }

    #[cfg(unix)]
    #[test]
    fn test_copy_file_no_preserve_permissions() {
        use std::os::unix::fs::PermissionsExt;

        let src_dir = tempdir().unwrap();
        let dst_dir = tempdir().unwrap();

        let src_file = src_dir.path().join("test.txt");
        let dst_file = dst_dir.path().join("test.txt");

        fs::write(&src_file, "content").unwrap();
        fs::set_permissions(&src_file, fs::Permissions::from_mode(0o600)).unwrap();

        let options = CopyOptions::default().without_permissions();
        copy_file(&src_file, &dst_file, &options).unwrap();

        let dst_meta = fs::metadata(&dst_file).unwrap();
        let mode = dst_meta.permissions().mode();
        // Should be default (umask applied), not 0o600
        assert_ne!(mode & 0o777, 0o600);
    }

    #[test]
    fn test_copy_file_without_fsync() {
        let src_dir = tempdir().unwrap();
        let dst_dir = tempdir().unwrap();

        let src_file = src_dir.path().join("test.txt");
        let dst_file = dst_dir.path().join("test.txt");

        fs::write(&src_file, "content").unwrap();

        let options = CopyOptions::default().without_fsync();
        let copied = copy_file(&src_file, &dst_file, &options).unwrap();

        assert!(copied);
        assert_eq!(fs::read_to_string(&dst_file).unwrap(), "content");
    }

    #[test]
    fn test_copy_file_with_spaces() {
        let src_dir = tempdir().unwrap();
        let dst_dir = tempdir().unwrap();

        let src_file = src_dir.path().join("file with spaces.txt");
        let dst_file = dst_dir.path().join("file with spaces.txt");

        fs::write(&src_file, "content").unwrap();

        let options = CopyOptions::default();
        let copied = copy_file(&src_file, &dst_file, &options).unwrap();

        assert!(copied);
    }

    #[test]
    fn test_copy_file_with_unicode() {
        let src_dir = tempdir().unwrap();
        let dst_dir = tempdir().unwrap();

        let src_file = src_dir.path().join("日本語ファイル.txt");
        let dst_file = dst_dir.path().join("日本語ファイル.txt");

        fs::write(&src_file, "内容").unwrap();

        let options = CopyOptions::default();
        let copied = copy_file(&src_file, &dst_file, &options).unwrap();

        assert!(copied);
        assert_eq!(fs::read_to_string(&dst_file).unwrap(), "内容");
    }

    #[test]
    fn test_copy_file_update_newer_copies_when_newer() {
        let src_dir = tempdir().unwrap();
        let dst_dir = tempdir().unwrap();

        let src_file = src_dir.path().join("test.txt");
        let dst_file = dst_dir.path().join("test.txt");

        // Create destination first
        fs::write(&dst_file, "old content").unwrap();
        std::thread::sleep(std::time::Duration::from_millis(10));
        // Create source later (newer)
        fs::write(&src_file, "new content").unwrap();

        let options = CopyOptions::default().with_on_conflict(OnConflict::UpdateNewer);
        let copied = copy_file(&src_file, &dst_file, &options).unwrap();

        assert!(copied);
        assert_eq!(fs::read_to_string(&dst_file).unwrap(), "new content");
    }

    #[test]
    fn test_copy_file_update_newer_skips_when_older() {
        let src_dir = tempdir().unwrap();
        let dst_dir = tempdir().unwrap();

        let src_file = src_dir.path().join("test.txt");
        let dst_file = dst_dir.path().join("test.txt");

        // Create source first (older)
        fs::write(&src_file, "old content").unwrap();
        std::thread::sleep(std::time::Duration::from_millis(10));
        // Create destination later (newer)
        fs::write(&dst_file, "new content").unwrap();

        let options = CopyOptions::default().with_on_conflict(OnConflict::UpdateNewer);
        let copied = copy_file(&src_file, &dst_file, &options).unwrap();

        assert!(!copied);
        assert_eq!(fs::read_to_string(&dst_file).unwrap(), "new content");
    }

    #[test]
    fn test_copy_file_update_newer_copies_when_dst_missing() {
        let src_dir = tempdir().unwrap();
        let dst_dir = tempdir().unwrap();

        let src_file = src_dir.path().join("test.txt");
        let dst_file = dst_dir.path().join("test.txt");

        fs::write(&src_file, "content").unwrap();

        let options = CopyOptions::default().with_on_conflict(OnConflict::UpdateNewer);
        let copied = copy_file(&src_file, &dst_file, &options).unwrap();

        assert!(copied);
    }

    #[test]
    fn test_copy_file_preserves_timestamps() {
        let src_dir = tempdir().unwrap();
        let dst_dir = tempdir().unwrap();

        let src_file = src_dir.path().join("test.txt");
        let dst_file = dst_dir.path().join("test.txt");

        fs::write(&src_file, "content").unwrap();

        let options = CopyOptions::default();
        copy_file(&src_file, &dst_file, &options).unwrap();

        let src_meta = fs::metadata(&src_file).unwrap();
        let dst_meta = fs::metadata(&dst_file).unwrap();

        // Timestamps should be preserved (allow small difference due to precision)
        let src_mtime = src_meta.modified().unwrap();
        let dst_mtime = dst_meta.modified().unwrap();
        let diff = src_mtime
            .duration_since(dst_mtime)
            .unwrap_or_else(|e| e.duration());
        assert!(diff.as_secs() < 2);
    }

    #[test]
    fn test_copy_file_without_timestamps() {
        let src_dir = tempdir().unwrap();
        let dst_dir = tempdir().unwrap();

        let src_file = src_dir.path().join("test.txt");
        let dst_file = dst_dir.path().join("test.txt");

        fs::write(&src_file, "content").unwrap();
        std::thread::sleep(std::time::Duration::from_millis(100));

        let options = CopyOptions::default().without_timestamps();
        copy_file(&src_file, &dst_file, &options).unwrap();

        let src_meta = fs::metadata(&src_file).unwrap();
        let dst_meta = fs::metadata(&dst_file).unwrap();

        // Without timestamp preservation, dst should be newer
        let src_mtime = src_meta.modified().unwrap();
        let dst_mtime = dst_meta.modified().unwrap();
        assert!(dst_mtime > src_mtime);
    }

    #[cfg(windows)]
    #[test]
    fn test_copy_file_preserves_hidden_attribute() {
        use std::os::windows::fs::OpenOptionsExt;
        use windows::Win32::Storage::FileSystem::FILE_ATTRIBUTE_HIDDEN;

        let src_dir = tempdir().unwrap();
        let dst_dir = tempdir().unwrap();

        let src_file = src_dir.path().join("test.txt");
        let dst_file = dst_dir.path().join("test.txt");

        // Create hidden file
        let _file = std::fs::OpenOptions::new()
            .write(true)
            .create(true)
            .attributes(FILE_ATTRIBUTE_HIDDEN.0)
            .open(&src_file)
            .unwrap();

        let options = CopyOptions::default();
        copy_file(&src_file, &dst_file, &options).unwrap();

        // Check hidden attribute is preserved
        let attrs: u32 = unsafe {
            windows::Win32::Storage::FileSystem::GetFileAttributesA(windows::core::PCSTR(
                dst_file.as_os_str().as_encoded_bytes().as_ptr(),
            ))
        };
        assert!(attrs & FILE_ATTRIBUTE_HIDDEN.0 != 0);
    }

    #[cfg(windows)]
    #[test]
    fn test_copy_file_with_long_name() {
        let src_dir = tempdir().unwrap();
        let dst_dir = tempdir().unwrap();

        // Create a filename longer than 125 characters (the reported threshold)
        let long_name = "a".repeat(150) + ".txt";
        let src_file = src_dir.path().join(&long_name);
        let dst_file = dst_dir.path().join(&long_name);

        // Create source file with long name
        fs::write(&src_file, "content with long filename").unwrap();

        // Copy the file - this should work with extended-length path support
        let options = CopyOptions::default();
        copy_file(&src_file, &dst_file, &options).unwrap();

        // Verify the destination exists and has correct content
        assert!(dst_file.exists());
        let content = fs::read_to_string(&dst_file).unwrap();
        assert_eq!(content, "content with long filename");
    }

    #[cfg(windows)]
    #[test]
    fn test_copy_file_with_very_long_name() {
        let src_dir = tempdir().unwrap();
        let dst_dir = tempdir().unwrap();

        // Create a filename close to Windows limit (255 characters)
        let long_name = "b".repeat(250) + ".txt";
        let src_file = src_dir.path().join(&long_name);
        let dst_file = dst_dir.path().join(&long_name);

        // Create source file with very long name
        fs::write(&src_file, "content with very long filename").unwrap();

        // Copy the file - this should work with extended-length path support
        let options = CopyOptions::default();
        copy_file(&src_file, &dst_file, &options).unwrap();

        // Verify the destination exists and has correct content
        assert!(dst_file.exists());
        let content = fs::read_to_string(&dst_file).unwrap();
        assert_eq!(content, "content with very long filename");
    }

    #[cfg(windows)]
    #[test]
    fn test_copy_file_with_long_name_overwrite() {
        let src_dir = tempdir().unwrap();
        let dst_dir = tempdir().unwrap();

        // Create a filename longer than 125 characters
        let long_name = "c".repeat(150) + ".txt";
        let src_file = src_dir.path().join(&long_name);
        let dst_file = dst_dir.path().join(&long_name);

        // Create source and destination files
        fs::write(&src_file, "new content").unwrap();
        fs::write(&dst_file, "old content").unwrap();

        // Copy with overwrite
        let options = CopyOptions::default().with_on_conflict(OnConflict::Overwrite);
        copy_file(&src_file, &dst_file, &options).unwrap();

        // Verify the destination has new content
        let content = fs::read_to_string(&dst_file).unwrap();
        assert_eq!(content, "new content");
    }

    #[cfg(windows)]
    #[test]
    fn test_copy_file_with_long_total_path() {
        let src_dir = tempdir().unwrap();
        let dst_base = tempdir().unwrap();

        // Create a deeply nested directory structure to get a long total path
        let mut dst_path = dst_base.path().to_path_buf();
        for i in 0..15 {
            dst_path = dst_path.join(format!("level{:02}_{}", i, "x".repeat(20)));
        }
        fs::create_dir_all(&dst_path).unwrap();

        // Add a reasonably long filename to make total path > 500 chars
        let long_name = "file_".repeat(30) + ".txt";
        let src_file = src_dir.path().join("source.txt");
        let dst_file = dst_path.join(&long_name);

        // Verify total path is long (> 500 chars)
        let total_path_len = dst_file.to_string_lossy().len();
        assert!(total_path_len > 500, "Test path length: {}", total_path_len);

        // Create source file
        fs::write(&src_file, "content with long total path").unwrap();

        // Copy the file - should work with extended-length path support
        let options = CopyOptions::default();
        copy_file(&src_file, &dst_file, &options).unwrap();

        // Verify the destination exists and has correct content
        assert!(dst_file.exists());
        let content = fs::read_to_string(&dst_file).unwrap();
        assert_eq!(content, "content with long total path");
    }

    #[cfg(windows)]
    #[test]
    fn test_copy_file_with_very_long_total_path() {
        let src_dir = tempdir().unwrap();
        let dst_base = tempdir().unwrap();

        // Create a very deeply nested directory structure
        let mut dst_path = dst_base.path().to_path_buf();
        for i in 0..30 {
            dst_path = dst_path.join(format!("nested{:03}_{}", i, "y".repeat(25)));
        }
        fs::create_dir_all(&dst_path).unwrap();

        // Add filename to make total path > 1000 chars
        let long_name = "data_".repeat(40) + ".txt";
        let src_file = src_dir.path().join("source.txt");
        let dst_file = dst_path.join(&long_name);

        // Verify total path is very long (> 1000 chars)
        let total_path_len = dst_file.to_string_lossy().len();
        assert!(
            total_path_len > 1000,
            "Test path length: {}",
            total_path_len
        );

        // Create source file
        fs::write(&src_file, "content with very long total path").unwrap();

        // Copy the file - should work with extended-length path support
        let options = CopyOptions::default();
        copy_file(&src_file, &dst_file, &options).unwrap();

        // Verify the destination exists and has correct content
        assert!(dst_file.exists());
        let content = fs::read_to_string(&dst_file).unwrap();
        assert_eq!(content, "content with very long total path");
    }

    #[cfg(windows)]
    #[test]
    fn test_copy_file_exceeds_max_path_limit() {
        let src_dir = tempdir().unwrap();
        let dst_base = tempdir().unwrap();

        // Create path that exceeds old MAX_PATH (260 chars)
        let mut dst_path = dst_base.path().to_path_buf();
        for i in 0..10 {
            dst_path = dst_path.join(format!("dir{:02}_{}", i, "z".repeat(20)));
        }
        fs::create_dir_all(&dst_path).unwrap();

        // Add filename to push total path well over 260 chars
        let long_name = "testfile_".repeat(20) + ".txt";
        let src_file = src_dir.path().join("source.txt");
        let dst_file = dst_path.join(&long_name);

        // Verify total path exceeds old MAX_PATH limit
        let total_path_len = dst_file.to_string_lossy().len();
        assert!(total_path_len > 260, "Test path length: {}", total_path_len);

        // Create source file
        fs::write(&src_file, "exceeds max path").unwrap();

        // Copy the file - would fail without extended-length path support
        let options = CopyOptions::default();
        copy_file(&src_file, &dst_file, &options).unwrap();

        // Verify the destination exists
        assert!(dst_file.exists());
        let content = fs::read_to_string(&dst_file).unwrap();
        assert_eq!(content, "exceeds max path");
    }
}
