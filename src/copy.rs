//! Core copy operations

use crate::error::{Error, Result};
use crate::options::{CopyOptions, OnConflict};
use filetime::{set_file_times, FileTime};
use rayon::prelude::*;
use std::collections::HashSet;
use std::fs::{self, File, Metadata};
use std::io;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::time::Instant;

// =============================================================================
// Reflink support detection (Linux-specific)
// =============================================================================

#[cfg(all(feature = "reflink", target_os = "linux"))]
mod reflink_detect {
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
            let cache = REFLINK_CACHE.lock().unwrap();
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
            let mut cache = REFLINK_CACHE.lock().unwrap();
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
mod reflink_detect {
    use std::path::Path;

    /// On macOS, APFS supports reflink. We assume APFS for simplicity.
    /// A more robust check would use statfs and check f_fstypename.
    pub fn supports_reflink(_path: &Path) -> bool {
        // APFS is the default on modern macOS, so we optimistically try reflink
        true
    }
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

/// Result of a single file copy operation (internal use)
#[derive(Debug, Clone, Copy)]
pub(crate) struct FileCopyResult {
    /// Whether the file was actually copied (false = skipped)
    pub copied: bool,
    /// Number of bytes copied (0 if skipped)
    pub bytes: u64,
}

impl FileCopyResult {
    fn copied(bytes: u64) -> Self {
        Self {
            copied: true,
            bytes,
        }
    }

