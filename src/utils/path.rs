//! Path utilities for cross-platform compatibility.
//!
//! This module provides utilities for handling path-related differences
//! between operating systems, particularly Windows long path support.

use std::path::{Path, PathBuf};

/// Convert a path to an extended-length path format on Windows.
///
/// On Windows, the default maximum path length is 260 characters (MAX_PATH).
/// By using the extended-length path syntax (prefixing with `\\?\`), paths
/// can be up to 32,767 characters.
///
/// This function:
/// - On Windows, converts paths to extended-length format if needed
/// - On other platforms, returns the path unchanged
///
/// # Windows Path Conversion
///
/// The conversion follows these rules:
/// - Absolute paths like `C:\path` become `\\?\C:\path`
/// - UNC paths like `\\server\share\path` become `\\?\UNC\server\share\path`
/// - Relative paths are first converted to absolute, then prefixed
///
/// # Example
///
/// ```no_run
/// use std::path::Path;
/// use parcopy::utils::path::to_extended_length_path;
///
/// let path = Path::new(r"C:\very\long\path\...");
/// let extended = to_extended_length_path(path);
/// // On Windows: extended = r"\\?\C:\very\long\path\..."
/// // On other platforms: extended is unchanged
/// ```
#[cfg(windows)]
pub fn to_extended_length_path(path: &Path) -> PathBuf {
    use std::ffi::OsString;
    use std::os::windows::ffi::OsStringExt;

    // Check if the path is already in extended-length format
    let path_str = path.as_os_str().to_string_lossy();
    if path_str.starts_with(r"\\?\") {
        return path.to_path_buf();
    }

    // Check if it's a UNC path (starts with \\)
    if path_str.starts_with(r"\\") {
        // Convert \\server\share\path to \\?\UNC\server\share\path
        let without_prefix = &path_str[2..];
        let extended = format!(r"\\?\UNC{}", without_prefix);
        return PathBuf::from(extended);
    }

    // For relative paths, canonicalize first
    // For absolute paths, just add the prefix
    let absolute_path = if path.is_absolute() {
        path.to_path_buf()
    } else {
        // Try to canonicalize, but if it fails (e.g., path doesn't exist yet),
        // just make it absolute relative to current directory
        match std::fs::canonicalize(path) {
            Ok(canonical) => canonical,
            Err(_) => {
                // Get current directory and join with the relative path
                match std::env::current_dir() {
                    Ok(cwd) => cwd.join(path),
                    Err(_) => path.to_path_buf(),
                }
            }
        }
    };

    // Add \\?\ prefix
    let extended = format!(r"\\?\{}", absolute_path.display());
    PathBuf::from(extended)
}

/// Convert a path to an extended-length path format on Windows.
///
/// On non-Windows platforms, this simply returns a clone of the input path.
#[cfg(not(windows))]
#[allow(dead_code)]
pub fn to_extended_length_path(path: &Path) -> PathBuf {
    path.to_path_buf()
}

/// Check if a path might exceed Windows MAX_PATH limitations.
///
/// Returns `true` if the path is longer than 200 characters, which is a
/// conservative threshold that accounts for temp file names and other
/// path components that might be added.
///
/// On non-Windows platforms, this always returns `false`.
#[cfg(windows)]
pub fn is_long_path(path: &Path) -> bool {
    // Windows MAX_PATH is 260, but we use 200 as a conservative threshold
    // to account for temp file names (~30 chars) and safety margin
    path.as_os_str().len() > 200
}

/// Check if a path might exceed Windows MAX_PATH limitations.
///
/// On non-Windows platforms, this always returns `false`.
#[cfg(not(windows))]
#[allow(dead_code)]
pub fn is_long_path(_path: &Path) -> bool {
    false
}

/// Convert a path for safe use with file operations.
///
/// On Windows, this converts the path to extended-length format if it's
/// already long or might become long (e.g., when a temp file name is added).
/// On other platforms, it returns the path unchanged.
///
/// This function is designed to be used proactively - even if a path
/// isn't currently long, it may become long when combined with temp
/// file names or other path components.
#[cfg(windows)]
pub fn safe_path(path: &Path) -> PathBuf {
    // Always use extended-length format on Windows for consistency
    // This avoids issues when parent paths are long and we need to
    // create temp files or subdirectories
    to_extended_length_path(path)
}

/// Convert a path for safe use with file operations.
///
/// On non-Windows platforms, this simply returns a clone of the input path.
#[cfg(not(windows))]
pub fn safe_path(path: &Path) -> PathBuf {
    path.to_path_buf()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(windows)]
    mod windows_tests {
        use super::*;

        #[test]
        fn test_extended_length_absolute_path() {
            let path = Path::new(r"C:\test\path");
            let extended = to_extended_length_path(path);
            assert_eq!(
                extended.to_string_lossy(),
                r"\\?\
C:\test\path"
            );
        }

        #[test]
        fn test_extended_length_already_extended() {
            let path = Path::new(r"\\?\C:\test\path");
            let extended = to_extended_length_path(path);
            assert_eq!(extended.to_string_lossy(), r"\\?\C:\test\path");
        }

        #[test]
        fn test_extended_length_unc_path() {
            let path = Path::new(r"\\server\share\path");
            let extended = to_extended_length_path(path);
            assert_eq!(extended.to_string_lossy(), r"\\?\UNC\server\share\path");
        }

        #[test]
        fn test_is_long_path() {
            let short_path = Path::new(r"C:\short");
            assert!(!is_long_path(short_path));

            // Create a path longer than 200 characters
            let long_name = "a".repeat(200);
            let long_path_str = format!(r"C:\{}", long_name);
            let long_path = Path::new(&long_path_str);
            assert!(is_long_path(long_path));
        }
    }

    #[cfg(not(windows))]
    mod non_windows_tests {
        use super::*;

        #[test]
        fn test_extended_length_returns_same() {
            let path = Path::new("/test/path");
            let extended = to_extended_length_path(path);
            assert_eq!(extended, path);
        }

        #[test]
        fn test_is_long_path_always_false() {
            let long_name = "a".repeat(300);
            let path = Path::new(&long_name);
            assert!(!is_long_path(path));
        }
    }
}
