//! Resumable copy integration tests for pcp CLI.
//!
//! These tests verify the "resume from failure" functionality:
//! - When copy fails partway, already-copied files are retained
//! - Re-running the same command skips already-copied files
//! - This enables resumable copy operations

use assert_cmd::cargo::cargo_bin_cmd;
use std::fs;
use tempfile::TempDir;

/// Test that partially copied files are retained and resume works.
///
/// This is the core test for verifying that cleanup logic was removed:
/// 1. Create 10 source files
/// 2. Manually copy first 5 files (simulating partial success)
/// 3. Run full copy command
/// 4. Verify: 5 files skipped (already exist), 5 files newly copied
/// 5. All 10 files should exist in destination
#[test]
fn test_resume_partial_copy() {
    let src = TempDir::new().unwrap();
    let dst = TempDir::new().unwrap();

    // Setup: Create 10 source files
    for i in 0..10 {
        fs::write(
            src.path().join(format!("file{:02}.txt", i)),
            format!("content{}", i),
        )
        .unwrap();
    }

    // Get the source directory name (last component of the path)
    let src_name = src.path().file_name().unwrap().to_str().unwrap();

    // When using -t flag, pcp creates: dst/copied/<src_name>/
    // So we need to simulate partial copy in that location
    let target_dir = dst.path().join("copied").join(src_name);
    fs::create_dir_all(&target_dir).unwrap();

    // Simulate partial copy: manually copy first 5 files
    for i in 0..5 {
        fs::copy(
            src.path().join(format!("file{:02}.txt", i)),
            target_dir.join(format!("file{:02}.txt", i)),
        )
        .unwrap();
    }

    // Verify initial state: 5 files exist
    let initial_count = count_files(&target_dir);
    assert_eq!(initial_count, 5, "Should have 5 files initially");

    // Run full copy command (default is Skip existing files)
    // Use -t to specify target directory explicitly
    let mut cmd = cargo_bin_cmd!("pcp");
    cmd.arg("-r")
        .arg(src.path())
        .arg("-t")
        .arg(dst.path().join("copied"))
        .assert()
        .success();

    // Verify: All 10 files now exist
    let final_count = count_files(&target_dir);
    assert_eq!(final_count, 10, "Should have all 10 files after resume");

    // Verify content of all files
    for i in 0..10 {
        let path = target_dir.join(format!("file{:02}.txt", i));
        assert!(path.exists(), "File {} should exist", i);
        assert_eq!(
            fs::read_to_string(&path).unwrap(),
            format!("content{}", i),
            "File {} content mismatch",
            i
        );
    }
}

/// Test that default behavior skips existing files (enabling resume).
#[test]
fn test_skip_existing_files_by_default() {
    let src = TempDir::new().unwrap();
    let dst = TempDir::new().unwrap();

    // Create source file
    fs::write(src.path().join("test.txt"), "source content").unwrap();

    // Create destination file with different content
    fs::write(dst.path().join("test.txt"), "destination content").unwrap();

    // Run copy with default behavior (Skip)
    let mut cmd = cargo_bin_cmd!("pcp");
    cmd.arg(src.path().join("test.txt"))
        .arg(dst.path().join("test.txt"))
        .assert()
        .success();

    // Verify: destination file was NOT overwritten
    assert_eq!(
        fs::read_to_string(dst.path().join("test.txt")).unwrap(),
        "destination content",
        "File should not be overwritten with default Skip behavior"
    );
}

