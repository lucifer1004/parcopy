//! Common test utilities for integration tests.

use std::fs;
use std::path::Path;
use tempfile::TempDir;

/// A test fixture that provides source and destination directories.
pub struct TestFixture {
    pub src: TempDir,
    pub dst: TempDir,
}

impl TestFixture {
    /// Create a new test fixture with fresh source and destination directories.
    pub fn new() -> Self {
        Self {
            src: TempDir::new().expect("Failed to create temp source dir"),
            dst: TempDir::new().expect("Failed to create temp dest dir"),
        }
    }

    /// Create a specified number of files with the given size (in bytes).
    pub fn create_files(&self, count: usize, size: usize) {
        for i in 0..count {
            let content = "x".repeat(size);
            fs::write(self.src.path().join(format!("file{}.txt", i)), content)
                .expect("Failed to write file");
        }
    }

    /// Create a nested directory structure with files.
    pub fn create_nested_structure(&self, depth: usize, files_per_level: usize) {
        let mut current_path = self.src.path().to_path_buf();
        for level in 0..depth {
            current_path = current_path.join(format!("level{}", level));
            fs::create_dir_all(&current_path).expect("Failed to create directory");
            for i in 0..files_per_level {
                fs::write(
                    current_path.join(format!("file{}.txt", i)),
                    format!("content at level {}", level),
                )
                .expect("Failed to write file");
            }
        }
    }

    /// Count all files in a directory (non-recursive).
    pub fn count_files(&self, dir: &Path) -> usize {
        fs::read_dir(dir)
            .expect("Failed to read directory")
            .filter_map(|e| e.ok())
            .filter(|e| e.path().is_file())
            .count()
    }

    /// Count all files in a directory recursively.
    pub fn count_files_recursive(&self, dir: &Path) -> usize {
        let mut count = 0;
        if dir.is_dir() {
            for entry in fs::read_dir(dir).expect("Failed to read directory") {
                let entry = entry.expect("Failed to read entry");
                let path = entry.path();
                if path.is_dir() {
                    count += self.count_files_recursive(&path);
                } else {
                    count += 1;
                }
            }
        }
        count
    }

    /// Check if a file exists and has the expected content.
    pub fn assert_file_content(&self, path: &Path, expected: &str) {
        assert!(path.exists(), "File does not exist: {:?}", path);
        let actual = fs::read_to_string(path).expect("Failed to read file");
        assert_eq!(actual, expected, "File content mismatch");
    }
}

impl Default for TestFixture {
    fn default() -> Self {
        Self::new()
    }
}

/// Create a directory with files for testing.
pub fn create_test_directory(path: &Path, file_count: usize, file_size: usize) {
    fs::create_dir_all(path).expect("Failed to create directory");
    for i in 0..file_count {
        let content = "x".repeat(file_size);
        fs::write(path.join(format!("file{}.txt", i)), content).expect("Failed to write file");
    }
}

/// Check if a command is available on the system.
#[cfg(target_os = "linux")]
pub fn command_exists(cmd: &str) -> bool {
    std::process::Command::new("which")
        .arg(cmd)
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}
