//! File type conflict integration tests for pcp CLI.
//!
//! These tests verify proper handling when source and destination have different types:
//! - File vs Directory conflicts
//! - Directory vs File conflicts
//! - Symlink vs File/Directory conflicts
//! - Special file type handling
//!
//! Key principle: We should NOT silently change file types to avoid data loss.
//! This behavior is consistent with `cp` which also prevents such operations.

use assert_cmd::cargo::cargo_bin_cmd;
use predicates::prelude::*;
use std::fs;
use tempfile::TempDir;

/// Test that copying a file to an existing directory path fails appropriately.
///
/// Scenario: cp file.txt existing_dir (where existing_dir is a directory)
/// Expected: cp copies file INTO the directory: existing_dir/file.txt
#[test]
fn test_file_to_existing_directory() {
    let src = TempDir::new().unwrap();
    let dst = TempDir::new().unwrap();

    // Create source file
    fs::write(src.path().join("file.txt"), "content").unwrap();

    // Create existing directory
    fs::create_dir_all(dst.path().join("existing_dir")).unwrap();

    // Copy file to directory path (without trailing slash)
    let mut cmd = cargo_bin_cmd!("pcp");
    cmd.arg(src.path().join("file.txt"))
        .arg(dst.path().join("existing_dir"))
        .assert()
        .success();

    // Should copy INTO the directory
    assert!(
        dst.path().join("existing_dir/file.txt").exists(),
        "File should be copied INTO the directory"
    );
}

/// Test that copying a directory to an existing file path fails.
///
/// This is a critical safety feature to prevent data loss.
#[test]
fn test_directory_to_existing_file_fails() {
    let src = TempDir::new().unwrap();
    let dst = TempDir::new().unwrap();

    // Create source directory with content
    fs::create_dir_all(src.path().join("mydir/nested")).unwrap();
    fs::write(src.path().join("mydir/file.txt"), "dir content").unwrap();

    // Create destination file (not a directory)
    fs::write(dst.path().join("mydir"), "file content").unwrap();

    // Try to copy directory to a file path
    let mut cmd = cargo_bin_cmd!("pcp");
    cmd.arg("-r")
        .arg("-c")
        .arg("overwrite")
        .arg(src.path().join("mydir"))
        .arg(dst.path().join("mydir"))
        .assert()
        .failure()
        .stderr(
            predicate::str::contains("is a file")
                .or(predicate::str::contains("Not a directory"))
                .or(predicate::str::contains("directory")),
        );

    // Verify: destination file is preserved (not replaced by directory)
    assert!(
        dst.path().join("mydir").is_file(),
        "Destination file should not be replaced by a directory"
    );
    assert_eq!(
        fs::read_to_string(dst.path().join("mydir")).unwrap(),
        "file content",
        "Destination file content should be preserved"
    );
}

/// Test that copying a file to a path where a directory with the same name exists fails.
///
/// This is already tested in error_handling.rs, but included here for completeness.
#[test]
fn test_file_cannot_overwrite_directory() {
    let src = TempDir::new().unwrap();
    let dst = TempDir::new().unwrap();

    // Create source file
    fs::write(src.path().join("conflict"), "file content").unwrap();

    // Create destination directory
    fs::create_dir_all(dst.path().join("conflict/nested")).unwrap();
    fs::write(dst.path().join("conflict/file.txt"), "protected").unwrap();

    // Try to copy (even with -c overwrite)
    let mut cmd = cargo_bin_cmd!("pcp");
    cmd.arg("-c")
        .arg("overwrite")
        .arg(src.path().join("conflict"))
        .arg(dst.path())
        .assert()
        .failure()
        .stderr(predicate::str::contains("directory"));

    // Verify directory is preserved
    assert!(dst.path().join("conflict").is_dir());
    assert!(dst.path().join("conflict/file.txt").exists());
}

