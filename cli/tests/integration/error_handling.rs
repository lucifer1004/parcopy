//! Error handling integration tests for pcp CLI.
//!
//! These tests verify proper error handling behaviors:
//! - Files cannot overwrite directories (prevents data loss)
//! - Error-on-conflict mode works correctly
//! - Source validation
//! - Permission errors

use assert_cmd::cargo::cargo_bin_cmd;
use predicates::prelude::*;
use std::fs;
use tempfile::TempDir;

/// Test that overwriting a directory with a file fails.
///
/// This is a critical safety feature: we should NOT delete a directory
/// and replace it with a file, as that would cause data loss.
#[test]
fn test_overwrite_directory_with_file_fails() {
    let src = TempDir::new().unwrap();
    let dst = TempDir::new().unwrap();

    // Create a source file with name matching an existing directory in destination
    fs::write(src.path().join("testdir"), "file content").unwrap();

    // Create a destination directory with the same name
    fs::create_dir(dst.path().join("testdir")).unwrap();
    fs::write(dst.path().join("testdir/inside.txt"), "inside content").unwrap();

    // Try to copy file to parent directory where a directory with same name exists
    // Using -t flag to specify parent directory
    let mut cmd = cargo_bin_cmd!("pcp");
    cmd.arg("-c")
        .arg("overwrite")
        .arg(src.path().join("testdir"))
        .arg("-t")
        .arg(dst.path())
        .assert()
        .failure()
        .stderr(predicate::str::contains("is a directory"));

    // Verify: directory still exists and content is preserved
    assert!(
        dst.path().join("testdir").is_dir(),
        "Directory should still exist"
    );
    assert!(
        dst.path().join("testdir/inside.txt").exists(),
        "Directory content should be preserved"
    );
    assert_eq!(
        fs::read_to_string(dst.path().join("testdir/inside.txt")).unwrap(),
        "inside content",
        "Directory content should not be modified"
    );
}

/// Test that overwriting a non-empty directory with a file also fails.
#[test]
fn test_overwrite_nonempty_directory_with_file_fails() {
    let src = TempDir::new().unwrap();
    let dst = TempDir::new().unwrap();

    // Create a source file with name matching an existing directory
    fs::write(src.path().join("datadir"), "data").unwrap();

    // Create a destination directory with multiple files
    fs::create_dir(dst.path().join("datadir")).unwrap();
    fs::write(dst.path().join("datadir/file1.txt"), "content1").unwrap();
    fs::write(dst.path().join("datadir/file2.txt"), "content2").unwrap();
    fs::create_dir(dst.path().join("datadir/subdir")).unwrap();
    fs::write(dst.path().join("datadir/subdir/file3.txt"), "content3").unwrap();

    // Try to copy file to parent directory where a directory with same name exists
    let mut cmd = cargo_bin_cmd!("pcp");
    cmd.arg("-c")
        .arg("overwrite")
        .arg(src.path().join("datadir"))
        .arg("-t")
        .arg(dst.path())
        .assert()
        .failure()
        .stderr(predicate::str::contains("is a directory"));

    // Verify: entire directory tree is preserved
    assert!(dst.path().join("datadir").is_dir());
    assert!(dst.path().join("datadir/file1.txt").exists());
    assert!(dst.path().join("datadir/file2.txt").exists());
    assert!(dst.path().join("datadir/subdir/file3.txt").exists());
}

/// Test error-on-conflict flag.
#[test]
fn test_error_on_conflict_mode() {
    let src = TempDir::new().unwrap();
    let dst = TempDir::new().unwrap();

    // Create source and destination files
    fs::write(src.path().join("test.txt"), "new content").unwrap();
    fs::write(dst.path().join("test.txt"), "old content").unwrap();

    // Try to copy with error-on-conflict
    let mut cmd = cargo_bin_cmd!("pcp");
    cmd.arg("-c")
        .arg("error")
        .arg(src.path().join("test.txt"))
        .arg(dst.path().join("test.txt"))
        .assert()
        .failure()
        .stderr(
            predicate::str::contains("already exists")
                .or(predicate::str::contains("AlreadyExists")),
        );

    // Verify: original file is not modified
    assert_eq!(
        fs::read_to_string(dst.path().join("test.txt")).unwrap(),
        "old content",
        "File should not be modified"
    );
}

