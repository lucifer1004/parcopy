//! Progress reporting support (requires `progress` feature)

use indicatif::{ProgressBar, ProgressStyle};

/// Callback for progress updates
pub type ProgressCallback = Box<dyn Fn(u64, u64) + Send + Sync>;

/// Create a default progress bar for file copying
#[must_use]
pub fn create_progress_bar(total: u64) -> ProgressBar {
    let pb = ProgressBar::new(total);
    pb.set_style(
        ProgressStyle::default_bar()
            .template("{spinner:.green} [{bar:40.cyan/blue}] {pos}/{len} files ({eta})")
            .unwrap_or_else(|_| ProgressStyle::default_bar())
            .progress_chars("=>-"),
    );
    pb
}
