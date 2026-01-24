//! pcp - Parallel Copy
//!
//! A fast, parallel file/directory copy command powered by parcopy.

use anyhow::{bail, Context, Result};
use clap::{Parser, ValueEnum};
use indicatif::{ProgressBar, ProgressStyle};
use parcopy::{copy_dir, copy_file, CopyOptions, CopyStats, OnConflict};
use std::fs::Metadata;
use std::path::PathBuf;
use std::time::{Duration, Instant};

/// pcp - Fast parallel file copy
///
/// Copy files and directories with parallel I/O, atomic writes, and smart defaults.
/// Optimized for NFS and network filesystems.
///
/// Usage:
///   pcp SOURCE DEST
///   pcp SOURCE... DIRECTORY
///   pcp -t DIRECTORY SOURCE...
#[derive(Parser, Debug)]
#[command(name = "pcp", version, about, long_about = None)]
struct Args {
    /// Source file(s) or directory(ies)
    ///
    /// When multiple sources are given, the destination must be a directory.
    #[arg(required = true)]
    sources: Vec<PathBuf>,

    /// Target directory (copy all sources into this directory)
    ///
    /// When specified, all SOURCE arguments are copied into DIRECTORY.
    #[arg(short = 't', long = "target-directory", value_name = "DIRECTORY")]
    target_directory: Option<PathBuf>,

    /// Copy directories recursively
    #[arg(short = 'r', long)]
    recursive: bool,

    /// Number of parallel copy operations
    #[arg(short = 'j', long, default_value = "16")]
    jobs: usize,

    /// Conflict resolution strategy
    #[arg(short = 'c', long, value_enum, default_value = "skip")]
    on_conflict: ConflictStrategy,

    /// Disable progress bar
    #[arg(short = 'q', long)]
    quiet: bool,

    /// Do not preserve file timestamps
    #[arg(long)]
    no_times: bool,

    /// Do not preserve file permissions
    #[arg(long)]
    no_perms: bool,

    /// Do not call fsync after each file (faster but less safe)
    #[arg(long)]
    no_sync: bool,

    /// Block symlinks that escape upward with ".." components
    #[arg(long)]
    block_escaping_symlinks: bool,

    /// Follow symlinks instead of preserving them
    #[arg(short = 'L', long)]
    follow_symlinks: bool,

    /// Maximum directory depth (default: unlimited)
    #[arg(long)]
    max_depth: Option<usize>,

    /// Print what would be copied without actually copying
    #[arg(short = 'n', long)]
    dry_run: bool,

    /// Verbose output
    #[arg(short = 'v', long)]
    verbose: bool,
}

#[derive(Debug, Clone, Copy, ValueEnum)]
enum ConflictStrategy {
    /// Skip existing files (default, enables resume)
    Skip,
    /// Overwrite existing files
    Overwrite,
    /// Fail if destination exists
    Error,
    /// Only copy if source is newer
    Update,
}

impl From<ConflictStrategy> for OnConflict {
    fn from(s: ConflictStrategy) -> Self {
        match s {
            ConflictStrategy::Skip => OnConflict::Skip,
            ConflictStrategy::Overwrite => OnConflict::Overwrite,
            ConflictStrategy::Error => OnConflict::Error,
            ConflictStrategy::Update => OnConflict::UpdateNewer,
        }
    }
}

fn main() -> Result<()> {
    let args = Args::parse();

    // Determine sources and destination based on arguments
    let (sources, dest) = resolve_sources_and_dest(&args)?;

    // Validate sources and collect metadata (single stat per source, reused later)
    let mut sources_with_meta: Vec<(PathBuf, Metadata)> = Vec::with_capacity(sources.len());
    for src in sources {
        match src.metadata() {
            Ok(meta) => {
                if meta.is_dir() && !args.recursive {
                    bail!(
                        "Source is a directory. Use -r/--recursive to copy directories: {}",
                        src.display()
                    );
                }
                sources_with_meta.push((src, meta));
            }
            Err(_) => bail!("Source does not exist: {}", src.display()),
        }
    }

    // Build options
    let options = build_options(&args);

    // Dry run mode
    if args.dry_run {
        print_dry_run(&sources_with_meta, &dest, &args);
        return Ok(());
    }

    // Create progress bar
    let pb = if !args.quiet {
        let pb = ProgressBar::new_spinner();
        pb.set_style(
            ProgressStyle::default_spinner()
                .template("{spinner:.green} {msg}")
                .unwrap(),
        );
        pb.enable_steady_tick(Duration::from_millis(100));
        if sources_with_meta.len() == 1 {
            pb.set_message(format!("Copying {}...", sources_with_meta[0].0.display()));
        } else {
            pb.set_message(format!("Copying {} items...", sources_with_meta.len()));
        }
        Some(pb)
    } else {
        None
    };

    // Perform the copy
    let start_time = Instant::now();
    let result = copy_sources(&sources_with_meta, &dest, &options);
    let total_duration = start_time.elapsed();

    // Finish progress bar
    if let Some(pb) = pb {
        pb.finish_and_clear();
    }

    // Handle result
    match result {
        Ok(mut stats) => {
            // Use total wall-clock time for multi-source copies
            if sources_with_meta.len() > 1 {
                stats.duration = total_duration;
            }
            print_stats(&stats, args.verbose);
            Ok(())
        }
        Err(e) => {
            eprintln!("error: {:#}", e);
            std::process::exit(1);
        }
    }
}