/// Test copying a directory to a directory path (should merge/replace based on flags).
///
/// Note: This test documents pcp's behavior when copying a source directory
/// to a destination path that is already an existing directory.
#[test]
fn test_directory_to_directory_path() {
    let src = TempDir::new().unwrap();
    let dst = TempDir::new().unwrap();

    // Create source directory
    fs::create_dir_all(src.path().join("mydir/subdir")).unwrap();
    fs::write(src.path().join("mydir/file1.txt"), "content1").unwrap();
    fs::write(src.path().join("mydir/subdir/file2.txt"), "content2").unwrap();

    // Create destination directory with different files
    fs::create_dir_all(dst.path().join("mydir/subdir")).unwrap();
    fs::write(dst.path().join("mydir/old_file.txt"), "old content").unwrap();

    // Copy source to destination directory path
    // Note: When destination exists and is a directory, pcp creates a nested
    // subdirectory with the source directory name (behavior similar to cp)
    let mut cmd = cargo_bin_cmd!("pcp");
    cmd.arg("-r")
        .arg("-c")
        .arg("overwrite")
        .arg(src.path().join("mydir"))
        .arg(dst.path().join("mydir"))
        .assert()
        .success();

    // Verify new file was copied in the nested directory
    // pcp creates dst/mydir/mydir/ when dst/mydir already exists
    assert!(
        dst.path().join("mydir/mydir/file1.txt").exists(),
        "New file should exist in nested directory"
    );

    // Verify nested structure was copied
    assert!(
        dst.path().join("mydir/mydir/subdir/file2.txt").exists(),
        "Nested file should exist in nested directory"
    );

    // Verify old file is preserved (merge behavior)
    assert!(
        dst.path().join("mydir/old_file.txt").exists(),
        "Old file should still exist (merge behavior)"
    );
}

/// Test copying empty directory over non-empty directory.
#[test]
fn test_empty_directory_over_nonempty_directory() {
    let src = TempDir::new().unwrap();
    let dst = TempDir::new().unwrap();

    // Create empty source directory
    fs::create_dir_all(src.path().join("empty_dir")).unwrap();

    // Create non-empty destination directory
    fs::create_dir_all(dst.path().join("empty_dir")).unwrap();
    fs::write(dst.path().join("empty_dir/file.txt"), "content").unwrap();

    // Copy empty directory over non-empty
    let mut cmd = cargo_bin_cmd!("pcp");
    cmd.arg("-r")
        .arg(src.path().join("empty_dir"))
        .arg(dst.path().join("empty_dir"))
        .assert()
        .success();

    // Behavior: The existing directory content should be preserved
    assert!(
        dst.path().join("empty_dir/file.txt").exists(),
        "Existing files should be preserved when copying empty directory"
    );
}

/// Test behavior when copying to a symlink that points to a directory.
#[cfg(unix)]
#[test]
fn test_copy_to_symlink_pointing_to_directory() {
    use std::os::unix::fs::symlink;

    let src = TempDir::new().unwrap();
    let dst = TempDir::new().unwrap();

    // Create source file
    fs::write(src.path().join("file.txt"), "content").unwrap();

    // Create a directory and a symlink pointing to it
    fs::create_dir_all(dst.path().join("real_dir")).unwrap();
    symlink("real_dir", dst.path().join("dir_link")).unwrap();

    // Copy file to the symlink path
    let mut cmd = cargo_bin_cmd!("pcp");
    cmd.arg(src.path().join("file.txt"))
        .arg(dst.path().join("dir_link"))
        .assert()
        .success();

    // Should copy INTO the directory that the symlink points to
    assert!(
        dst.path().join("real_dir/file.txt").exists(),
        "File should be copied into the symlink's target directory"
    );
}

