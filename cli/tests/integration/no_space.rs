//! No-space error integration tests for pcp CLI.
//!
//! These tests simulate "no space left on device" errors.
//!
//! # Running these tests
//!
//! These tests require elevated privileges to mount filesystems:
//!
//! **Option 1: Run with sudo (Linux/macOS)**
//! ```bash
//! sudo cargo test --test no_space
//! ```
//!
//! **Option 2: Use Docker**
//! ```bash
//! docker run --privileged -v $(pwd):/workspace -w /workspace rust:latest \
//!   cargo test --package parcopy-cli --test integration no_space
//! ```
//!
//! **Option 3: GitHub Actions**
//! These tests are automatically run in GitHub Actions with privileged containers.
//!
//! If run without privileges, tests will be gracefully skipped.

use assert_cmd::cargo::cargo_bin_cmd;
use predicates::prelude::*;
use std::fs;
use std::path::Path;
use tempfile::TempDir;

/// Check if we have root/admin privileges.
#[cfg(unix)]
fn has_root_privileges() -> bool {
    unsafe { libc::getuid() == 0 }
}

#[cfg(not(unix))]
fn has_root_privileges() -> bool {
    false
}

/// Check if a command exists on the system.
fn command_exists(cmd: &str) -> bool {
    std::process::Command::new("which")
        .arg(cmd)
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

// =============================================================================
// Linux Tests (using tmpfs)
// =============================================================================

#[cfg(target_os = "linux")]
mod linux_tests {
    use super::*;
    use std::process::Command as StdCommand;

    /// Test that "no space" error retains copied files for resumable copy.
    ///
    /// This is the KEY test for verifying cleanup logic was removed.
    #[test]
    fn test_no_space_retains_files_linux() {
        if !has_root_privileges() {
            eprintln!("SKIP: Test requires root privileges");
            eprintln!("      Run with: sudo cargo test test_no_space_retains_files_linux");
            eprintln!("      Or run in GitHub Actions with privileged container");
            return;
        }

        let mount_point = TempDir::new().expect("Failed to create temp dir");
        let src = TempDir::new().expect("Failed to create temp dir");

        // Create 1MB tmpfs
        let mount_result = StdCommand::new("mount")
            .args(["-t", "tmpfs", "-o", "size=1M", "tmpfs"])
            .arg(mount_point.path())
            .output();

        match mount_result {
            Ok(output) if output.status.success() => {
                eprintln!("Created 1MB tmpfs at {:?}", mount_point.path());
            }
            Ok(output) => {
                eprintln!(
                    "SKIP: Failed to mount tmpfs: {}",
                    String::from_utf8_lossy(&output.stderr)
                );
                return;
            }
            Err(e) => {
                eprintln!("SKIP: Failed to execute mount: {}", e);
                return;
            }
        }

        // Ensure cleanup
        let _guard = scopeguard::guard(mount_point.path(), |path| {
            let _ = StdCommand::new("umount").arg(path).output();
        });

        // Create source files (~1.5MB total, will exceed 1MB limit)
        for i in 0..15 {
            fs::write(
                src.path().join(format!("file{:02}.txt", i)),
                "x".repeat(100_000), // 100KB each
            )
            .expect("Failed to write file");
        }

        // Attempt copy - should fail with "no space"
        let mut cmd = cargo_bin_cmd!("pcp");
        let result = cmd
            .arg("-r")
            .arg(src.path())
            .arg(mount_point.path().join("dst"))
            .assert();

        // Should fail
        result.failure().stderr(
            predicate::str::contains("No space left on device")
                .or(predicate::str::contains("NoSpace"))
                .or(predicate::str::contains("space")),
        );

        // KEY ASSERTION: Some files should be retained (not cleaned up!)
        let copied_count = count_files_recursive(&mount_point.path().join("dst"));
        assert!(
            copied_count > 0,
            "Some files should be retained after no-space error"
        );
        assert!(copied_count < 15, "Not all files should be copied");

        eprintln!("Copied {} files before running out of space", copied_count);

        // Verify files are valid (not corrupted)
        // Note: Files may be copied in any order due to parallel copying,
        // so we iterate over actually existing files instead of assuming sequential order
        let dst_path = mount_point.path().join("dst");
        if dst_path.exists() {
            for entry in fs::read_dir(&dst_path).expect("Failed to read dst directory") {
                let entry = entry.expect("Failed to read entry");
                let path = entry.path();
                if path.is_file() {
                    let content = fs::read_to_string(&path).expect("Failed to read file");
                    assert_eq!(content.len(), 100_000, "File content should be complete");
                }
            }
        }
    }

    /// Test resuming after "no space" error.
    #[test]
    fn test_resume_after_no_space_linux() {
        if !has_root_privileges() {
            eprintln!("SKIP: Test requires root privileges");
            return;
        }

        let mount_point = TempDir::new().expect("Failed to create temp dir");
        let src = TempDir::new().expect("Failed to create temp dir");

        // Create 2MB tmpfs
        let mount_result = StdCommand::new("mount")
            .args(["-t", "tmpfs", "-o", "size=2M", "tmpfs"])
            .arg(mount_point.path())
            .output();

        if !mount_result.map(|o| o.status.success()).unwrap_or(false) {
            eprintln!("SKIP: Failed to mount tmpfs");
            return;
        }

        let _guard = scopeguard::guard(mount_point.path(), |path| {
            let _ = StdCommand::new("umount").arg(path).output();
        });

        // Create source files (will partially fill 2MB)
        for i in 0..10 {
            fs::write(
                src.path().join(format!("file{:02}.txt", i)),
                "x".repeat(150_000), // 150KB each
            )
            .expect("Failed to write file");
        }

        // First copy: should partially succeed
        let mut cmd = cargo_bin_cmd!("pcp");
        let _ = cmd
            .arg("-r")
            .arg(src.path())
            .arg(mount_point.path().join("dst"))
            .assert();

        // Record which files were actually copied (may not be sequential due to parallel copying)
        let dst_path = mount_point.path().join("dst");
        let copied_files: Vec<String> = if dst_path.exists() {
            fs::read_dir(&dst_path)
                .expect("Failed to read dst directory")
                .filter_map(|e| e.ok())
                .filter(|e| e.path().is_file())
                .filter_map(|e| e.file_name().to_str().map(|s| s.to_string()))
                .collect()
        } else {
            Vec::new()
        };
        let first_count = copied_files.len();

        // Increase space (remount with larger size)
        let _ = StdCommand::new("umount").arg(mount_point.path()).output();
        let _ = StdCommand::new("mount")
            .args(["-t", "tmpfs", "-o", "size=5M", "tmpfs"])
            .arg(mount_point.path())
            .output();

        // Re-create the dst directory structure (it was in tmpfs)
        fs::create_dir_all(mount_point.path().join("dst")).ok();

        // Simulate previous partial copy (for resume to work)
        // In real scenario, these files would already exist
        // Note: We recreate the actual files that were copied, not assuming sequential order
        for filename in &copied_files {
            fs::write(
                mount_point.path().join("dst").join(filename),
                "x".repeat(150_000),
            )
            .ok();
        }

        // Second copy: should skip existing and copy remaining
        let mut cmd = cargo_bin_cmd!("pcp");
        cmd.arg("-r")
            .arg(src.path())
            .arg(mount_point.path().join("dst"))
            .assert()
            .success();

        // Now all files should exist
        let final_count = count_files_recursive(&mount_point.path().join("dst"));
        assert_eq!(final_count, 10, "All files should be copied after resume");
    }
}

// =============================================================================
// macOS Tests (using hdiutil)
// =============================================================================

#[cfg(target_os = "macos")]
mod macos_tests {
    use super::*;
    use std::process::Command as StdCommand;

    /// Test "no space" error on macOS using disk image.
    #[test]
    fn test_no_space_retains_files_macos() {
        if !command_exists("hdiutil") {
            eprintln!("SKIP: hdiutil not available");
            return;
        }

        let base_dir = TempDir::new().expect("Failed to create temp dir");
        let src = TempDir::new().expect("Failed to create temp dir");
        let mount_point = base_dir.path().join("mount");

        fs::create_dir_all(&mount_point).expect("Failed to create mount point");

        // Create 1MB disk image
        let dmg_path = base_dir.path().join("small.dmg");

        let create_result = StdCommand::new("hdiutil")
            .args([
                "create",
                "-size",
                "1m",
                "-fs",
                "HFS+",
                "-volname",
                "SmallDisk",
            ])
            .arg(&dmg_path)
            .output();

        match create_result {
            Ok(output) if output.status.success() => {}
            Ok(output) => {
                eprintln!(
                    "SKIP: Failed to create disk image: {}",
                    String::from_utf8_lossy(&output.stderr)
                );
                return;
            }
            Err(e) => {
                eprintln!("SKIP: Failed to execute hdiutil: {}", e);
                return;
            }
        }

        // Mount the disk image
        let mount_result = StdCommand::new("hdiutil")
            .args(["attach", "-mountpoint"])
            .arg(&mount_point)
            .arg(&dmg_path)
            .output();

        if !mount_result.map(|o| o.status.success()).unwrap_or(false) {
            eprintln!("SKIP: Failed to mount disk image");
            return;
        }

        // Ensure cleanup
        let _guard = scopeguard::guard(&mount_point, |path| {
            let _ = StdCommand::new("hdiutil")
                .args(["detach", "-force"])
                .arg(path)
                .output();
        });

        // Create source files (~1.5MB)
        for i in 0..15 {
            fs::write(
                src.path().join(format!("file{:02}.txt", i)),
                "x".repeat(100_000),
            )
            .expect("Failed to write file");
        }

        // Attempt copy
        let mut cmd = cargo_bin_cmd!("pcp");
        let result = cmd
            .arg("-r")
            .arg(src.path())
            .arg(mount_point.join("dst"))
            .assert();

        // Should fail with no space
        result.failure().stderr(predicate::str::contains("space"));

        // Verify partial files retained
        let copied_count = count_files_recursive(&mount_point.join("dst"));
        assert!(copied_count > 0, "Files should be retained");
        assert!(copied_count < 15, "Not all files should be copied");
    }
}

// =============================================================================
// Helper Functions
// =============================================================================

/// Count all files in a directory recursively.
fn count_files_recursive(dir: &Path) -> usize {
    if !dir.exists() {
        return 0;
    }

    let mut count = 0;
    if let Ok(entries) = fs::read_dir(dir) {
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                count += count_files_recursive(&path);
            } else {
                count += 1;
            }
        }
    }
    count
}

// =============================================================================
// Documentation Tests
// =============================================================================

#[test]
fn test_privilege_check() {
    if has_root_privileges() {
        eprintln!("Running with root privileges - all tests will run");
    } else {
        eprintln!("Running without root privileges - some tests will be skipped");
        eprintln!("To run all tests:");
        eprintln!("  sudo cargo test --package parcopy-cli --test integration no_space");
    }
}