/// Resolve sources and destination from command-line arguments.
///
/// Handles three usage patterns:
/// 1. `pcp SOURCE DEST` - single source to destination
/// 2. `pcp SOURCE... DIRECTORY` - multiple sources to directory
/// 3. `pcp -t DIRECTORY SOURCE...` - explicit target directory
fn resolve_sources_and_dest(args: &Args) -> Result<(Vec<PathBuf>, PathBuf)> {
    if let Some(ref target_dir) = args.target_directory {
        // -t DIRECTORY mode: all args are sources
        if args.sources.is_empty() {
            bail!("No source files specified");
        }
        if !target_dir.is_dir() && target_dir.exists() {
            bail!("Target is not a directory: {}", target_dir.display());
        }
        Ok((args.sources.clone(), target_dir.clone()))
    } else if args.sources.len() == 1 {
        // Single argument - this is an error, we need at least source and dest
        bail!(
            "Missing destination operand after '{}'",
            args.sources[0].display()
        );
    } else if args.sources.len() == 2 {
        // Two arguments: SOURCE DEST
        let src = args.sources[0].clone();
        let dest = args.sources[1].clone();
        Ok((vec![src], dest))
    } else {
        // Multiple arguments: SOURCE... DIRECTORY
        // Last argument must be the destination directory
        let mut sources = args.sources.clone();
        let dest = sources.pop().unwrap();

        if !dest.is_dir() && dest.exists() {
            bail!(
                "Target '{}' is not a directory (when copying multiple sources)",
                dest.display()
            );
        }

        Ok((sources, dest))
    }
}

/// Build CopyOptions from command-line arguments
fn build_options(args: &Args) -> CopyOptions {
    let mut options = CopyOptions::default()
        .with_parallel(args.jobs)
        .with_on_conflict(args.on_conflict.into());

    if args.no_times {
        options = options.without_timestamps();
    }
    if args.no_perms {
        options.preserve_permissions = false;
        options.preserve_dir_permissions = false;
    }
    if args.no_sync {
        options = options.without_fsync();
    }
    if args.block_escaping_symlinks {
        options = options.with_block_escaping_symlinks();
    }
    if args.follow_symlinks {
        options.preserve_symlinks = false;
    }
    if let Some(depth) = args.max_depth {
        options = options.with_max_depth(depth);
    }

    // Verbose warning handler
    if args.verbose {
        options = options.with_warn_handler(|msg| {
            eprintln!("warning: {}", msg);
        });
    }

    options
}

/// Print dry-run information
fn print_dry_run(sources_with_meta: &[(PathBuf, Metadata)], dest: &std::path::Path, args: &Args) {
    println!("Dry run - would copy:");
    for (src, meta) in sources_with_meta {
        // Reuse pre-fetched metadata (no stat call)
        let src_type = if meta.is_dir() { "directory" } else { "file" };
        println!("  {} {} -> {}", src_type, src.display(), dest.display());
    }
    println!("Options:");
    println!("  Jobs: {}", args.jobs);
    println!("  On conflict: {:?}", args.on_conflict);
}