/// Test error-on-conflict with directory copy.
#[test]
fn test_error_on_conflict_directory() {
    let src = TempDir::new().unwrap();
    let dst = TempDir::new().unwrap();

    // Create source files
    fs::write(src.path().join("file1.txt"), "content1").unwrap();
    fs::write(src.path().join("file2.txt"), "content2").unwrap();

    // Create destination with one existing file
    // Note: when using -t flag, pcp creates dst/src_name/
    let src_name = src.path().file_name().unwrap().to_str().unwrap();
    let target_dir = dst.path().join(src_name);
    fs::create_dir_all(&target_dir).unwrap();
    fs::write(target_dir.join("file1.txt"), "old content").unwrap();

    // Try to copy with error-on-conflict
    let mut cmd = cargo_bin_cmd!("pcp");
    cmd.arg("-r")
        .arg("-c")
        .arg("error")
        .arg(src.path())
        .arg("-t")
        .arg(dst.path())
        .assert()
        .failure()
        .stderr(predicate::str::contains("Failed to copy"));

    // Verify: only file1.txt existed, so the copy should fail
    // file1.txt should have old content
    assert_eq!(
        fs::read_to_string(target_dir.join("file1.txt")).unwrap(),
        "old content"
    );
}

/// Test source not found error.
#[test]
fn test_source_not_found() {
    let dst = TempDir::new().unwrap();

    let mut cmd = cargo_bin_cmd!("pcp");
    cmd.arg("/nonexistent/path/file.txt")
        .arg(dst.path().join("file.txt"))
        .assert()
        .failure()
        .stderr(predicate::str::contains("Source does not exist"));
}

/// Test source directory not found.
#[test]
fn test_source_directory_not_found() {
    let dst = TempDir::new().unwrap();

    let mut cmd = cargo_bin_cmd!("pcp");
    cmd.arg("-r")
        .arg("/nonexistent/directory")
        .arg(dst.path().join("dest"))
        .assert()
        .failure()
        .stderr(predicate::str::contains("Source does not exist"));
}

/// Test that copying a file to a directory without trailing slash works.
#[test]
fn test_copy_file_to_directory_path() {
    let src = TempDir::new().unwrap();
    let dst = TempDir::new().unwrap();

    fs::write(src.path().join("file.txt"), "content").unwrap();
    fs::create_dir_all(dst.path().join("existing_dir")).unwrap();

    // When destination is an existing directory, file should be copied inside
    let mut cmd = cargo_bin_cmd!("pcp");
    cmd.arg(src.path().join("file.txt"))
        .arg(dst.path().join("existing_dir"))
        .assert()
        .success();

    assert!(dst.path().join("existing_dir/file.txt").exists());
}

/// Test copying multiple sources with one non-existent.
#[test]
fn test_copy_multiple_with_missing_source() {
    let src1 = TempDir::new().unwrap();
    let dst = TempDir::new().unwrap();

    fs::write(src1.path().join("file1.txt"), "content1").unwrap();
    fs::create_dir_all(dst.path().join("dest")).unwrap();

    let mut cmd = cargo_bin_cmd!("pcp");
    cmd.arg(src1.path().join("file1.txt"))
        .arg("/nonexistent/file2.txt")
        .arg(dst.path().join("dest"))
        .assert()
        .failure()
        .stderr(
            predicate::str::contains("does not exist")
                .or(predicate::str::contains("not found"))
                .or(predicate::str::contains("No such file")),
        );

    // Behavior note: Unlike `cp` which checks all sources before copying,
    // pcp might copy some files before encountering the missing source.
    // We verify that at least the error is reported correctly.
    // The existence of file1.txt in dest depends on implementation:
    // - If pcp validates all sources first (like cp), no files should be copied
    // - If pcp copies in parallel, file1.txt might be copied before error
}