    fn skipped() -> Self {
        Self {
            copied: false,
            bytes: 0,
        }
    }
}

/// Efficiently copy file contents using the best available method.
///
/// On Linux 4.5+, uses `copy_file_range` for zero-copy kernel-to-kernel transfer.
/// Falls back to `std::io::copy` on other platforms or on error.
fn copy_file_contents(src: &File, dst: &File, len: u64) -> io::Result<u64> {
    #[cfg(target_os = "linux")]
    {
        copy_file_range_all(src, dst, len)
    }
    #[cfg(not(target_os = "linux"))]
    {
        let _ = len; // unused on non-Linux
        io::copy(&mut io::BufReader::new(src), &mut &*dst)
    }
}

/// Linux-specific: copy using copy_file_range(2) syscall.
///
/// This is a zero-copy operation - data never enters userspace.
/// Falls back to io::copy if copy_file_range fails (e.g., cross-filesystem).
#[cfg(target_os = "linux")]
fn copy_file_range_all(src: &File, dst: &File, len: u64) -> io::Result<u64> {
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
                return io::copy(&mut io::BufReader::new(src), &mut &*dst);
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

/// Helper to check if path is a symlink without following it
#[inline]
fn is_symlink(path: &Path) -> bool {
    fs::symlink_metadata(path)
        .map(|m| m.file_type().is_symlink())
        .unwrap_or(false)
}

/// Check if a symlink target contains ".." components that could escape upward
///
/// Returns true if any path component is ".."
#[inline]
fn is_escaping_symlink(target: &Path) -> bool {
    use std::path::Component;
    target
        .components()
        .any(|c| matches!(c, Component::ParentDir))
}

#[cfg(unix)]
use std::os::unix::fs::symlink;

#[cfg(not(unix))]
fn symlink(_target: &Path, _link: &Path) -> std::io::Result<()> {
    Err(std::io::Error::new(
        std::io::ErrorKind::Unsupported,
        "Symlinks not supported on this platform",
    ))
}

/// Get a unique key for a directory based on device and inode.
///
/// This is used for cycle detection and is much faster than canonicalize().
/// On Unix, returns (dev, ino). On non-Unix, falls back to a hash of the path.
#[cfg(unix)]
fn get_dir_key(path: &Path) -> io::Result<(u64, u64)> {
    use std::os::unix::fs::MetadataExt;
    let meta = fs::metadata(path)?;
    Ok((meta.dev(), meta.ino()))
}

#[cfg(not(unix))]
fn get_dir_key(path: &Path) -> io::Result<(u64, u64)> {
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
struct DirEntry {
    src: PathBuf,
    dst: PathBuf,
}

/// Check if source is newer than destination based on mtime
#[inline]
fn is_source_newer(src_meta: &Metadata, dst_meta: &Metadata) -> bool {
    // Compare modification times
    match (src_meta.modified(), dst_meta.modified()) {
        (Ok(src_mtime), Ok(dst_mtime)) => src_mtime > dst_mtime,
        // If we can't get mtime, assume source is newer (conservative: do the copy)
        _ => true,
    }
}

/// Preserve file timestamps (mtime and atime)
fn preserve_timestamps(src_meta: &Metadata, dst: &Path) -> io::Result<()> {
    let mtime = FileTime::from_last_modification_time(src_meta);
    let atime = FileTime::from_last_access_time(src_meta);
    set_file_times(dst, atime, mtime)
}

/// Remove an existing file, symlink, or directory at the given path
#[inline]
fn remove_existing(path: &Path, meta: &Metadata) -> io::Result<()> {
    let ft = meta.file_type();
    if ft.is_symlink() || ft.is_file() {
        fs::remove_file(path)
    } else if ft.is_dir() {
        fs::remove_dir_all(path)
    } else {
        Ok(())
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
    if reflink_detect::supports_reflink(dst.parent().unwrap_or(dst))
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
                // Default file mode 0o666 minus typical umask 0o022 = 0o644
                let _ = fs::set_permissions(dst, fs::Permissions::from_mode(0o644));
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
    let temp_file = tempfile::NamedTempFile::new_in(dst_parent).map_err(|e| Error::TempFile {
        path: dst_parent.to_path_buf(),
        source: e,
    })?;

    // Copy file contents using best available method (zero-copy on Linux)
    let bytes_copied = copy_file_contents(&src_file, temp_file.as_file(), file_len)?;

    // Ensure data is on disk before rename
    if options.fsync {
        temp_file.as_file().sync_all()?;
    }

    // Preserve permissions
    if options.preserve_permissions {
        let perms = src_meta.permissions();
        fs::set_permissions(temp_file.path(), perms)?;
    }

    // Atomic rename
    // - Overwrite/UpdateNewer mode: use persist() to overwrite any file created in the TOCTOU window
    // - Skip/Error mode: use persist_noclobber() to detect race conditions
    let persist_result = if options.on_conflict == OnConflict::Overwrite
        || options.on_conflict == OnConflict::UpdateNewer
    {
        temp_file.persist(dst).map_err(|e| Error::Persist {
            path: dst.to_path_buf(),
            source: e.error,
        })
    } else {
        match temp_file.persist_noclobber(dst) {
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
        // Check if directory already exists
        let created = if !dir.dst.exists() {
            fs::create_dir_all(&dir.dst)?;
            true
        } else {
            false
        };

        // Preserve directory permissions from source
        if options.preserve_dir_permissions {
            match fs::metadata(&dir.src) {
                Ok(metadata) => {
                    if let Err(e) = fs::set_permissions(&dir.dst, metadata.permissions()) {
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
            crate::win_attrs::copy_attributes(&dir.src, &dir.dst);
        }

        if created {
            dirs_created += 1;
        }
    }

    let total_files = files.len();
    let total_symlinks = symlinks.len();

    // Atomic counters for parallel statistics collection
    let files_copied = AtomicU64::new(0);
    let files_skipped = AtomicU64::new(0);
    let bytes_copied = AtomicU64::new(0);
    let fail_count = AtomicUsize::new(0);

    // Phase 3: Copy files in parallel with controlled concurrency
    if total_files > 0 {
        let do_copy = |files: &[(PathBuf, PathBuf)]| {
            files.par_iter().for_each(|(src_file, dst_file)| {
                match copy_file_internal(src_file, dst_file, options) {
                    Ok(result) => {
                        if result.copied {
                            files_copied.fetch_add(1, Ordering::Relaxed);
                            bytes_copied.fetch_add(result.bytes, Ordering::Relaxed);
                        } else {
                            files_skipped.fetch_add(1, Ordering::Relaxed);
                        }
                    }
                    Err(e) => {
                        options.warn(&format!("Failed to copy {}: {}", src_file.display(), e));
                        fail_count.fetch_add(1, Ordering::Relaxed);
                    }
                }
            });
        };

        // Use custom thread pool only if parallelism differs from default
        // This avoids unnecessary pool creation overhead
        if options.parallel != rayon::current_num_threads() {
            let custom_pool = rayon::ThreadPoolBuilder::new()
                .num_threads(options.parallel)
                .build();

            match custom_pool {
                Ok(pool) => pool.install(|| do_copy(&files)),
                Err(e) => {
                    options.warn(&format!(
                        "Failed to create thread pool ({e}), using global pool"
                    ));
                    do_copy(&files);
                }
            }
        } else {
            // Use rayon's global pool directly
            do_copy(&files);
        }

        let failed = fail_count.load(Ordering::Relaxed);
        if failed > 0 {
            return Err(Error::PartialCopy {
                failed,
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
                                // Remove existing file/symlink/dir before creating symlink
                                if is_symlink(dst_link) || dst_link.is_file() {
                                    if let Err(e) = fs::remove_file(dst_link) {
                                        options.warn(&format!(
                                            "Failed to remove existing file {}: {}",
                                            dst_link.display(),
                                            e
                                        ));
                                        symlink_failures += 1;
                                        continue;
                                    }
                                } else if dst_link.is_dir() {
                                    if let Err(e) = fs::remove_dir_all(dst_link) {
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

                    // Create symlink
                    if let Err(e) = symlink(&target, dst_link) {
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
        files_copied: files_copied.load(Ordering::Relaxed),
        files_skipped: files_skipped.load(Ordering::Relaxed),
        symlinks_copied,
        symlinks_skipped,
        dirs_created,
        bytes_copied: bytes_copied.load(Ordering::Relaxed),
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

    #[cfg(unix)]
    #[test]
    fn test_copy_dir_with_symlink() {
        use std::os::unix::fs::symlink;

        let src_dir = tempdir().unwrap();
        let dst_dir = tempdir().unwrap();

        // Create source structure with symlink
        fs::write(src_dir.path().join("target.txt"), "target content").unwrap();
        symlink("target.txt", src_dir.path().join("link.txt")).unwrap();

        let options = CopyOptions::default();
        let dst = dst_dir.path().join("copied");
        copy_dir(src_dir.path(), &dst, &options).unwrap();

        assert!(dst.join("target.txt").exists());
        assert!(dst.join("link.txt").is_symlink());
        assert_eq!(
            fs::read_link(dst.join("link.txt"))
                .unwrap()
                .to_string_lossy(),
            "target.txt"
        );
    }

    #[test]
    fn test_copy_dir_empty() {
        let src_dir = tempdir().unwrap();
        let dst_dir = tempdir().unwrap();

        let options = CopyOptions::default();
        let dst = dst_dir.path().join("copied");
        copy_dir(src_dir.path(), &dst, &options).unwrap();

        assert!(dst.exists());
        assert!(dst.is_dir());
    }

    // ==================== Overwrite mode tests ====================

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

        let src_file = src_dir.path().join("test");
        let dst_path = dst_dir.path().join("test");

        fs::write(&src_file, "file content").unwrap();
        fs::create_dir(&dst_path).unwrap();
        fs::write(dst_path.join("inner.txt"), "inner").unwrap();

        let options = CopyOptions::default().with_on_conflict(OnConflict::Overwrite);
        let copied = copy_file(&src_file, &dst_path, &options).unwrap();

        assert!(copied);
        assert!(dst_path.is_file());
        assert_eq!(fs::read_to_string(&dst_path).unwrap(), "file content");
    }

    #[cfg(unix)]
    #[test]
    fn test_copy_file_overwrite_symlink() {
        use std::os::unix::fs::symlink;

        let src_dir = tempdir().unwrap();
        let dst_dir = tempdir().unwrap();

        let src_file = src_dir.path().join("test.txt");
        let dst_file = dst_dir.path().join("test.txt");
        let target_file = dst_dir.path().join("target.txt");

        fs::write(&src_file, "new content").unwrap();
        fs::write(&target_file, "target content").unwrap();
        symlink(&target_file, &dst_file).unwrap();

        let options = CopyOptions::default().with_on_conflict(OnConflict::Overwrite);
        let copied = copy_file(&src_file, &dst_file, &options).unwrap();

        assert!(copied);
        assert!(!dst_file.is_symlink());
        assert_eq!(fs::read_to_string(&dst_file).unwrap(), "new content");
        // Original target should be unchanged
        assert_eq!(fs::read_to_string(&target_file).unwrap(), "target content");
    }

    // ==================== Error mode tests ====================

    #[test]
    fn test_copy_file_error_on_existing() {
        let src_dir = tempdir().unwrap();
        let dst_dir = tempdir().unwrap();

        let src_file = src_dir.path().join("test.txt");
        let dst_file = dst_dir.path().join("test.txt");

        fs::write(&src_file, "new content").unwrap();
        fs::write(&dst_file, "old content").unwrap();

        let options = CopyOptions::default().with_on_conflict(OnConflict::Error);
        let result = copy_file(&src_file, &dst_file, &options);

        assert!(result.is_err());
        match result.unwrap_err() {
            Error::AlreadyExists(path) => assert_eq!(path, dst_file),
            e => panic!("Expected AlreadyExists error, got: {:?}", e),
        }
        // Original content should be unchanged
        assert_eq!(fs::read_to_string(&dst_file).unwrap(), "old content");
    }

    // ==================== IsADirectory error test ====================

    #[test]
    fn test_copy_file_source_is_directory() {
        let src_dir = tempdir().unwrap();
        let dst_dir = tempdir().unwrap();

        let src_path = src_dir.path().join("subdir");
        let dst_file = dst_dir.path().join("test.txt");

        fs::create_dir(&src_path).unwrap();

        let options = CopyOptions::default();
        let result = copy_file(&src_path, &dst_file, &options);

        assert!(result.is_err());
        match result.unwrap_err() {
            Error::IsADirectory(path) => assert_eq!(path, src_path),
            e => panic!("Expected IsADirectory error, got: {:?}", e),
        }
    }

    // ==================== Permission preservation tests ====================

    #[cfg(unix)]
    #[test]
    fn test_copy_file_preserves_permissions() {
        use std::os::unix::fs::PermissionsExt;

        let src_dir = tempdir().unwrap();
        let dst_dir = tempdir().unwrap();

        let src_file = src_dir.path().join("test.sh");
        let dst_file = dst_dir.path().join("test.sh");

        fs::write(&src_file, "#!/bin/bash\necho hello").unwrap();
        fs::set_permissions(&src_file, fs::Permissions::from_mode(0o755)).unwrap();

        let options = CopyOptions::default();
        copy_file(&src_file, &dst_file, &options).unwrap();

        let dst_perms = fs::metadata(&dst_file).unwrap().permissions();
        assert_eq!(dst_perms.mode() & 0o777, 0o755);
    }

    #[cfg(unix)]
    #[test]
    fn test_copy_file_no_preserve_permissions() {
        use std::os::unix::fs::PermissionsExt;

        let src_dir = tempdir().unwrap();
        let dst_dir = tempdir().unwrap();

        let src_file = src_dir.path().join("test.sh");
        let dst_file = dst_dir.path().join("test.sh");

        fs::write(&src_file, "#!/bin/bash\necho hello").unwrap();
        fs::set_permissions(&src_file, fs::Permissions::from_mode(0o755)).unwrap();

        let mut options = CopyOptions::default();
        options.preserve_permissions = false;
        copy_file(&src_file, &dst_file, &options).unwrap();

        // Permissions should be default (umask applied), not 0o755
        let dst_perms = fs::metadata(&dst_file).unwrap().permissions();
        // Just verify it's not executable (exact value depends on umask)
        assert_ne!(dst_perms.mode() & 0o777, 0o755);
    }

    #[cfg(unix)]
    #[test]
    fn test_copy_dir_preserves_directory_permissions() {
        use std::os::unix::fs::PermissionsExt;

        let src_dir = tempdir().unwrap();
        let dst_dir = tempdir().unwrap();

        let sub_dir = src_dir.path().join("subdir");
        fs::create_dir(&sub_dir).unwrap();
        fs::set_permissions(&sub_dir, fs::Permissions::from_mode(0o700)).unwrap();
        fs::write(sub_dir.join("file.txt"), "content").unwrap();

        let options = CopyOptions::default();
        let dst = dst_dir.path().join("copied");
        copy_dir(src_dir.path(), &dst, &options).unwrap();

        let dst_sub_perms = fs::metadata(dst.join("subdir")).unwrap().permissions();
        assert_eq!(dst_sub_perms.mode() & 0o777, 0o700);
    }

    // ==================== Special filename tests ====================

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
        assert_eq!(fs::read_to_string(&dst_file).unwrap(), "content");
    }

    #[test]
    fn test_copy_file_with_unicode() {
        let src_dir = tempdir().unwrap();
        let dst_dir = tempdir().unwrap();

        let src_file = src_dir.path().join("文件名_ファイル_📁.txt");
        let dst_file = dst_dir.path().join("文件名_ファイル_📁.txt");

        fs::write(&src_file, "unicode content 你好").unwrap();

        let options = CopyOptions::default();
        let copied = copy_file(&src_file, &dst_file, &options).unwrap();

        assert!(copied);
        assert_eq!(
            fs::read_to_string(&dst_file).unwrap(),
            "unicode content 你好"
        );
    }

    #[test]
    fn test_copy_dir_with_special_names() {
        let src_dir = tempdir().unwrap();
        let dst_dir = tempdir().unwrap();

        // Create structure with special names
        let special_dir = src_dir.path().join("目录 with spaces");
        fs::create_dir(&special_dir).unwrap();
        fs::write(special_dir.join("файл.txt"), "кириллица").unwrap();
        fs::write(src_dir.path().join("emoji_🎉.txt"), "party").unwrap();

        let options = CopyOptions::default();
        let dst = dst_dir.path().join("copied");
        copy_dir(src_dir.path(), &dst, &options).unwrap();

        assert!(dst.join("目录 with spaces").join("файл.txt").exists());
        assert_eq!(
            fs::read_to_string(dst.join("目录 with spaces").join("файл.txt")).unwrap(),
            "кириллица"
        );
        assert_eq!(
            fs::read_to_string(dst.join("emoji_🎉.txt")).unwrap(),
            "party"
        );
    }

    // ==================== Symlink overwrite tests ====================

    #[cfg(unix)]
    #[test]
    fn test_copy_dir_overwrite_symlinks() {
        use std::os::unix::fs::symlink;

        let src_dir = tempdir().unwrap();
        let dst_dir = tempdir().unwrap();

        // Source: regular file
        fs::write(src_dir.path().join("file.txt"), "source content").unwrap();
        // Source: symlink pointing to "new_target"
        symlink("new_target", src_dir.path().join("link.txt")).unwrap();

        // Destination: existing symlink pointing to something else
        let dst = dst_dir.path().join("copied");
        fs::create_dir(&dst).unwrap();
        fs::write(dst.join("old_target.txt"), "old target").unwrap();
        symlink("old_target.txt", dst.join("link.txt")).unwrap();

        let options = CopyOptions::default().with_on_conflict(OnConflict::Overwrite);
        copy_dir(src_dir.path(), &dst, &options).unwrap();

        // Symlink should now point to "new_target"
        assert!(dst.join("link.txt").is_symlink());
        assert_eq!(
            fs::read_link(dst.join("link.txt"))
                .unwrap()
                .to_string_lossy(),
            "new_target"
        );
    }

    #[cfg(unix)]
    #[test]
    fn test_copy_dir_skip_existing_symlinks() {
        use std::os::unix::fs::symlink;

        let src_dir = tempdir().unwrap();
        let dst_dir = tempdir().unwrap();

        // Source: symlink pointing to "new_target"
        symlink("new_target", src_dir.path().join("link.txt")).unwrap();

        // Destination: existing symlink pointing to something else
        let dst = dst_dir.path().join("copied");
        fs::create_dir(&dst).unwrap();
        symlink("old_target", dst.join("link.txt")).unwrap();

        let options = CopyOptions::default(); // Default is Skip
        copy_dir(src_dir.path(), &dst, &options).unwrap();

        // Symlink should still point to "old_target" (skipped)
        assert_eq!(
            fs::read_link(dst.join("link.txt"))
                .unwrap()
                .to_string_lossy(),
            "old_target"
        );
    }

    #[cfg(unix)]
    #[test]
    fn test_copy_dir_error_on_existing_symlinks() {
        use std::os::unix::fs::symlink;

        let src_dir = tempdir().unwrap();
        let dst_dir = tempdir().unwrap();

        // Source: symlink
        symlink("target", src_dir.path().join("link.txt")).unwrap();

        // Destination: existing symlink
        let dst = dst_dir.path().join("copied");
        fs::create_dir(&dst).unwrap();
        symlink("old_target", dst.join("link.txt")).unwrap();

        let options = CopyOptions::default().with_on_conflict(OnConflict::Error);
        let result = copy_dir(src_dir.path(), &dst, &options);

        assert!(result.is_err());
        match result.unwrap_err() {
            Error::AlreadyExists(path) => assert_eq!(path, dst.join("link.txt")),
            e => panic!("Expected AlreadyExists error, got: {:?}", e),
        }
    }

    // ==================== Source not found tests ====================

    #[test]
    fn test_copy_file_source_not_found() {
        let dst_dir = tempdir().unwrap();

        let src_file = PathBuf::from("/nonexistent/path/file.txt");
        let dst_file = dst_dir.path().join("test.txt");

        let options = CopyOptions::default();
        let result = copy_file(&src_file, &dst_file, &options);

        assert!(result.is_err());
        // Should be an IO error (NotFound)
        match result.unwrap_err() {
            Error::Io(e) => assert_eq!(e.kind(), std::io::ErrorKind::NotFound),
            e => panic!("Expected Io error, got: {:?}", e),
        }
    }

    #[test]
    fn test_copy_dir_source_not_found() {
        let dst_dir = tempdir().unwrap();

        let src_path = PathBuf::from("/nonexistent/path");
        let dst = dst_dir.path().join("copied");

        let options = CopyOptions::default();
        let result = copy_dir(&src_path, &dst, &options);

        assert!(result.is_err());
        match result.unwrap_err() {
            Error::SourceNotFound(path) => assert_eq!(path, src_path),
            e => panic!("Expected SourceNotFound error, got: {:?}", e),
        }
    }

    #[test]
    fn test_copy_dir_source_is_file() {
        let src_dir = tempdir().unwrap();
        let dst_dir = tempdir().unwrap();

        let src_file = src_dir.path().join("file.txt");
        fs::write(&src_file, "content").unwrap();

        let options = CopyOptions::default();
        let result = copy_dir(&src_file, dst_dir.path(), &options);

        assert!(result.is_err());
        match result.unwrap_err() {
            Error::NotADirectory(path) => assert_eq!(path, src_file),
            e => panic!("Expected NotADirectory error, got: {:?}", e),
        }
    }

    // ==================== fsync option tests ====================

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

    // ==================== Symlink loop detection tests ====================

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

    // ==================== Max depth tests ====================

    #[test]
    fn test_copy_dir_max_depth_zero() {
        let src_dir = tempdir().unwrap();
        let dst_dir = tempdir().unwrap();

        // Create nested structure
        let sub_dir = src_dir.path().join("subdir");
        fs::create_dir(&sub_dir).unwrap();
        fs::write(src_dir.path().join("file.txt"), "root").unwrap();
        fs::write(sub_dir.join("nested.txt"), "nested").unwrap();

        // max_depth: 0 means only root directory
        let options = CopyOptions::default().with_max_depth(0);
        let dst = dst_dir.path().join("copied");

        let result = copy_dir(src_dir.path(), &dst, &options);

        // Should fail when trying to recurse into subdir
        assert!(result.is_err());
        match result.unwrap_err() {
            Error::MaxDepthExceeded { max_depth, .. } => assert_eq!(max_depth, 0),
            e => panic!("Expected MaxDepthExceeded error, got: {:?}", e),
        }
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

        assert!(dst.join("a").join("b").join("file.txt").exists());
        assert_eq!(
            fs::read_to_string(dst.join("a").join("b").join("file.txt")).unwrap(),
            "content"
        );
    }

    #[test]
    fn test_copy_dir_no_max_depth() {
        let src_dir = tempdir().unwrap();
        let dst_dir = tempdir().unwrap();

        // Create deeply nested structure
        let mut current = src_dir.path().to_path_buf();
        for i in 0..10 {
            current = current.join(format!("level{}", i));
        }
        fs::create_dir_all(&current).unwrap();
        fs::write(current.join("deep_file.txt"), "very deep").unwrap();

        // No max_depth limit (default)
        let options = CopyOptions::default();
        let dst = dst_dir.path().join("copied");

        copy_dir(src_dir.path(), &dst, &options).unwrap();

        // Build expected path
        let mut expected = dst.clone();
        for i in 0..10 {
            expected = expected.join(format!("level{}", i));
        }
        assert!(expected.join("deep_file.txt").exists());
    }

    // ==================== Broken symlink handling ====================

    #[cfg(unix)]
    #[test]
    fn test_copy_dir_skips_broken_symlink_when_following() {
        use std::os::unix::fs::symlink;
        use std::sync::atomic::{AtomicBool, Ordering};

        let src_dir = tempdir().unwrap();
        let dst_dir = tempdir().unwrap();

        // Create a broken symlink (target doesn't exist)
        fs::write(src_dir.path().join("good.txt"), "content").unwrap();
        symlink("nonexistent_target", src_dir.path().join("broken_link")).unwrap();

        // Track warnings
        static WARNING_ISSUED: AtomicBool = AtomicBool::new(false);
        fn warn_handler(_msg: &str) {
            WARNING_ISSUED.store(true, Ordering::SeqCst);
        }

        WARNING_ISSUED.store(false, Ordering::SeqCst);

        let mut options = CopyOptions::default().with_warn_handler(warn_handler);
        options.preserve_symlinks = false; // Try to follow symlinks

        let dst = dst_dir.path().join("copied");
        copy_dir(src_dir.path(), &dst, &options).unwrap();

        // Good file should be copied
        assert!(dst.join("good.txt").exists());
        // Broken symlink should be skipped with warning
        assert!(!dst.join("broken_link").exists());
        assert!(WARNING_ISSUED.load(Ordering::SeqCst));
    }

    // ==================== Security feature tests ====================

    #[test]
    fn test_is_escaping_symlink() {
        use std::path::Path;

        // Should detect escaping
        assert!(super::is_escaping_symlink(Path::new("..")));
        assert!(super::is_escaping_symlink(Path::new("../foo")));
        assert!(super::is_escaping_symlink(Path::new("foo/../bar")));
        assert!(super::is_escaping_symlink(Path::new("foo/bar/../baz")));
        assert!(super::is_escaping_symlink(Path::new("./../../etc")));

        // Should not detect escaping
        assert!(!super::is_escaping_symlink(Path::new("foo")));
        assert!(!super::is_escaping_symlink(Path::new("foo/bar")));
        assert!(!super::is_escaping_symlink(Path::new("./foo")));
        assert!(!super::is_escaping_symlink(Path::new("foo/./bar")));
    }

    #[cfg(unix)]
    #[test]
    fn test_copy_dir_blocks_escaping_symlinks() {
        use std::os::unix::fs::symlink;
        use std::sync::atomic::{AtomicUsize, Ordering};

        let src_dir = tempdir().unwrap();
        let dst_dir = tempdir().unwrap();

        // Create directory with escaping symlinks
        fs::write(src_dir.path().join("good.txt"), "content").unwrap();
        symlink("../escape", src_dir.path().join("escape_link")).unwrap();
        symlink("foo/../../../bar", src_dir.path().join("nested_escape")).unwrap();
        symlink("safe_target", src_dir.path().join("safe_link")).unwrap();

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

        let dst = dst_dir.path().join("copied");
        copy_dir(src_dir.path(), &dst, &options).unwrap();

        // Regular file should be copied
        assert!(dst.join("good.txt").exists());
        // Safe symlink should be created
        assert!(dst.join("safe_link").is_symlink());
        // Escaping symlinks should be blocked
        assert!(!dst.join("escape_link").exists());
        assert!(!dst.join("nested_escape").exists());
        // Two symlinks should have been blocked
        assert_eq!(BLOCK_COUNT.load(Ordering::SeqCst), 2);
    }

    #[cfg(unix)]
    #[test]
    fn test_copy_dir_warns_but_allows_escaping_symlinks_by_default() {
        use std::os::unix::fs::symlink;
        use std::sync::atomic::{AtomicUsize, Ordering};

        let src_dir = tempdir().unwrap();
        let dst_dir = tempdir().unwrap();

        // Create directory with escaping symlink
        symlink("../escape", src_dir.path().join("escape_link")).unwrap();

        static WARN_COUNT: AtomicUsize = AtomicUsize::new(0);
        fn warn_handler(msg: &str) {
            if msg.contains("escaping upward") {
                WARN_COUNT.fetch_add(1, Ordering::SeqCst);
            }
        }

        WARN_COUNT.store(0, Ordering::SeqCst);

        // Default options: warn but don't block
        let options = CopyOptions::default().with_warn_handler(warn_handler);

        let dst = dst_dir.path().join("copied");
        copy_dir(src_dir.path(), &dst, &options).unwrap();

        // Escaping symlink should still be created (just warned)
        assert!(dst.join("escape_link").is_symlink());
        assert_eq!(
            fs::read_link(dst.join("escape_link"))
                .unwrap()
                .to_string_lossy(),
            "../escape"
        );
        // Warning should have been issued
        assert_eq!(WARN_COUNT.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn test_parallel_validation() {
        // parallel: 0 should be clamped to 1
        let options = CopyOptions::default().with_parallel(0);
        assert_eq!(options.parallel, 1);

        // parallel: 1 should stay 1
        let options = CopyOptions::default().with_parallel(1);
        assert_eq!(options.parallel, 1);

        // parallel: 100 should stay 100
        let options = CopyOptions::default().with_parallel(100);
        assert_eq!(options.parallel, 100);
    }

    // ==================== CopyStats tests ====================

    #[test]
    fn test_copy_dir_returns_stats() {
        let src_dir = tempdir().unwrap();
        let dst_dir = tempdir().unwrap();

        // Create source structure
        let sub_dir = src_dir.path().join("subdir");
        fs::create_dir(&sub_dir).unwrap();
        fs::write(src_dir.path().join("file1.txt"), "content1").unwrap();
        fs::write(sub_dir.join("file2.txt"), "content2").unwrap();

        let options = CopyOptions::default();
        let dst = dst_dir.path().join("copied");
        let stats = copy_dir(src_dir.path(), &dst, &options).unwrap();

        assert_eq!(stats.files_copied, 2);
        assert_eq!(stats.files_skipped, 0);
        assert_eq!(stats.dirs_created, 2); // root + subdir
        assert!(stats.bytes_copied > 0);
        assert!(stats.duration.as_nanos() > 0);
    }

    #[test]
    fn test_copy_dir_stats_with_skip() {
        let src_dir = tempdir().unwrap();
        let dst_dir = tempdir().unwrap();

        // Create source structure
        fs::write(src_dir.path().join("file1.txt"), "content1").unwrap();
        fs::write(src_dir.path().join("file2.txt"), "content2").unwrap();

        let dst = dst_dir.path().join("copied");
        fs::create_dir_all(&dst).unwrap();
        // Pre-create one file
        fs::write(dst.join("file1.txt"), "existing").unwrap();

        let options = CopyOptions::default(); // Default is Skip
        let stats = copy_dir(src_dir.path(), &dst, &options).unwrap();

        assert_eq!(stats.files_copied, 1); // Only file2.txt
        assert_eq!(stats.files_skipped, 1); // file1.txt was skipped
    }

    // ==================== Timestamp preservation tests ====================

    #[cfg(unix)]
    #[test]
    fn test_copy_file_preserves_timestamps() {
        use std::thread;
        use std::time::Duration;

        let src_dir = tempdir().unwrap();
        let dst_dir = tempdir().unwrap();

        let src_file = src_dir.path().join("test.txt");
        let dst_file = dst_dir.path().join("test.txt");

        fs::write(&src_file, "content").unwrap();

        // Get source timestamps
        let src_meta = fs::metadata(&src_file).unwrap();
        let src_mtime = src_meta.modified().unwrap();

        // Wait a bit to ensure any new file would have different timestamp
        thread::sleep(Duration::from_millis(50));

        let options = CopyOptions::default();
        copy_file(&src_file, &dst_file, &options).unwrap();

        let dst_meta = fs::metadata(&dst_file).unwrap();
        let dst_mtime = dst_meta.modified().unwrap();

        // Destination should have same mtime as source (within reasonable tolerance)
        let diff = if dst_mtime > src_mtime {
            dst_mtime.duration_since(src_mtime).unwrap()
        } else {
            src_mtime.duration_since(dst_mtime).unwrap()
        };
        assert!(
            diff.as_millis() < 10,
            "mtime difference too large: {:?}",
            diff
        );
    }

    #[test]
    fn test_copy_file_without_timestamps() {
        use std::thread;
        use std::time::Duration;

        let src_dir = tempdir().unwrap();
        let dst_dir = tempdir().unwrap();

        let src_file = src_dir.path().join("test.txt");
        let dst_file = dst_dir.path().join("test.txt");

        fs::write(&src_file, "content").unwrap();

        // Set source to an old timestamp using filetime
        let old_time = filetime::FileTime::from_unix_time(1000000000, 0); // ~2001
        filetime::set_file_mtime(&src_file, old_time).unwrap();

        // Wait to ensure any new file would have different timestamp
        thread::sleep(Duration::from_millis(10));

        let options = CopyOptions::default().without_timestamps();
        copy_file(&src_file, &dst_file, &options).unwrap();

        let dst_meta = fs::metadata(&dst_file).unwrap();
        let dst_mtime = dst_meta.modified().unwrap();

        // Destination should have a recent timestamp (not 2001)
        let now = std::time::SystemTime::now();
        let diff = now.duration_since(dst_mtime).unwrap();
        assert!(
            diff.as_secs() < 60,
            "timestamp should be recent, not preserved"
        );
    }

    // ==================== UpdateNewer tests ====================

    #[test]
    fn test_copy_file_update_newer_copies_when_newer() {
        use std::thread;
        use std::time::Duration;

        let src_dir = tempdir().unwrap();
        let dst_dir = tempdir().unwrap();

        let src_file = src_dir.path().join("test.txt");
        let dst_file = dst_dir.path().join("test.txt");

        // Create old destination first
        fs::write(&dst_file, "old content").unwrap();
        let old_time = filetime::FileTime::from_unix_time(1000000000, 0);
        filetime::set_file_mtime(&dst_file, old_time).unwrap();

        // Wait and create newer source
        thread::sleep(Duration::from_millis(10));
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

        // Create source with old timestamp
        fs::write(&src_file, "old content").unwrap();
        let old_time = filetime::FileTime::from_unix_time(1000000000, 0);
        filetime::set_file_mtime(&src_file, old_time).unwrap();

        // Create newer destination
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
        assert!(dst_file.exists());
    }

    #[test]
    fn test_copy_dir_update_newer() {
        let src_dir = tempdir().unwrap();
        let dst_dir = tempdir().unwrap();

        // Create source files
        fs::write(src_dir.path().join("new.txt"), "new content").unwrap();
        fs::write(src_dir.path().join("old.txt"), "old content").unwrap();
        let old_time = filetime::FileTime::from_unix_time(1000000000, 0);
        filetime::set_file_mtime(src_dir.path().join("old.txt"), old_time).unwrap();

        // Create destination with one newer file
        let dst = dst_dir.path().join("copied");
        fs::create_dir_all(&dst).unwrap();
        fs::write(dst.join("old.txt"), "newer in dst").unwrap();

        let options = CopyOptions::default().with_on_conflict(OnConflict::UpdateNewer);
        let stats = copy_dir(src_dir.path(), &dst, &options).unwrap();

        // new.txt should be copied (didn't exist)
        // old.txt should be skipped (dst is newer)
        assert_eq!(stats.files_copied, 1);
        assert_eq!(stats.files_skipped, 1);
        assert_eq!(
            fs::read_to_string(dst.join("new.txt")).unwrap(),
            "new content"
        );
        assert_eq!(
            fs::read_to_string(dst.join("old.txt")).unwrap(),
            "newer in dst"
        );
    }

    // ==================== Windows attribute preservation tests ====================

    #[cfg(windows)]
    #[test]
    fn test_copy_file_preserves_hidden_attribute() {
        use crate::win_attrs;

        let src_dir = tempdir().unwrap();
        let dst_dir = tempdir().unwrap();

        let src_file = src_dir.path().join("hidden.txt");
        let dst_file = dst_dir.path().join("hidden.txt");

        fs::write(&src_file, "secret content").unwrap();

        // Set hidden attribute on source
        win_attrs::set_attributes(&src_file, 0x2).unwrap(); // FILE_ATTRIBUTE_HIDDEN

        let options = CopyOptions::default();
        copy_file(&src_file, &dst_file, &options).unwrap();

        // Verify destination is also hidden
        let dst_attrs = win_attrs::get_attributes(&dst_file).unwrap();
        assert_ne!(dst_attrs & 0x2, 0, "Destination file should be hidden");
    }

    #[cfg(windows)]
    #[test]
    fn test_copy_file_preserves_system_attribute() {
        use crate::win_attrs;

        let src_dir = tempdir().unwrap();
        let dst_dir = tempdir().unwrap();

        let src_file = src_dir.path().join("system.txt");
        let dst_file = dst_dir.path().join("system.txt");

        fs::write(&src_file, "system content").unwrap();

        // Set system attribute on source
        win_attrs::set_attributes(&src_file, 0x4).unwrap(); // FILE_ATTRIBUTE_SYSTEM

        let options = CopyOptions::default();
        copy_file(&src_file, &dst_file, &options).unwrap();

        // Verify destination has system attribute
        let dst_attrs = win_attrs::get_attributes(&dst_file).unwrap();
        assert_ne!(
            dst_attrs & 0x4,
            0,
            "Destination file should have system attribute"
        );
    }

    #[cfg(windows)]
    #[test]
    fn test_copy_file_preserves_multiple_attributes() {
        use crate::win_attrs;

        let src_dir = tempdir().unwrap();
        let dst_dir = tempdir().unwrap();

        let src_file = src_dir.path().join("multi.txt");
        let dst_file = dst_dir.path().join("multi.txt");

        fs::write(&src_file, "content").unwrap();

        // Set multiple attributes: hidden + system + archive
        let attrs = 0x2 | 0x4 | 0x20;
        win_attrs::set_attributes(&src_file, attrs).unwrap();

        let options = CopyOptions::default();
        copy_file(&src_file, &dst_file, &options).unwrap();

        // Verify all attributes are preserved
        let dst_attrs = win_attrs::get_attributes(&dst_file).unwrap();
        assert_ne!(dst_attrs & 0x2, 0, "Hidden attribute should be preserved");
        assert_ne!(dst_attrs & 0x4, 0, "System attribute should be preserved");
        assert_ne!(dst_attrs & 0x20, 0, "Archive attribute should be preserved");
    }

    #[cfg(windows)]
    #[test]
    fn test_copy_file_without_windows_attributes() {
        use crate::win_attrs;

        let src_dir = tempdir().unwrap();
        let dst_dir = tempdir().unwrap();

        let src_file = src_dir.path().join("hidden.txt");
        let dst_file = dst_dir.path().join("hidden.txt");

        fs::write(&src_file, "content").unwrap();

        // Set hidden attribute on source
        win_attrs::set_attributes(&src_file, 0x2).unwrap();

        // Disable Windows attribute preservation
        let options = CopyOptions::default().without_windows_attributes();
        copy_file(&src_file, &dst_file, &options).unwrap();

        // Destination should NOT be hidden
        let dst_attrs = win_attrs::get_attributes(&dst_file).unwrap();
        assert_eq!(dst_attrs & 0x2, 0, "Destination file should not be hidden");
    }

    #[cfg(windows)]
    #[test]
    fn test_copy_dir_preserves_hidden_directory() {
        use crate::win_attrs;

        let src_dir = tempdir().unwrap();
        let dst_dir = tempdir().unwrap();

        // Create a hidden subdirectory with a file
        let hidden_subdir = src_dir.path().join("hidden_dir");
        fs::create_dir(&hidden_subdir).unwrap();
        fs::write(hidden_subdir.join("file.txt"), "content").unwrap();

        // Make the subdirectory hidden
        win_attrs::set_attributes(&hidden_subdir, 0x2).unwrap();

        let options = CopyOptions::default();
        let dst = dst_dir.path().join("copied");
        copy_dir(src_dir.path(), &dst, &options).unwrap();

        // Verify the copied subdirectory is also hidden
        let dst_subdir = dst.join("hidden_dir");
        let dst_attrs = win_attrs::get_attributes(&dst_subdir).unwrap();
        assert_ne!(dst_attrs & 0x2, 0, "Destination directory should be hidden");
    }

    #[cfg(windows)]
    #[test]
    fn test_copy_dir_preserves_hidden_files() {
        use crate::win_attrs;

        let src_dir = tempdir().unwrap();
        let dst_dir = tempdir().unwrap();

        // Create a hidden file
        let hidden_file = src_dir.path().join("hidden.txt");
        fs::write(&hidden_file, "hidden content").unwrap();
        win_attrs::set_attributes(&hidden_file, 0x2).unwrap();

        // Create a normal file
        fs::write(src_dir.path().join("normal.txt"), "normal content").unwrap();

        let options = CopyOptions::default();
        let dst = dst_dir.path().join("copied");
        copy_dir(src_dir.path(), &dst, &options).unwrap();

        // Hidden file should be hidden in destination
        let dst_hidden = dst.join("hidden.txt");
        let dst_attrs = win_attrs::get_attributes(&dst_hidden).unwrap();
        assert_ne!(dst_attrs & 0x2, 0, "Hidden file should remain hidden");

        // Normal file should not be hidden
        let dst_normal = dst.join("normal.txt");
        let normal_attrs = win_attrs::get_attributes(&dst_normal).unwrap();
        assert_eq!(normal_attrs & 0x2, 0, "Normal file should not be hidden");
    }
}
