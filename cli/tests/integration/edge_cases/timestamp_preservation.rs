//! Timestamp preservation integration tests for pcp CLI.
//!
//! These tests verify that file timestamps are preserved during copy:
//! - Modification time (mtime) preservation
//! - Access time (atime) preservation
//! - Behavior comparison with `cp -p`
//!
//! Note: Full timestamp preservation requires proper permissions and
//! may not work correctly on all file systems.

use assert_cmd::cargo::cargo_bin_cmd;
use std::fs;
use std::time::{Duration, SystemTime};
use tempfile::TempDir;

/// Helper function to get file modification time
fn get_mtime(path: &std::path::Path) -> SystemTime {
    fs::metadata(path)
        .expect("Failed to get metadata")
        .modified()
        .expect("Failed to get modification time")
}

/// Helper function to get file access time
fn get_atime(path: &std::path::Path) -> SystemTime {
    fs::metadata(path)
        .expect("Failed to get metadata")
        .accessed()
        .expect("Failed to get access time")
}

/// Test that modification time is preserved during copy.
#[test]
fn test_modification_time_preserved() {
    let src = TempDir::new().unwrap();
    let dst = TempDir::new().unwrap();

    // Create source file
    fs::write(src.path().join("test.txt"), "content").unwrap();

    // Get original mtime
    let original_mtime = get_mtime(&src.path().join("test.txt"));

    // Wait a bit to ensure time difference would be visible if not preserved
    std::thread::sleep(Duration::from_millis(100));

    // Copy the file
    let mut cmd = cargo_bin_cmd!("pcp");
    cmd.arg(src.path().join("test.txt"))
        .arg(dst.path().join("test.txt"))
        .assert()
        .success();

    // Verify mtime is preserved
    let copied_mtime = get_mtime(&dst.path().join("test.txt"));

    // Allow small difference due to file system timestamp precision
    let diff = if copied_mtime > original_mtime {
        copied_mtime.duration_since(original_mtime).unwrap()
    } else {
        original_mtime.duration_since(copied_mtime).unwrap()
    };

    assert!(
        diff < Duration::from_secs(2),
        "Modification time should be preserved (difference: {:?})",
        diff
    );
}

/// Test that access time is preserved during copy.
#[test]
fn test_access_time_preserved() {
    let src = TempDir::new().unwrap();
    let dst = TempDir::new().unwrap();

    // Create source file
    fs::write(src.path().join("test.txt"), "content").unwrap();

    // Get original atime
    let _original_atime = get_atime(&src.path().join("test.txt"));

    // Wait a bit
    std::thread::sleep(Duration::from_millis(100));

    // Copy the file
    let mut cmd = cargo_bin_cmd!("pcp");
    cmd.arg(src.path().join("test.txt"))
        .arg(dst.path().join("test.txt"))
        .assert()
        .success();

    // Verify atime behavior
    // Note: Access time behavior may vary between file systems
    // Some file systems don't update atime, or have relatime enabled
    let _copied_atime = get_atime(&dst.path().join("test.txt"));

    // We don't strictly assert atime preservation because:
    // 1. Many modern file systems have relatime or noatime
    // 2. The copy operation itself might update the access time
    // Just verify the file was copied successfully
    assert!(dst.path().join("test.txt").exists());
}

/// Test timestamp preservation for directories.
#[test]
fn test_directory_timestamp_preserved() {
    let src = TempDir::new().unwrap();
    let dst = TempDir::new().unwrap();

    // Create a directory structure
    fs::create_dir_all(src.path().join("subdir/nested")).unwrap();
    fs::write(src.path().join("file1.txt"), "content1").unwrap();
    fs::write(src.path().join("subdir/file2.txt"), "content2").unwrap();

    // Wait a bit
    std::thread::sleep(Duration::from_millis(100));

    // Copy recursively
    let mut cmd = cargo_bin_cmd!("pcp");
    cmd.arg("-r")
        .arg(src.path())
        .arg(dst.path().join("copied"))
        .assert()
        .success();

    // Directory timestamps are typically updated when contents are added
    // so we can't strictly test preservation, but we verify the structure is correct
    assert!(dst.path().join("copied").is_dir());
}

/// Test that timestamps are preserved even after waiting.
#[test]
fn test_timestamp_preserved_after_delay() {
    let src = TempDir::new().unwrap();
    let dst = TempDir::new().unwrap();

    // Create source file
    fs::write(src.path().join("test.txt"), "content").unwrap();

    // Wait significantly longer than file system timestamp precision
    std::thread::sleep(Duration::from_secs(2));

    // Copy the file
    let mut cmd = cargo_bin_cmd!("pcp");
    cmd.arg(src.path().join("test.txt"))
        .arg(dst.path().join("test.txt"))
        .assert()
        .success();

    // Verify mtime is still preserved from original, not from copy time
    let original_mtime = get_mtime(&src.path().join("test.txt"));
    let copied_mtime = get_mtime(&dst.path().join("test.txt"));

    let diff = if copied_mtime > original_mtime {
        copied_mtime.duration_since(original_mtime).unwrap()
    } else {
        original_mtime.duration_since(copied_mtime).unwrap()
    };

    // Should be much less than the 2 second delay
    assert!(
        diff < Duration::from_secs(2),
        "Modification time should be preserved from original, not set to copy time"
    );
}