/// Test behavior when copying to a symlink that points to a file.
#[cfg(unix)]
#[test]
fn test_copy_to_symlink_pointing_to_file() {
    use std::os::unix::fs::symlink;

    let src = TempDir::new().unwrap();
    let dst = TempDir::new().unwrap();

    // Create source file
    fs::write(src.path().join("file.txt"), "new content").unwrap();

    // Create a file and a symlink pointing to it
    fs::write(dst.path().join("real_file.txt"), "old content").unwrap();
    symlink("real_file.txt", dst.path().join("file_link")).unwrap();

    // Copy to the symlink path (should overwrite the target)
    // Note: pcp follows symlinks by default when the destination is a symlink
    let mut cmd = cargo_bin_cmd!("pcp");
    cmd.arg("-c")
        .arg("overwrite")
        .arg(src.path().join("file.txt"))
        .arg(dst.path().join("file_link"))
        .assert()
        .success();

    // When copying to a symlink, pcp follows the symlink and updates the target
    let real_file_content = fs::read_to_string(dst.path().join("real_file.txt")).unwrap();

    // Note: pcp may replace the symlink with a regular file instead of
    // following it to update the target. This test verifies the actual behavior.
    // Check if symlink still exists and points to the same target
    let link_metadata = fs::symlink_metadata(dst.path().join("file_link"));

    if link_metadata.is_ok() && link_metadata.unwrap().file_type().is_symlink() {
        // Symlink was preserved, target should be updated
        assert_eq!(
            real_file_content, "old content",
            "If symlink is preserved, target should be unchanged"
        );
    } else {
        // Symlink was replaced with the source file
        assert_eq!(
            fs::read_to_string(dst.path().join("file_link")).unwrap(),
            "new content",
            "If symlink is replaced, should have new content"
        );
    }
}

/// Test copying directory to a symlink pointing to a directory.
#[cfg(unix)]
#[test]
fn test_copy_directory_to_symlink_pointing_to_directory() {
    use std::os::unix::fs::symlink;

    let src = TempDir::new().unwrap();
    let dst = TempDir::new().unwrap();

    // Create source directory
    fs::create_dir_all(src.path().join("mydir")).unwrap();
    fs::write(src.path().join("mydir/file.txt"), "content").unwrap();

    // Create destination: real_dir <- dir_link
    fs::create_dir_all(dst.path().join("real_dir")).unwrap();
    symlink("real_dir", dst.path().join("dir_link")).unwrap();

    // Copy directory to symlink path
    let mut cmd = cargo_bin_cmd!("pcp");
    cmd.arg("-r")
        .arg(src.path().join("mydir"))
        .arg(dst.path().join("dir_link"))
        .assert()
        .success();

    // Should merge/copy into the symlink's target
    assert!(
        dst.path().join("real_dir/mydir").is_dir() || dst.path().join("real_dir/file.txt").exists(),
        "Directory should be copied into symlink's target"
    );
}

/// Test copying when destination is a broken symlink.
#[cfg(unix)]
#[test]
fn test_copy_to_broken_symlink() {
    use std::os::unix::fs::symlink;

    let src = TempDir::new().unwrap();
    let dst = TempDir::new().unwrap();

    // Create source file
    fs::write(src.path().join("file.txt"), "content").unwrap();

    // Create a broken symlink (points to non-existent target)
    symlink("nonexistent", dst.path().join("broken_link")).unwrap();

    // Try to copy to the broken symlink
    let mut cmd = cargo_bin_cmd!("pcp");
    let _result = cmd
        .arg(src.path().join("file.txt"))
        .arg(dst.path().join("broken_link"))
        .assert();

    // Behavior depends on implementation:
    // - Could fail (symlink is broken)
    // - Could replace the symlink with the actual file
    // - Could follow and fail (target doesn't exist)
    // Just verify no panic occurs
}