/// Test that --overwrite flag allows resuming after fixing conflicts.
#[test]
fn test_overwrite_allows_resume() {
    let src = TempDir::new().unwrap();
    let dst = TempDir::new().unwrap();

    // Create source files
    for i in 0..5 {
        fs::write(
            src.path().join(format!("file{}.txt", i)),
            format!("new{}", i),
        )
        .unwrap();
    }

    // Get the source directory name (last component of the path)
    let src_name = src.path().file_name().unwrap().to_str().unwrap();
    let target_dir = dst.path().join(src_name);

    // Create 2 existing destination files with old content
    fs::create_dir_all(&target_dir).unwrap();
    fs::write(target_dir.join("file0.txt"), "old0").unwrap();
    fs::write(target_dir.join("file1.txt"), "old1").unwrap();

    // Run copy with overwrite (use -t to specify target directory)
    let mut cmd = cargo_bin_cmd!("pcp");
    cmd.arg("-r")
        .arg("-c")
        .arg("overwrite")
        .arg(src.path())
        .arg("-t")
        .arg(dst.path())
        .assert()
        .success();

    // Verify: all files exist with new content
    for i in 0..5 {
        let path = target_dir.join(format!("file{}.txt", i));
        assert!(path.exists(), "File {} should exist", i);
        assert_eq!(
            fs::read_to_string(&path).unwrap(),
            format!("new{}", i),
            "File {} should have new content",
            i
        );
    }
}

/// Test that --update-newer only copies newer files (useful for incremental backup).
#[test]
fn test_update_newer_for_resume() {
    let src = TempDir::new().unwrap();
    let dst = TempDir::new().unwrap();

    // Get the source directory name (last component of the path)
    let src_name = src.path().file_name().unwrap().to_str().unwrap();
    let target_dir = dst.path().join(src_name);

    // Create destination files FIRST (older timestamps)
    fs::create_dir_all(&target_dir).unwrap();
    fs::write(target_dir.join("file1.txt"), "old1").unwrap();

    // Wait to ensure different timestamps (file systems have limited timestamp precision)
    // 1 second is usually sufficient for most file systems
    std::thread::sleep(std::time::Duration::from_secs(1));

    // Create source files AFTER (newer timestamps)
    fs::write(src.path().join("file1.txt"), "source1").unwrap();
    fs::write(src.path().join("file2.txt"), "source2").unwrap();

    // Run copy with --update-newer (use -t to specify target directory)
    let mut cmd = cargo_bin_cmd!("pcp");
    cmd.arg("-r")
        .arg("-c")
        .arg("update")
        .arg(src.path())
        .arg("-t")
        .arg(dst.path())
        .assert()
        .success();

    // file1.txt should be updated (source is newer)
    assert_eq!(
        fs::read_to_string(target_dir.join("file1.txt")).unwrap(),
        "source1",
        "file1.txt should be updated"
    );

    // file2.txt should be new
    assert_eq!(
        fs::read_to_string(target_dir.join("file2.txt")).unwrap(),
        "source2",
        "file2.txt should be created"
    );
}

/// Test resuming a directory copy with nested structure.
#[test]
fn test_resume_nested_directory() {
    let src = TempDir::new().unwrap();
    let dst = TempDir::new().unwrap();

    // Create nested source structure
    fs::create_dir_all(src.path().join("a/b/c")).unwrap();
    fs::write(src.path().join("a/file1.txt"), "content1").unwrap();
    fs::write(src.path().join("a/b/file2.txt"), "content2").unwrap();
    fs::write(src.path().join("a/b/c/file3.txt"), "content3").unwrap();

    // Get the source directory name (last component of the path)
    let src_name = src.path().file_name().unwrap().to_str().unwrap();
    let target_dir = dst.path().join("copied").join(src_name);

    // Simulate partial copy: copy only the top-level directory and one file
    fs::create_dir_all(target_dir.join("a/b")).unwrap();
    fs::copy(
        src.path().join("a/file1.txt"),
        target_dir.join("a/file1.txt"),
    )
    .unwrap();

    // Run full copy (use -t to specify target directory)
    let mut cmd = cargo_bin_cmd!("pcp");
    cmd.arg("-r")
        .arg(src.path())
        .arg("-t")
        .arg(dst.path().join("copied"))
        .assert()
        .success();

    // Verify all files exist
    assert!(target_dir.join("a/file1.txt").exists());
    assert!(target_dir.join("a/b/file2.txt").exists());
    assert!(target_dir.join("a/b/c/file3.txt").exists());
}

