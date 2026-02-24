//! Reflink/Copy-on-Write support detection.
//!
//! This module provides functionality to detect whether a filesystem supports
//! reflink (copy-on-write) operations, which can enable instant file copies
//! on filesystems like Btrfs, XFS, and APFS.

#[cfg(all(feature = "reflink", any(target_os = "linux", target_os = "macos")))]
use std::path::Path;

#[cfg(all(feature = "reflink", target_os = "linux"))]
mod platform {
    use std::collections::HashMap;
    use std::os::unix::fs::MetadataExt;
    use std::path::Path;
    use std::sync::Mutex;

    // CoW filesystem magic numbers (from /usr/include/linux/magic.h)
    const BTRFS_SUPER_MAGIC: i64 = 0x9123683E;
    const XFS_SUPER_MAGIC: i64 = 0x58465342;
    // Note: XFS requires reflink to be enabled at mkfs time

    // Cache: device_id -> supports_reflink
    static REFLINK_CACHE: Mutex<Option<HashMap<u64, bool>>> = Mutex::new(None);

    /// Check if the filesystem at `path` supports reflink.
    /// Results are cached by device ID to avoid repeated statfs calls.
    pub fn supports_reflink(path: &Path) -> bool {
        // Get device ID from path metadata
        let dev_id = match path
            .metadata()
            .or_else(|_| path.parent().unwrap_or(path).metadata())
        {
            Ok(m) => m.dev(),
            Err(_) => return false,
        };

        // Check cache first
        {
            let cache = REFLINK_CACHE.lock().unwrap_or_else(|e| e.into_inner());
            if let Some(ref map) = *cache {
                if let Some(&supported) = map.get(&dev_id) {
                    return supported;
                }
            }
        }

        // Detect filesystem type using statfs
        let supported = check_fs_supports_reflink(path);

        // Update cache
        {
            let mut cache = REFLINK_CACHE.lock().unwrap_or_else(|e| e.into_inner());
            let map = cache.get_or_insert_with(HashMap::new);
            map.insert(dev_id, supported);
        }

        supported
    }

    fn check_fs_supports_reflink(path: &Path) -> bool {
        use std::ffi::CString;
        use std::os::unix::ffi::OsStrExt;

        let path_cstr = match CString::new(path.as_os_str().as_bytes()) {
            Ok(s) => s,
            Err(_) => return false,
        };

        let mut statfs_buf: libc::statfs = unsafe { std::mem::zeroed() };
        let result = unsafe { libc::statfs(path_cstr.as_ptr(), &mut statfs_buf) };

        if result != 0 {
            // Try parent directory
            if let Some(parent) = path.parent() {
                let parent_cstr = match CString::new(parent.as_os_str().as_bytes()) {
                    Ok(s) => s,
                    Err(_) => return false,
                };
                let result = unsafe { libc::statfs(parent_cstr.as_ptr(), &mut statfs_buf) };
                if result != 0 {
                    return false;
                }
            } else {
                return false;
            }
        }

        let fs_type = statfs_buf.f_type;
        fs_type == BTRFS_SUPER_MAGIC || fs_type == XFS_SUPER_MAGIC
    }
}

#[cfg(all(feature = "reflink", target_os = "macos"))]
mod platform {
    use std::path::Path;

    /// On macOS, APFS supports reflink. We assume APFS for simplicity.
    /// A more robust check would use statfs and check f_fstypename.
    pub fn supports_reflink(_path: &Path) -> bool {
        // APFS is the default on modern macOS, so we optimistically try reflink
        true
    }
}

/// Check if the filesystem at the given path supports reflink operations.
///
/// Reflink enables instant copy-on-write file copies on supported filesystems
/// (Btrfs, XFS with reflink enabled, APFS).
///
/// # Platform Support
///
/// | Platform | Detection Method |
/// |----------|-----------------|
/// | Linux | Checks for Btrfs or XFS via `statfs` |
/// | macOS | Assumes APFS (optimistic) |
/// | Other | Returns `false` |
///
/// # Caching
///
/// On Linux, results are cached by device ID to avoid repeated `statfs` calls.
///
/// # Example
///
/// ```ignore
/// use parcopy::copy::reflink::supports_reflink;
/// use std::path::Path;
///
/// if supports_reflink(Path::new("/data")) {
///     println!("Reflink supported!");
/// }
/// ```
#[cfg(all(feature = "reflink", any(target_os = "linux", target_os = "macos")))]
pub fn supports_reflink(path: &Path) -> bool {
    platform::supports_reflink(path)
}

#[cfg(all(
    test,
    feature = "reflink",
    any(target_os = "linux", target_os = "macos")
))]
mod tests {
    use super::*;
    use std::path::Path;

    #[test]
    fn test_supports_reflink_returns_bool() {
        // Just verify the function returns without panicking
        let _ = supports_reflink(Path::new("."));
    }
}