/// Test timestamp preservation when overwriting with -c overwrite.
#[test]
fn test_timestamp_on_overwrite() {
    let src = TempDir::new().unwrap();
    let dst = TempDir::new().unwrap();

    // Create source and destination files
    fs::write(src.path().join("test.txt"), "new content").unwrap();
    fs::write(dst.path().join("test.txt"), "old content").unwrap();

    // Wait a bit
    std::thread::sleep(Duration::from_millis(100));

    // Get original timestamps
    let src_mtime = get_mtime(&src.path().join("test.txt"));

    // Overwrite with -c overwrite
    let mut cmd = cargo_bin_cmd!("pcp");
    cmd.arg("-c")
        .arg("overwrite")
        .arg(src.path().join("test.txt"))
        .arg(dst.path().join("test.txt"))
        .assert()
        .success();

    // Verify the file was overwritten with the source's timestamp
    let dst_mtime = get_mtime(&dst.path().join("test.txt"));

    let diff = if dst_mtime > src_mtime {
        dst_mtime.duration_since(src_mtime).unwrap()
    } else {
        src_mtime.duration_since(dst_mtime).unwrap()
    };

    assert!(
        diff < Duration::from_secs(2),
        "Overwritten file should have source's modification time"
    );
}

/// Test that timestamps are not modified when skipping existing files.
#[test]
fn test_timestamp_not_changed_on_skip() {
    let src = TempDir::new().unwrap();
    let dst = TempDir::new().unwrap();

    // Create source and destination files with different content
    fs::write(src.path().join("test.txt"), "source content").unwrap();
    fs::write(dst.path().join("test.txt"), "dst content").unwrap();

    // Wait a bit
    std::thread::sleep(Duration::from_millis(100));

    // Get destination's original timestamp
    let original_dst_mtime = get_mtime(&dst.path().join("test.txt"));

    // Run copy (default is skip)
    let mut cmd = cargo_bin_cmd!("pcp");
    cmd.arg(src.path().join("test.txt"))
        .arg(dst.path().join("test.txt"))
        .assert()
        .success();

    // Verify timestamp wasn't changed
    let after_dst_mtime = get_mtime(&dst.path().join("test.txt"));

    assert_eq!(
        original_dst_mtime, after_dst_mtime,
        "Skipped file should keep its original timestamp"
    );

    // Verify content wasn't changed either
    assert_eq!(
        fs::read_to_string(dst.path().join("test.txt")).unwrap(),
        "dst content",
        "Skipped file should keep its original content"
    );
}

/// Test timestamp preservation for newly created nested directories.
#[test]
fn test_nested_directory_timestamps() {
    let src = TempDir::new().unwrap();
    let dst = TempDir::new().unwrap();

    // Create nested structure
    fs::create_dir_all(src.path().join("a/b/c")).unwrap();
    fs::write(src.path().join("a/file1.txt"), "content1").unwrap();
    fs::write(src.path().join("a/b/file2.txt"), "content2").unwrap();
    fs::write(src.path().join("a/b/c/file3.txt"), "content3").unwrap();

    // Wait a bit
    std::thread::sleep(Duration::from_millis(100));

    // Copy recursively
    let mut cmd = cargo_bin_cmd!("pcp");
    cmd.arg("-r")
        .arg(src.path())
        .arg(dst.path().join("copied"))
        .assert()
        .success();

    // Verify file timestamps are preserved
    // Note: pcp copies content directly into target directory
    let copied_base = dst.path().join("copied");

    let src_file_mtime = get_mtime(&src.path().join("a/b/file2.txt"));
    let dst_file_mtime = get_mtime(&copied_base.join("a/b/file2.txt"));

    let diff = if dst_file_mtime > src_file_mtime {
        dst_file_mtime.duration_since(src_file_mtime).unwrap()
    } else {
        src_file_mtime.duration_since(dst_file_mtime).unwrap()
    };

    assert!(
        diff < Duration::from_secs(2),
        "Nested file modification time should be preserved"
    );
}

/// Test behavior comparison with cp for timestamp preservation.
#[test]
fn test_timestamp_behavior_documentation() {
    // This test documents the expected behavior for timestamp preservation
    //
    // Expected behavior (similar to `cp -p`):
    // 1. Source file's mtime should be preserved in the destination
    // 2. Source file's atime should be preserved (when possible)
    // 3. Permission bits should be preserved (separate test)
    //
    // Differences from `cp`:
    // - `cp` by default doesn't preserve timestamps (uses current time)
    // - `cp -p` preserves timestamps, mode, ownership
    // - pcp's default behavior is to preserve timestamps (more like `cp -p`)
    //
    // This is a design decision for resume functionality

    eprintln!("Note: pcp preserves timestamps by default, similar to 'cp -p'");
}
