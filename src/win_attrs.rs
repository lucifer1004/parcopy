//! Windows file attribute preservation.
//!
//! On Windows, files have additional attributes beyond POSIX permissions:
//! - Hidden (FILE_ATTRIBUTE_HIDDEN)
//! - System (FILE_ATTRIBUTE_SYSTEM)
//! - Archive (FILE_ATTRIBUTE_ARCHIVE)
//! - Readonly (FILE_ATTRIBUTE_READONLY)
//! - etc.
//!
//! This module provides functions to get and set these attributes,
//! ensuring they are preserved during copy operations.
//!
//! The implementation uses a read-modify-write pattern to preserve
//! filesystem-managed attributes (like COMPRESSED, ENCRYPTED) while
//! only changing the user-controllable attributes.

use std::io;
use std::os::windows::ffi::OsStrExt;
use std::path::Path;

use crate::utils::path::to_extended_length_path;

use windows::Win32::Storage::FileSystem::{
    GetFileAttributesW, SetFileAttributesW, FILE_ATTRIBUTE_NORMAL, FILE_FLAGS_AND_ATTRIBUTES,
};

// INVALID_FILE_ATTRIBUTES is defined in WinBase.h as ((DWORD)-1), which equals 0xFFFFFFFF.
// The `windows` crate doesn't export this constant, so we define it here.
// See: https://learn.microsoft.com/en-us/windows/win32/api/fileapi/nf-fileapi-getfileattributesw
const INVALID_FILE_ATTRIBUTES: u32 = u32::MAX;

/// Mask for attributes that should be preserved during copy.
///
/// We preserve:
/// - FILE_ATTRIBUTE_READONLY (0x1)
/// - FILE_ATTRIBUTE_HIDDEN (0x2)
/// - FILE_ATTRIBUTE_SYSTEM (0x4)
/// - FILE_ATTRIBUTE_ARCHIVE (0x20)
/// - FILE_ATTRIBUTE_NOT_CONTENT_INDEXED (0x2000)
///
/// We do NOT preserve (these are filesystem-managed):
/// - FILE_ATTRIBUTE_DIRECTORY (0x10) - set by the filesystem
/// - FILE_ATTRIBUTE_NORMAL (0x80) - means "no other attributes"
/// - FILE_ATTRIBUTE_TEMPORARY (0x100) - file-specific
/// - FILE_ATTRIBUTE_SPARSE_FILE (0x200) - filesystem-specific
/// - FILE_ATTRIBUTE_REPARSE_POINT (0x400) - symlinks, handled separately
/// - FILE_ATTRIBUTE_COMPRESSED (0x800) - inherited from parent directory
/// - FILE_ATTRIBUTE_ENCRYPTED (0x4000) - requires special handling
const PRESERVE_MASK: u32 = 0x1 | 0x2 | 0x4 | 0x20 | 0x2000;

/// Convert a Path to a null-terminated wide string for Win32 API.
///
/// On Windows, this first converts the path to extended-length format
/// (\\?\ prefix) to support paths longer than MAX_PATH (260 characters).
#[inline]
fn path_to_wide(path: &Path) -> Vec<u16> {
    let extended_path = to_extended_length_path(path);
    extended_path
        .as_os_str()
        .encode_wide()
        .chain(Some(0))
        .collect()
}

/// Get file attributes from a path.
///
/// Returns the raw `DWORD` attribute value.
///
/// # Errors
///
/// Returns an IO error if the file doesn't exist or attributes cannot be read.
pub fn get_attributes(path: &Path) -> io::Result<u32> {
    let wide = path_to_wide(path);
    // SAFETY: We're passing a valid null-terminated wide string
    let attrs = unsafe { GetFileAttributesW(windows::core::PCWSTR(wide.as_ptr())) };

    // INVALID_FILE_ATTRIBUTES is 0xFFFFFFFF (u32::MAX)
    if attrs == INVALID_FILE_ATTRIBUTES {
        Err(io::Error::last_os_error())
    } else {
        Ok(attrs)
    }
}

/// Set file attributes on a path using read-modify-write pattern.
///
/// This function:
/// 1. Reads the current destination attributes
/// 2. Clears the PRESERVE_MASK bits from destination
/// 3. Sets the PRESERVE_MASK bits from source
/// 4. Writes the merged result
///
/// This ensures filesystem-managed attributes (COMPRESSED, ENCRYPTED, etc.)
/// are preserved while user-controllable attributes are copied from source.
///
/// # Arguments
///
/// * `path` - The file or directory path
/// * `src_attrs` - The source attribute value (typically from `get_attributes`)
///
/// # Errors
///
/// Returns an IO error if the attributes cannot be read or set.
pub fn set_attributes(path: &Path, src_attrs: u32) -> io::Result<()> {
    // Read current destination attributes
    let dst_attrs = get_attributes(path)?;

    // Merge: keep non-preserved bits from destination, take preserved bits from source
    // new = (dst_attrs & !PRESERVE_MASK) | (src_attrs & PRESERVE_MASK)
    let new_attrs = (dst_attrs & !PRESERVE_MASK) | (src_attrs & PRESERVE_MASK);

    // If the result would be 0, we need to set FILE_ATTRIBUTE_NORMAL
    // (Windows requires at least one attribute bit set)
    let attrs_to_set = if new_attrs == 0 {
        FILE_ATTRIBUTE_NORMAL.0
    } else {
        new_attrs
    };

    // Skip if nothing changed
    if attrs_to_set == dst_attrs {
        return Ok(());
    }

    let wide = path_to_wide(path);
    // SAFETY: We're passing a valid null-terminated wide string
    let result = unsafe {
        SetFileAttributesW(
            windows::core::PCWSTR(wide.as_ptr()),
            FILE_FLAGS_AND_ATTRIBUTES(attrs_to_set),
        )
    };

    if result.is_ok() {
        Ok(())
    } else {
        Err(io::Error::last_os_error())
    }
}