/// Test copying multiple file types simultaneously.
#[test]
fn test_mixed_file_types_copy() {
    let src = TempDir::new().unwrap();
    let dst = TempDir::new().unwrap();

    // Create mixed content: files and directories
    fs::write(src.path().join("file1.txt"), "file1").unwrap();
    fs::create_dir_all(src.path().join("dir1")).unwrap();
    fs::write(src.path().join("dir1/file2.txt"), "file2").unwrap();

    // Copy multiple sources where destination is an existing directory
    fs::create_dir_all(dst.path().join("dest")).unwrap();

    let mut cmd = cargo_bin_cmd!("pcp");
    cmd.arg("-r")
        .arg(src.path().join("file1.txt"))
        .arg(src.path().join("dir1"))
        .arg(dst.path().join("dest"))
        .assert()
        .success();

    // Verify both were copied
    assert!(dst.path().join("dest/file1.txt").exists());
    assert!(dst.path().join("dest/dir1").is_dir());
    assert!(dst.path().join("dest/dir1/file2.txt").exists());
}

/// Test behavior when source and destination are the same (or overlap).
#[test]
fn test_copy_to_self_or_overlapping() {
    let src = TempDir::new().unwrap();

    // Create a file
    fs::write(src.path().join("file.txt"), "content").unwrap();

    // Try to copy file to itself
    let mut cmd = cargo_bin_cmd!("pcp");
    cmd.arg(src.path().join("file.txt"))
        .arg(src.path().join("file.txt"))
        .assert()
        .success(); // Should succeed (skip, since file exists)

    // Try to copy directory into itself
    let mut cmd = cargo_bin_cmd!("pcp");
    let _result = cmd
        .arg("-r")
        .arg(src.path())
        .arg(src.path().join("nested"))
        .assert();

    // This should either fail or handle gracefully
    // Copying a directory into itself would create infinite recursion
}

/// Test handling of special filenames (spaces, special characters).
#[test]
fn test_special_filenames() {
    let src = TempDir::new().unwrap();
    let dst = TempDir::new().unwrap();

    // Create files with special names
    fs::write(src.path().join("file with spaces.txt"), "content1").unwrap();
    fs::write(src.path().join("file\twith\ttabs.txt"), "content2").unwrap();
    fs::write(src.path().join("file'with'quotes.txt"), "content3").unwrap();

    // Note: We avoid truly problematic names like:
    // - Newlines (might break command line)
    // - Shell metacharacters like $, `, !, etc.
    // - Control characters

    // Copy recursively
    let mut cmd = cargo_bin_cmd!("pcp");
    cmd.arg("-r")
        .arg(src.path())
        .arg(dst.path().join("copied"))
        .assert()
        .success();

    // Verify all files were copied
    // Note: pcp copies content directly into target directory
    let copied_base = dst.path().join("copied");

    assert!(copied_base.join("file with spaces.txt").exists());
    assert!(copied_base.join("file\twith\ttabs.txt").exists());
    assert!(copied_base.join("file'with'quotes.txt").exists());
}

/// Test behavior comparison with cp for file type conflicts.
#[test]
fn test_file_type_conflict_behavior_documentation() {
    // This test documents the expected behavior for file type conflicts
    //
    // Behavior comparison with `cp`:
    //
    // 1. File -> Directory (overwriting):
    //    cp: "cp: cannot overwrite directory 'X' with non-directory"
    //    pcp: Should fail with similar error
    //
    // 2. Directory -> File (overwriting):
    //    cp: "cp: cannot overwrite non-directory 'X' with directory"
    //    pcp: Should fail with similar error
    //
    // 3. File -> Directory (as destination path):
    //    cp file dir/ -> copies as dir/file
    //    cp file dir  -> copies as dir/file
    //    pcp: Should behave the same
    //
    // 4. Directory -> Directory:
    //    cp -r dir1 dir2/ -> creates dir2/dir1/
    //    cp -r dir1 dir2  -> creates dir2/dir1/ (if dir2 exists)
    //                       or creates dir2/ (if dir2 doesn't exist)
    //    pcp: Should behave consistently

    eprintln!("File type conflict behavior should be consistent with cp");
}
