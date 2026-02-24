//! Basic functionality integration tests for pcp CLI.

use assert_cmd::cargo::cargo_bin_cmd;
use predicates::prelude::*;
use std::fs;
use tempfile::TempDir;

#[test]
fn test_basic_file_copy() {
    let src = TempDir::new().unwrap();
    let dst = TempDir::new().unwrap();

    fs::write(src.path().join("test.txt"), "hello world").unwrap();

    let mut cmd = cargo_bin_cmd!("pcp");
    cmd.arg(src.path().join("test.txt"))
        .arg(dst.path().join("test.txt"))
        .assert()
        .success();

    assert!(dst.path().join("test.txt").exists());
    assert_eq!(
        fs::read_to_string(dst.path().join("test.txt")).unwrap(),
        "hello world"
    );
}

#[test]
fn test_recursive_directory_copy() {
    let src = TempDir::new().unwrap();
    let dst = TempDir::new().unwrap();

    // Create directory structure
    fs::create_dir_all(src.path().join("subdir/nested")).unwrap();
    fs::write(src.path().join("file1.txt"), "content1").unwrap();
    fs::write(src.path().join("subdir/file2.txt"), "content2").unwrap();
    fs::write(src.path().join("subdir/nested/file3.txt"), "content3").unwrap();

    let mut cmd = cargo_bin_cmd!("pcp");
    cmd.arg("-r")
        .arg(src.path())
        .arg(dst.path().join("copied"))
        .assert()
        .success();

    // Verify all files exist
    assert!(dst.path().join("copied/file1.txt").exists());
    assert!(dst.path().join("copied/subdir/file2.txt").exists());
    assert!(dst.path().join("copied/subdir/nested/file3.txt").exists());

    // Verify content
    assert_eq!(
        fs::read_to_string(dst.path().join("copied/file1.txt")).unwrap(),
        "content1"
    );
}

#[test]
fn test_copy_multiple_sources() {
    let src1 = TempDir::new().unwrap();
    let src2 = TempDir::new().unwrap();
    let dst = TempDir::new().unwrap();

    fs::write(src1.path().join("file1.txt"), "content1").unwrap();
    fs::write(src2.path().join("file2.txt"), "content2").unwrap();

    // Create destination directory
    fs::create_dir_all(dst.path().join("dest")).unwrap();

    let mut cmd = cargo_bin_cmd!("pcp");
    cmd.arg(src1.path().join("file1.txt"))
        .arg(src2.path().join("file2.txt"))
        .arg(dst.path().join("dest"))
        .assert()
        .success();

    assert!(dst.path().join("dest/file1.txt").exists());
    assert!(dst.path().join("dest/file2.txt").exists());
}

#[test]
fn test_overwrite_existing_file() {
    let src = TempDir::new().unwrap();
    let dst = TempDir::new().unwrap();

    fs::write(src.path().join("test.txt"), "new content").unwrap();
    fs::write(dst.path().join("test.txt"), "old content").unwrap();

    let mut cmd = cargo_bin_cmd!("pcp");
    cmd.arg("-c")
        .arg("overwrite")
        .arg(src.path().join("test.txt"))
        .arg(dst.path().join("test.txt"))
        .assert()
        .success();

    // Verify file was overwritten
    assert_eq!(
        fs::read_to_string(dst.path().join("test.txt")).unwrap(),
        "new content"
    );
}

#[test]
fn test_skip_existing_file() {
    let src = TempDir::new().unwrap();
    let dst = TempDir::new().unwrap();

    fs::write(src.path().join("test.txt"), "new content").unwrap();
    fs::write(dst.path().join("test.txt"), "old content").unwrap();

    // Default behavior is Skip
    let mut cmd = cargo_bin_cmd!("pcp");
    cmd.arg(src.path().join("test.txt"))
        .arg(dst.path().join("test.txt"))
        .assert()
        .success();

    // Verify file was NOT overwritten
    assert_eq!(
        fs::read_to_string(dst.path().join("test.txt")).unwrap(),
        "old content"
    );
}

#[test]
fn test_quiet_mode() {
    let src = TempDir::new().unwrap();
    let dst = TempDir::new().unwrap();

    fs::write(src.path().join("test.txt"), "content").unwrap();

    let mut cmd = cargo_bin_cmd!("pcp");
    cmd.arg("--quiet")
        .arg(src.path().join("test.txt"))
        .arg(dst.path().join("test.txt"))
        .assert()
        .success();
    // Note: --quiet only disables progress bar, not summary output

    assert!(dst.path().join("test.txt").exists());
}

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

#[test]
fn test_help_flag() {
    let mut cmd = cargo_bin_cmd!("pcp");
    cmd.arg("--help")
        .assert()
        .success()
        .stdout(predicate::str::contains("pcp"))
        .stdout(predicate::str::contains("parallel"));
}

#[test]
fn test_version_flag() {
    let mut cmd = cargo_bin_cmd!("pcp");
    cmd.arg("--version")
        .assert()
        .success()
        .stdout(predicate::str::contains("pcp"));
}