/// Test copying directory without -r flag fails.
#[test]
fn test_copy_directory_without_recursive_flag() {
    let src = TempDir::new().unwrap();
    let dst = TempDir::new().unwrap();

    // Create a source directory with files
    fs::create_dir_all(src.path().join("subdir")).unwrap();
    fs::write(src.path().join("file.txt"), "content").unwrap();

    // Try to copy directory without -r flag
    let mut cmd = cargo_bin_cmd!("pcp");
    cmd.arg(src.path())
        .arg(dst.path().join("dest"))
        .assert()
        .failure()
        .stderr(
            predicate::str::contains("directory")
                .or(predicate::str::contains("-r"))
                .or(predicate::str::contains("recursive")),
        );
}

/// Test that we cannot copy to a read-only destination.
#[cfg(unix)]
#[test]
fn test_copy_to_readonly_directory() {
    use std::os::unix::fs::PermissionsExt;

    let src = TempDir::new().unwrap();
    let dst = TempDir::new().unwrap();

    // Create source file
    fs::write(src.path().join("file.txt"), "content").unwrap();

    // Make destination directory read-only
    fs::set_permissions(dst.path(), fs::Permissions::from_mode(0o555)).unwrap();

    // Try to copy
    let mut cmd = cargo_bin_cmd!("pcp");
    let result = cmd
        .arg(src.path().join("file.txt"))
        .arg(dst.path().join("file.txt"))
        .assert();

    // Should fail with permission error
    result.failure().stderr(
        predicate::str::contains("Permission denied")
            .or(predicate::str::contains("permission"))
            .or(predicate::str::contains("denied")),
    );

    // Cleanup: restore permissions so TempDir can be deleted
    fs::set_permissions(dst.path(), fs::Permissions::from_mode(0o755)).ok();
}

/// Test copying to a file whose parent directory doesn't exist.
#[test]
fn test_copy_to_nonexistent_parent_directory() {
    let src = TempDir::new().unwrap();

    fs::write(src.path().join("file.txt"), "content").unwrap();

    // Try to copy to a path where parent directory doesn't exist
    let mut cmd = cargo_bin_cmd!("pcp");
    cmd.arg(src.path().join("file.txt"))
        .arg("/nonexistent/path/file.txt")
        .assert()
        .failure()
        .stderr(
            predicate::str::contains("No such file or directory")
                .or(predicate::str::contains("does not exist"))
                .or(predicate::str::contains("not found")),
        );

    // Behavior: consistent with `cp` - should fail without creating parent directories
    // `cp file.txt /nonexistent/path/file.txt` fails with:
    // "cp: cannot create regular file '/nonexistent/path/file.txt': No such file or directory"
}

/// Test that copying an empty directory works.
#[test]
fn test_copy_empty_directory() {
    let src = TempDir::new().unwrap();
    let dst = TempDir::new().unwrap();

    // Create empty directory
    fs::create_dir_all(src.path().join("empty_dir")).unwrap();

    let mut cmd = cargo_bin_cmd!("pcp");
    cmd.arg("-r")
        .arg(src.path().join("empty_dir"))
        .arg(dst.path().join("empty_dir"))
        .assert()
        .success();

    assert!(dst.path().join("empty_dir").is_dir());
}

/// Test error message quality for common mistakes.
#[test]
fn test_error_message_quality() {
    let src = TempDir::new().unwrap();
    let dst = TempDir::new().unwrap();

    // Create a source directory
    fs::create_dir_all(src.path().join("mydir")).unwrap();

    // Try to copy directory without -r
    let mut cmd = cargo_bin_cmd!("pcp");
    cmd.arg(src.path().join("mydir"))
        .arg(dst.path().join("mydir"))
        .assert()
        .failure()
        .stderr(
            predicate::str::contains("-r")
                .or(predicate::str::contains("recursive"))
                .or(predicate::str::contains("directory")),
        );
}