/// Test that pcp preserves file permissions when copying.
#[cfg(unix)]
#[test]
fn test_copy_preserves_permissions() {
    use std::os::unix::fs::PermissionsExt;

    let src = TempDir::new().unwrap();
    let dst = TempDir::new().unwrap();

    // Create source file with specific permissions
    fs::write(src.path().join("script.sh"), "#!/bin/bash\necho test").unwrap();
    fs::set_permissions(
        src.path().join("script.sh"),
        fs::Permissions::from_mode(0o755),
    )
    .unwrap();

    // Copy using pcp (not fs::copy, to test pcp's behavior)
    let mut cmd = cargo_bin_cmd!("pcp");
    cmd.arg(src.path().join("script.sh"))
        .arg(dst.path().join("script.sh"))
        .assert()
        .success();

    // Verify permissions are preserved by pcp
    let metadata = fs::metadata(dst.path().join("script.sh")).unwrap();
    let permissions = metadata.permissions();
    assert_eq!(
        permissions.mode() & 0o777,
        0o755,
        "pcp should preserve file permissions (expected 0o755)"
    );
}

/// Test that skipping existing files preserves their permissions.
#[cfg(unix)]
#[test]
fn test_skip_existing_preserves_permissions() {
    use std::os::unix::fs::PermissionsExt;

    let src = TempDir::new().unwrap();
    let dst = TempDir::new().unwrap();

    // Create source file with different permissions
    fs::write(src.path().join("script.sh"), "#!/bin/bash\necho test").unwrap();
    fs::set_permissions(
        src.path().join("script.sh"),
        fs::Permissions::from_mode(0o755),
    )
    .unwrap();

    // Create destination file with different content and permissions
    fs::write(dst.path().join("script.sh"), "#!/bin/bash\necho old").unwrap();
    fs::set_permissions(
        dst.path().join("script.sh"),
        fs::Permissions::from_mode(0o644),
    )
    .unwrap();

    // Run copy - should skip existing file (default behavior)
    let mut cmd = cargo_bin_cmd!("pcp");
    cmd.arg(src.path().join("script.sh"))
        .arg(dst.path().join("script.sh"))
        .assert()
        .success();

    // Verify: file should NOT be overwritten, permissions should remain unchanged
    let metadata = fs::metadata(dst.path().join("script.sh")).unwrap();
    let permissions = metadata.permissions();
    assert_eq!(
        permissions.mode() & 0o777,
        0o644,
        "Skipping existing file should preserve its original permissions"
    );
    assert_eq!(
        fs::read_to_string(dst.path().join("script.sh")).unwrap(),
        "#!/bin/bash\necho old",
        "Skipping existing file should preserve its content"
    );
}

/// Test resume with symlinks.
#[cfg(unix)]
#[test]
fn test_resume_with_symlinks() {
    use std::os::unix::fs::symlink;

    let src = TempDir::new().unwrap();
    let dst = TempDir::new().unwrap();

    // Create source files and symlink
    fs::write(src.path().join("target.txt"), "target content").unwrap();
    symlink("target.txt", src.path().join("link")).unwrap();

    // Get the source directory name (last component of the path)
    let src_name = src.path().file_name().unwrap().to_str().unwrap();
    let target_dir = dst.path().join("copied").join(src_name);

    // Simulate partial copy: copy only the target file
    fs::create_dir_all(&target_dir).unwrap();
    fs::copy(src.path().join("target.txt"), target_dir.join("target.txt")).unwrap();

    // Run full copy (use -t to specify target directory)
    let mut cmd = cargo_bin_cmd!("pcp");
    cmd.arg("-r")
        .arg(src.path())
        .arg("-t")
        .arg(dst.path().join("copied"))
        .assert()
        .success();

    // Verify both files exist
    assert!(target_dir.join("target.txt").exists());
    // Note: symlink handling depends on --no-symlinks flag
}

// Helper function to count files in a directory (non-recursive)
fn count_files(dir: &std::path::Path) -> usize {
    fs::read_dir(dir)
        .unwrap()
        .filter_map(|e| e.ok())
        .filter(|e| e.path().is_file())
        .count()
}