/// Copy multiple sources to destination, aggregating stats
/// Takes sources with pre-fetched metadata to avoid redundant stat calls.
fn copy_sources(
    sources_with_meta: &[(PathBuf, Metadata)],
    dest: &PathBuf,
    options: &CopyOptions,
) -> Result<CopyStats> {
    let mut total_stats = CopyStats::default();
    let start_time = Instant::now();

    // Check destination once (single stat call, avoid repeated checks in loop)
    let (dest_is_dir, mut dest_created) = match dest.metadata() {
        Ok(m) => (m.is_dir(), true),
        Err(_) => (false, false),
    };
    let multi_source = sources_with_meta.len() > 1;

    for (src, src_meta) in sources_with_meta {
        // Reuse pre-fetched metadata (no stat call here)
        let is_dir = src_meta.is_dir();
        let file_size = src_meta.len();

        // Determine actual destination path
        let actual_dest = if dest_is_dir || multi_source {
            // Copying into a directory - use source filename
            let filename = src
                .file_name()
                .with_context(|| format!("Source has no filename: {}", src.display()))?;

            // Create destination directory if it doesn't exist (for multi-source)
            if !dest_created {
                std::fs::create_dir_all(dest)
                    .with_context(|| format!("Failed to create directory: {}", dest.display()))?;
                dest_created = true;
            }

            dest.join(filename)
        } else {
            dest.clone()
        };

        if is_dir {
            let stats = copy_dir(src, &actual_dest, options)
                .with_context(|| format!("Failed to copy directory: {}", src.display()))?;
            total_stats = merge_stats(total_stats, stats);
        } else {
            let copied = copy_file(src, &actual_dest, options)
                .with_context(|| format!("Failed to copy file: {}", src.display()))?;

            if copied {
                total_stats.files_copied += 1;
                total_stats.bytes_copied += file_size;
            } else {
                total_stats.files_skipped += 1;
            }
        }
    }

    total_stats.duration = start_time.elapsed();
    Ok(total_stats)
}

/// Merge two CopyStats
fn merge_stats(mut a: CopyStats, b: CopyStats) -> CopyStats {
    a.files_copied += b.files_copied;
    a.files_skipped += b.files_skipped;
    a.symlinks_copied += b.symlinks_copied;
    a.symlinks_skipped += b.symlinks_skipped;
    a.dirs_created += b.dirs_created;
    a.bytes_copied += b.bytes_copied;
    // Duration is handled separately
    a
}

fn print_stats(stats: &CopyStats, verbose: bool) {
    if stats.files_copied == 0 && stats.symlinks_copied == 0 && stats.dirs_created == 0 {
        if stats.files_skipped > 0 {
            println!(
                "Nothing to copy ({} files already exist)",
                stats.files_skipped
            );
        } else {
            println!("Nothing to copy");
        }
        return;
    }

    // Format bytes
    let bytes_str = format_bytes(stats.bytes_copied);

    if verbose {
        println!("Copy completed in {:?}", stats.duration);
        println!("  Files copied:   {}", stats.files_copied);
        println!("  Files skipped:  {}", stats.files_skipped);
        println!("  Symlinks:       {}", stats.symlinks_copied);
        println!("  Directories:    {}", stats.dirs_created);
        println!("  Total size:     {}", bytes_str);

        if stats.duration.as_secs_f64() > 0.0 {
            let speed = stats.bytes_copied as f64 / stats.duration.as_secs_f64();
            println!("  Speed:          {}/s", format_bytes(speed as u64));
        }
    } else {
        // Compact output
        let mut parts = vec![];
        if stats.files_copied > 0 {
            parts.push(format!("{} files", stats.files_copied));
        }
        if stats.symlinks_copied > 0 {
            parts.push(format!("{} symlinks", stats.symlinks_copied));
        }
        if stats.dirs_created > 0 {
            parts.push(format!("{} dirs", stats.dirs_created));
        }

        if parts.is_empty() {
            println!("Done");
        } else {
            println!("Copied {} ({})", parts.join(", "), bytes_str);
        }
    }
}

fn format_bytes(bytes: u64) -> String {
    const KB: u64 = 1024;
    const MB: u64 = KB * 1024;
    const GB: u64 = MB * 1024;
    const TB: u64 = GB * 1024;

    if bytes >= TB {
        format!("{:.2} TB", bytes as f64 / TB as f64)
    } else if bytes >= GB {
        format!("{:.2} GB", bytes as f64 / GB as f64)
    } else if bytes >= MB {
        format!("{:.2} MB", bytes as f64 / MB as f64)
    } else if bytes >= KB {
        format!("{:.2} KB", bytes as f64 / KB as f64)
    } else {
        format!("{} B", bytes)
    }
}