/// Copy file attributes from source to destination.
///
/// This is a convenience function that combines `get_attributes` and `set_attributes`.
/// Errors are intentionally ignored to match the behavior of timestamp preservation.
///
/// # Arguments
///
/// * `src` - Source file path
/// * `dst` - Destination file path
pub fn copy_attributes(src: &Path, dst: &Path) {
    if let Ok(attrs) = get_attributes(src) {
        let _ = set_attributes(dst, attrs);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::tempdir;

    /// Helper to set raw attributes directly for test setup
    fn set_raw_attributes(path: &Path, attrs: u32) -> io::Result<()> {
        let wide = path_to_wide(path);
        let result = unsafe {
            SetFileAttributesW(
                windows::core::PCWSTR(wide.as_ptr()),
                FILE_FLAGS_AND_ATTRIBUTES(attrs),
            )
        };
        if result.is_ok() {
            Ok(())
        } else {
            Err(io::Error::last_os_error())
        }
    }

    #[test]
    fn test_get_set_hidden_attribute() {
        let dir = tempdir().unwrap();
        let file = dir.path().join("test.txt");
        fs::write(&file, "content").unwrap();

        // Get initial attributes
        let initial_attrs = get_attributes(&file).unwrap();
        assert_eq!(
            initial_attrs & 0x2,
            0,
            "File should not be hidden initially"
        );

        // Set hidden attribute (simulating copy from hidden source)
        set_attributes(&file, 0x2).unwrap();

        // Verify hidden attribute is set
        let attrs = get_attributes(&file).unwrap();
        assert_ne!(attrs & 0x2, 0, "File should be hidden");
    }

    #[test]
    fn test_copy_attributes_preserves_hidden() {
        let dir = tempdir().unwrap();
        let src = dir.path().join("src.txt");
        let dst = dir.path().join("dst.txt");

        fs::write(&src, "content").unwrap();
        fs::write(&dst, "content").unwrap();

        // Make source hidden using raw set
        set_raw_attributes(&src, 0x2).unwrap();

        // Copy attributes
        copy_attributes(&src, &dst);

        // Verify destination is now hidden
        let dst_attrs = get_attributes(&dst).unwrap();
        assert_ne!(dst_attrs & 0x2, 0, "Destination should be hidden");
    }

    #[test]
    fn test_preserve_mask_excludes_directory() {
        // Directory attribute (0x10) should not be in the mask
        assert_eq!(PRESERVE_MASK & 0x10, 0);
    }

    #[test]
    fn test_clears_hidden_when_source_not_hidden() {
        let dir = tempdir().unwrap();
        let file = dir.path().join("test.txt");
        fs::write(&file, "content").unwrap();

        // Make file hidden initially using raw set
        set_raw_attributes(&file, 0x2).unwrap();
        assert_ne!(
            get_attributes(&file).unwrap() & 0x2,
            0,
            "File should be hidden"
        );

        // Now "copy" from a source with no hidden attribute (attrs = 0)
        set_attributes(&file, 0).unwrap();

        // Hidden bit should be cleared
        let attrs = get_attributes(&file).unwrap();
        assert_eq!(attrs & 0x2, 0, "Hidden bit should be cleared");
    }

    #[test]
    fn test_preserves_non_mask_attributes() {
        let dir = tempdir().unwrap();
        let file = dir.path().join("test.txt");
        fs::write(&file, "content").unwrap();

        // Get baseline attributes
        let baseline = get_attributes(&file).unwrap();

        // Set hidden attribute from "source"
        set_attributes(&file, 0x2).unwrap();

        // Verify hidden is set
        let attrs = get_attributes(&file).unwrap();
        assert_ne!(attrs & 0x2, 0, "Hidden should be set");

        // Non-preserved bits should remain unchanged
        // (baseline likely has ARCHIVE set by default)
        let non_preserved_baseline = baseline & !PRESERVE_MASK;
        let non_preserved_after = attrs & !PRESERVE_MASK;
        assert_eq!(
            non_preserved_baseline, non_preserved_after,
            "Non-preserved attributes should remain unchanged"
        );
    }

    #[test]
    fn test_set_attributes_handles_empty_result() {
        let dir = tempdir().unwrap();
        let file = dir.path().join("test.txt");
        fs::write(&file, "content").unwrap();

        // Clear all preserved attributes
        set_attributes(&file, 0).unwrap();

        // File should still be readable (FILE_ATTRIBUTE_NORMAL is set internally)
        let attrs = get_attributes(&file).unwrap();
        // Either NORMAL (0x80) is set, or some non-preserved attribute remains
        assert!(attrs != 0, "Attributes should not be zero");
    }

    #[test]
    fn test_multiple_preserved_attributes() {
        let dir = tempdir().unwrap();
        let file = dir.path().join("test.txt");
        fs::write(&file, "content").unwrap();

        // Set multiple preserved attributes: hidden + system + readonly
        let src_attrs = 0x1 | 0x2 | 0x4; // READONLY | HIDDEN | SYSTEM
        set_attributes(&file, src_attrs).unwrap();

        let attrs = get_attributes(&file).unwrap();
        assert_ne!(attrs & 0x1, 0, "READONLY should be set");
        assert_ne!(attrs & 0x2, 0, "HIDDEN should be set");
        assert_ne!(attrs & 0x4, 0, "SYSTEM should be set");
    }

    #[test]
    fn test_directory_attribute_preservation() {
        let dir = tempdir().unwrap();
        let subdir = dir.path().join("subdir");
        fs::create_dir(&subdir).unwrap();

        // Get baseline - should have DIRECTORY bit (0x10)
        let baseline = get_attributes(&subdir).unwrap();
        assert_ne!(baseline & 0x10, 0, "Should have DIRECTORY attribute");

        // Set hidden attribute
        set_attributes(&subdir, 0x2).unwrap();

        // Verify hidden is set AND directory bit is preserved
        let attrs = get_attributes(&subdir).unwrap();
        assert_ne!(attrs & 0x2, 0, "Hidden should be set");
        assert_ne!(attrs & 0x10, 0, "DIRECTORY attribute should be preserved");
    }

    #[test]
    fn test_merge_logic_preserves_non_mask_bits() {
        // This test verifies the merge formula:
        // new = (dst & !PRESERVE_MASK) | (src & PRESERVE_MASK)
        //
        // We can't easily set COMPRESSED/ENCRYPTED in tests, but we can
        // verify the logic by checking that DIRECTORY (0x10) survives.

        let dir = tempdir().unwrap();
        let subdir = dir.path().join("testdir");
        fs::create_dir(&subdir).unwrap();

        // Baseline: DIRECTORY is set (0x10), possibly ARCHIVE too
        let baseline = get_attributes(&subdir).unwrap();
        let baseline_non_mask = baseline & !PRESERVE_MASK;

        // Step 1: Set HIDDEN | SYSTEM
        set_attributes(&subdir, 0x2 | 0x4).unwrap();
        let after_set = get_attributes(&subdir).unwrap();

        // Verify preserved bits changed
        assert_ne!(after_set & 0x2, 0, "HIDDEN should be set");
        assert_ne!(after_set & 0x4, 0, "SYSTEM should be set");

        // Verify non-preserved bits unchanged
        assert_eq!(
            after_set & !PRESERVE_MASK,
            baseline_non_mask,
            "Non-mask bits should survive setting preserved bits"
        );

        // Step 2: Clear HIDDEN, keep SYSTEM
        set_attributes(&subdir, 0x4).unwrap();
        let after_clear = get_attributes(&subdir).unwrap();

        // Verify HIDDEN cleared, SYSTEM remains
        assert_eq!(after_clear & 0x2, 0, "HIDDEN should be cleared");
        assert_ne!(after_clear & 0x4, 0, "SYSTEM should remain");

        // Verify non-preserved bits still unchanged
        assert_eq!(
            after_clear & !PRESERVE_MASK,
            baseline_non_mask,
            "Non-mask bits should survive clearing preserved bits"
        );
    }

    #[test]
    fn test_copy_attributes_overwrites_preserved_bits() {
        let dir = tempdir().unwrap();
        let src = dir.path().join("src.txt");
        let dst = dir.path().join("dst.txt");

        fs::write(&src, "source").unwrap();
        fs::write(&dst, "dest").unwrap();

        // Make source hidden + readonly
        set_raw_attributes(&src, 0x1 | 0x2).unwrap();

        // Make destination have different preserved attrs (system + archive)
        set_raw_attributes(&dst, 0x4 | 0x20).unwrap();

        // Copy attributes from source to destination
        copy_attributes(&src, &dst);

        let dst_after = get_attributes(&dst).unwrap();

        // Destination should now have source's preserved attrs
        assert_ne!(dst_after & 0x1, 0, "READONLY should be copied from src");
        assert_ne!(dst_after & 0x2, 0, "HIDDEN should be copied from src");
        assert_eq!(dst_after & 0x4, 0, "SYSTEM should be cleared (not in src)");
        // ARCHIVE (0x20) is in PRESERVE_MASK, so it should also be cleared
        // since source doesn't have it (source only has 0x1 | 0x2)
        assert_eq!(
            dst_after & 0x20,
            0,
            "ARCHIVE should be cleared (not in src)"
        );
    }
}
