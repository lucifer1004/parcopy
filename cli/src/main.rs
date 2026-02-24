//! pcp - Parallel Copy
//!
//! A fast, parallel file/directory copy command powered by parcopy.

use clap::{Parser, ValueEnum};
use indicatif::{ProgressBar, ProgressStyle};
use parcopy::{
    CopyOptions, CopyStats, Error as ParcopyError, ErrorCode, OnConflict, copy_dir, copy_file,
    is_no_space_error,
};
use serde_json::{Value, json};
use std::fs::Metadata;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};
use thiserror::Error;

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
    #[arg(short = 'c', long, value_enum)]
    on_conflict: Option<ConflictStrategy>,

    /// Profile-driven defaults
    #[arg(long, value_enum, default_value = "modern")]
    profile: ProfileName,

    /// Output format
    #[arg(long, value_enum, default_value = "human")]
    output: OutputMode,

    /// Disable progress bar
    #[arg(short = 'q', long)]
    quiet: bool,

    /// Do not preserve file timestamps
    #[arg(long)]
    no_times: bool,

    /// Do not preserve file permissions
    #[arg(long)]
    no_perms: bool,

    /// Do not preserve Windows file attributes (hidden, system, etc.)
    ///
    /// This option only has an effect on Windows.
    #[arg(long)]
    no_win_attrs: bool,

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
    #[arg(short = 'n', long = "plan", alias = "dry-run")]
    plan: bool,

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

impl ConflictStrategy {
    fn as_str(self) -> &'static str {
        match self {
            Self::Skip => "skip",
            Self::Overwrite => "overwrite",
            Self::Error => "error",
            Self::Update => "update_newer",
        }
    }
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

#[derive(Debug, Clone, Copy, ValueEnum)]
enum ProfileName {
    Modern,
    Safe,
    Fast,
}

impl ProfileName {
    fn as_str(self) -> &'static str {
        match self {
            Self::Modern => "modern",
            Self::Safe => "safe",
            Self::Fast => "fast",
        }
    }
}

#[derive(Debug, Clone, Copy, ValueEnum, PartialEq, Eq)]
enum OutputMode {
    Human,
    Json,
    Jsonl,
}

impl OutputMode {
    fn as_str(self) -> &'static str {
        match self {
            Self::Human => "human",
            Self::Json => "json",
            Self::Jsonl => "jsonl",
        }
    }
}

#[derive(Debug, Clone, Copy)]
struct ProfileDefaults {
    on_conflict: ConflictStrategy,
    preserve_timestamps: bool,
    preserve_permissions: bool,
    fsync: bool,
    preserve_symlinks: bool,
    verbose: bool,
}

fn profile_defaults(profile: ProfileName) -> ProfileDefaults {
    match profile {
        ProfileName::Modern => ProfileDefaults {
            on_conflict: ConflictStrategy::Skip,
            preserve_timestamps: true,
            preserve_permissions: true,
            fsync: true,
            preserve_symlinks: true,
            verbose: false,
        },
        ProfileName::Safe => ProfileDefaults {
            on_conflict: ConflictStrategy::Error,
            preserve_timestamps: true,
            preserve_permissions: true,
            fsync: true,
            preserve_symlinks: true,
            verbose: true,
        },
        ProfileName::Fast => ProfileDefaults {
            on_conflict: ConflictStrategy::Skip,
            preserve_timestamps: false,
            preserve_permissions: false,
            fsync: false,
            preserve_symlinks: true,
            verbose: false,
        },
    }
}

type CliResult<T> = std::result::Result<T, CliError>;

#[derive(Debug, Error)]
enum CliError {
    #[error("Source is a directory. Use -r/--recursive to copy directories: {path}")]
    SourceIsDirectoryWithoutRecursive { path: PathBuf },

    #[error("Source does not exist: {path}")]
    SourceNotFound { path: PathBuf },

    #[error("Failed to read source metadata: {path}: {source}")]
    SourceMetadata { path: PathBuf, source: io::Error },

    #[error("Target is not a directory: {path}")]
    TargetNotDirectory { path: PathBuf },

    #[error("Missing destination operand after '{operand}'")]
    MissingDestinationOperand { operand: PathBuf },

    #[error("Target '{path}' is not a directory (when copying multiple sources)")]
    MultiSourceTargetNotDirectory { path: PathBuf },

    #[error("Source has no filename: {path}")]
    SourceHasNoFilename { path: PathBuf },

    #[error("Failed to create directory: {path}: {source}")]
    CreateDirectory { path: PathBuf, source: io::Error },

    #[error("Failed to copy directory: {path}: {source}")]
    CopyDirectory { path: PathBuf, source: ParcopyError },

    #[error("Failed to copy file: {path}: {source}")]
    CopyFile { path: PathBuf, source: ParcopyError },

    #[error("Failed to serialize JSON output: {source}")]
    JsonSerialize { source: serde_json::Error },
}

impl CliError {
    fn code(&self) -> ErrorCode {
        match self {
            Self::SourceIsDirectoryWithoutRecursive { .. }
            | Self::TargetNotDirectory { .. }
            | Self::MissingDestinationOperand { .. }
            | Self::MultiSourceTargetNotDirectory { .. }
            | Self::SourceHasNoFilename { .. } => ErrorCode::InvalidInput,
            Self::SourceNotFound { .. } => ErrorCode::SourceNotFound,
            Self::SourceMetadata { source, .. } | Self::CreateDirectory { source, .. } => {
                io_error_code(source)
            }
            Self::CopyDirectory { source, .. } | Self::CopyFile { source, .. } => source.code(),
            Self::JsonSerialize { .. } => ErrorCode::Internal,
        }
    }
}

#[derive(Debug, Clone)]
struct EffectiveConfig {
    profile: ProfileName,
    conflict_policy: ConflictStrategy,
    preserve_timestamps: bool,
    preserve_permissions: bool,
    fsync: bool,
    symlink_mode: &'static str,
    output_mode: OutputMode,
    verbose: bool,
}

impl EffectiveConfig {
    fn to_json_value(&self) -> Value {
        json!({
            "profile": self.profile.as_str(),
            "conflict_policy": self.conflict_policy.as_str(),
            "preserve_timestamps": self.preserve_timestamps,
            "preserve_permissions": self.preserve_permissions,
            "fsync": self.fsync,
            "symlink_mode": self.symlink_mode,
            "output_mode": self.output_mode.as_str(),
        })
    }

    fn print_human_stderr(&self) {
        eprintln!("Effective configuration:");
        eprintln!("  profile: {}", self.profile.as_str());
        eprintln!("  conflict_policy: {}", self.conflict_policy.as_str());
        eprintln!("  preserve_timestamps: {}", self.preserve_timestamps);
        eprintln!("  preserve_permissions: {}", self.preserve_permissions);
        eprintln!("  fsync: {}", self.fsync);
        eprintln!("  symlink_mode: {}", self.symlink_mode);
        eprintln!("  output_mode: {}", self.output_mode.as_str());
    }
}

#[derive(Debug, Clone)]
struct PlanItem {
    source: PathBuf,
    destination: PathBuf,
    source_size: u64,
    action: &'static str,
    reason: &'static str,
}

impl PlanItem {
    fn to_json_value(&self) -> Value {
        json!({
            "source": display_path(&self.source),
            "destination": display_path(&self.destination),
            "action": self.action,
            "reason": self.reason,
        })
    }

    fn to_jsonl_record(&self) -> Value {
        json!({
            "schema_version": "1.0",
            "record_type": "plan_item",
            "source": display_path(&self.source),
            "destination": display_path(&self.destination),
            "action": self.action,
            "reason": self.reason,
        })
    }
}

#[derive(Debug, Clone)]
struct ExecuteItem {
    source: String,
    destination: String,
    outcome: &'static str,
    bytes_copied: Option<u64>,
    error_code: Option<String>,
    error_message: Option<String>,
}

impl ExecuteItem {
    fn copied(source: &Path, destination: &Path, bytes_copied: u64) -> Self {
        Self {
            source: display_path(source),
            destination: display_path(destination),
            outcome: "copied",
            bytes_copied: Some(bytes_copied),
            error_code: None,
            error_message: None,
        }
    }

    fn skipped(source: &Path, destination: &Path) -> Self {
        Self {
            source: display_path(source),
            destination: display_path(destination),
            outcome: "skipped",
            bytes_copied: None,
            error_code: None,
            error_message: None,
        }
    }

    fn failed(
        source: impl Into<String>,
        destination: impl Into<String>,
        code: ErrorCode,
        message: String,
    ) -> Self {
        Self {
            source: source.into(),
            destination: destination.into(),
            outcome: "failed",
            bytes_copied: None,
            error_code: Some(code.as_str().to_owned()),
            error_message: Some(message),
        }
    }

    fn to_json_value(&self) -> Value {
        let mut obj = serde_json::Map::new();
        obj.insert("source".to_owned(), Value::String(self.source.clone()));
        obj.insert(
            "destination".to_owned(),
            Value::String(self.destination.clone()),
        );
        obj.insert("outcome".to_owned(), Value::String(self.outcome.to_owned()));

        if let Some(bytes) = self.bytes_copied {
            obj.insert("bytes_copied".to_owned(), Value::Number(bytes.into()));
        }
        if let Some(ref code) = self.error_code {
            obj.insert("error_code".to_owned(), Value::String(code.clone()));
        }
        if let Some(ref message) = self.error_message {
            obj.insert("error_message".to_owned(), Value::String(message.clone()));
        }

        Value::Object(obj)
    }

    fn to_jsonl_record(&self) -> Value {
        let mut obj = serde_json::Map::new();
        obj.insert("schema_version".to_owned(), Value::String("1.0".to_owned()));
        obj.insert(
            "record_type".to_owned(),
            Value::String("execute_item".to_owned()),
        );
        obj.insert("source".to_owned(), Value::String(self.source.clone()));
        obj.insert(
            "destination".to_owned(),
            Value::String(self.destination.clone()),
        );
        obj.insert("outcome".to_owned(), Value::String(self.outcome.to_owned()));

        if let Some(bytes) = self.bytes_copied {
            obj.insert("bytes_copied".to_owned(), Value::Number(bytes.into()));
        }
        if let Some(ref code) = self.error_code {
            obj.insert("error_code".to_owned(), Value::String(code.clone()));
        }
        if let Some(ref message) = self.error_message {
            obj.insert("error_message".to_owned(), Value::String(message.clone()));
        }

        Value::Object(obj)
    }
}

fn io_error_code(error: &io::Error) -> ErrorCode {
    if is_no_space_error(error) {
        return ErrorCode::NoSpace;
    }
    if error.kind() == io::ErrorKind::PermissionDenied {
        return ErrorCode::PermissionDenied;
    }
    ErrorCode::IoError
}

fn cancellation_stats(error: &CliError) -> Option<(u64, u64)> {
    match error {
        CliError::CopyDirectory {
            source:
                ParcopyError::Cancelled {
                    files_copied,
                    bytes_copied,
                    ..
                },
            ..
        }
        | CliError::CopyFile {
            source:
                ParcopyError::Cancelled {
                    files_copied,
                    bytes_copied,
                    ..
                },
            ..
        } => Some((*files_copied, *bytes_copied)),
        _ => None,
    }
}

fn exit_code_for(code: ErrorCode) -> i32 {
    match code {
        ErrorCode::InvalidInput => 2,
        _ => 1,
    }
}

fn main() {
    if let Err(error) = run() {
        if let Some((files_copied, bytes_copied)) = cancellation_stats(&error) {
            eprintln!(
                "Cancelled after copying {} files ({}).",
                files_copied,
                format_bytes(bytes_copied)
            );
            eprintln!("Re-run with the same command to resume.");
            std::process::exit(130);
        }
        eprintln!("error[{}]: {}", error.code(), error);
        std::process::exit(exit_code_for(error.code()));
    }
}

fn run() -> CliResult<()> {
    let args = Args::parse();

    let (sources, dest) = resolve_sources_and_dest(&args)?;

    let mut sources_with_meta: Vec<(PathBuf, Metadata)> = Vec::with_capacity(sources.len());
    for src in sources {
        match src.metadata() {
            Ok(meta) => {
                if meta.is_dir() && !args.recursive {
                    return Err(CliError::SourceIsDirectoryWithoutRecursive { path: src });
                }
                sources_with_meta.push((src, meta));
            }
            Err(source) if source.kind() == io::ErrorKind::NotFound => {
                return Err(CliError::SourceNotFound { path: src });
            }
            Err(source) => {
                return Err(CliError::SourceMetadata { path: src, source });
            }
        }
    }

    let (mut options, effective_config) = build_options_and_effective_config(&args);

    let cancel = Arc::new(AtomicBool::new(false));
    {
        let cancel_clone = cancel.clone();
        ctrlc::set_handler(move || {
            if cancel_clone.load(Ordering::Relaxed) {
                eprintln!("\nForce quit.");
                std::process::exit(130);
            }
            cancel_clone.store(true, Ordering::Relaxed);
            eprintln!(
                "\nCancelling... finishing in-flight files. Press Ctrl+C again to abort immediately."
            );
        })
        .ok();
    }
    options = options.with_cancel_token(cancel);

    let plan_items = build_plan_items(&sources_with_meta, &dest, options.on_conflict)?;

    if effective_config.output_mode == OutputMode::Human && effective_config.verbose {
        effective_config.print_human_stderr();
    }

    if args.plan {
        emit_plan_output(effective_config.output_mode, &effective_config, &plan_items)?;
        return Ok(());
    }

    let pb = if effective_config.output_mode == OutputMode::Human && !args.quiet {
        let pb = ProgressBar::new_spinner();
        let style = ProgressStyle::default_spinner().template("{spinner:.green} {msg}");
        if let Ok(style) = style {
            pb.set_style(style);
            pb.enable_steady_tick(Duration::from_millis(100));
            if sources_with_meta.len() == 1 {
                pb.set_message(format!("Copying {}...", sources_with_meta[0].0.display()));
            } else {
                pb.set_message(format!("Copying {} items...", sources_with_meta.len()));
            }
            Some(pb)
        } else {
            None
        }
    } else {
        None
    };

    let start_time = Instant::now();
    let copy_result = copy_sources(&sources_with_meta, &dest, &options);
    let total_duration = start_time.elapsed();

    if let Some(pb) = pb {
        pb.finish_and_clear();
    }

    match copy_result {
        Ok(mut stats) => {
            if sources_with_meta.len() > 1 {
                stats.duration = total_duration;
            }

            if effective_config.output_mode == OutputMode::Human {
                print_stats(&stats, effective_config.verbose);
            } else {
                let execute_items = build_execute_items_from_success_plan(&plan_items);
                emit_execute_output(
                    effective_config.output_mode,
                    &effective_config,
                    &execute_items,
                )?;
            }
            Ok(())
        }
        Err(error) => {
            if cancellation_stats(&error).is_none()
                && effective_config.output_mode != OutputMode::Human
            {
                let failure_item = build_execute_failure_item(&error, &plan_items);
                emit_execute_output(
                    effective_config.output_mode,
                    &effective_config,
                    &[failure_item],
                )?;
            }
            Err(error)
        }
    }
}

fn resolve_sources_and_dest(args: &Args) -> CliResult<(Vec<PathBuf>, PathBuf)> {
    if let Some(ref target_dir) = args.target_directory {
        if !target_dir.is_dir() && target_dir.exists() {
            return Err(CliError::TargetNotDirectory {
                path: target_dir.clone(),
            });
        }
        Ok((args.sources.clone(), target_dir.clone()))
    } else if args.sources.len() == 1 {
        Err(CliError::MissingDestinationOperand {
            operand: args.sources[0].clone(),
        })
    } else if args.sources.len() == 2 {
        let src = args.sources[0].clone();
        let dest = args.sources[1].clone();
        Ok((vec![src], dest))
    } else {
        let (sources_slice, dest_slice) = args.sources.split_at(args.sources.len() - 1);
        let dest = &dest_slice[0];

        if !dest.is_dir() && dest.exists() {
            return Err(CliError::MultiSourceTargetNotDirectory { path: dest.clone() });
        }

        Ok((sources_slice.to_vec(), dest.clone()))
    }
}

fn build_options_and_effective_config(args: &Args) -> (CopyOptions, EffectiveConfig) {
    let defaults = profile_defaults(args.profile);

    let conflict = args.on_conflict.unwrap_or(defaults.on_conflict);

    let mut preserve_timestamps = defaults.preserve_timestamps;
    if args.no_times {
        preserve_timestamps = false;
    }

    let mut preserve_permissions = defaults.preserve_permissions;
    if args.no_perms {
        preserve_permissions = false;
    }

    let mut fsync = defaults.fsync;
    if args.no_sync {
        fsync = false;
    }

    let mut preserve_symlinks = defaults.preserve_symlinks;
    if args.follow_symlinks {
        preserve_symlinks = false;
    }

    let mut verbose = defaults.verbose;
    if args.verbose {
        verbose = true;
    }

    let mut options = CopyOptions::default()
        .with_parallel(args.jobs)
        .with_on_conflict(conflict.into());

    if !preserve_timestamps {
        options = options.without_timestamps();
    }
    if !preserve_permissions {
        options.preserve_permissions = false;
        options.preserve_dir_permissions = false;
    }
    if args.no_win_attrs {
        options = options.without_windows_attributes();
    }
    if !fsync {
        options = options.without_fsync();
    }
    if args.block_escaping_symlinks {
        options = options.with_block_escaping_symlinks();
    }
    if !preserve_symlinks {
        options.preserve_symlinks = false;
    }
    if let Some(depth) = args.max_depth {
        options = options.with_max_depth(depth);
    }

    if verbose && args.output == OutputMode::Human {
        options = options.with_warn_handler(|msg| {
            eprintln!("warning: {}", msg);
        });
    }

    let effective_config = EffectiveConfig {
        profile: args.profile,
        conflict_policy: conflict,
        preserve_timestamps,
        preserve_permissions,
        fsync,
        symlink_mode: if preserve_symlinks {
            "preserve"
        } else {
            "follow"
        },
        output_mode: args.output,
        verbose,
    };

    (options, effective_config)
}

fn build_plan_items(
    sources_with_meta: &[(PathBuf, Metadata)],
    dest: &PathBuf,
    on_conflict: OnConflict,
) -> CliResult<Vec<PlanItem>> {
    let mut items = Vec::with_capacity(sources_with_meta.len());
    let (dest_is_dir, mut dest_created) = match dest.metadata() {
        Ok(m) => (m.is_dir(), true),
        Err(_) => (false, false),
    };
    let multi_source = sources_with_meta.len() > 1;

    for (src, src_meta) in sources_with_meta {
        let actual_dest = resolve_actual_destination_path(
            src,
            dest,
            dest_is_dir,
            multi_source,
            &mut dest_created,
            false,
        )?;

        let (action, reason) = classify_plan_action(src_meta, &actual_dest, on_conflict);

        items.push(PlanItem {
            source: src.clone(),
            destination: actual_dest,
            source_size: src_meta.len(),
            action,
            reason,
        });
    }

    Ok(items)
}

fn classify_plan_action(
    source_meta: &Metadata,
    destination: &Path,
    on_conflict: OnConflict,
) -> (&'static str, &'static str) {
    if !destination.exists() {
        return ("copy", "not_exists");
    }

    match on_conflict {
        OnConflict::Skip => ("skip", "exists"),
        OnConflict::Overwrite => ("overwrite", "exists"),
        OnConflict::Error => ("error", "exists"),
        OnConflict::UpdateNewer => {
            let destination_meta = match destination.metadata() {
                Ok(meta) => meta,
                Err(_) => return ("copy", "exists"),
            };

            let src_mtime = source_meta.modified();
            let dst_mtime = destination_meta.modified();

            match (src_mtime, dst_mtime) {
                (Ok(src), Ok(dst)) if src <= dst => ("skip", "newer_or_same"),
                _ => ("copy", "exists"),
            }
        }
    }
}

fn emit_plan_output(
    output_mode: OutputMode,
    effective_config: &EffectiveConfig,
    items: &[PlanItem],
) -> CliResult<()> {
    match output_mode {
        OutputMode::Human => {
            println!("Plan ({} items):", items.len());
            for item in items {
                println!(
                    "  {} {} -> {} ({})",
                    item.action,
                    item.source.display(),
                    item.destination.display(),
                    item.reason
                );
            }
            Ok(())
        }
        OutputMode::Json => {
            let payload = json!({
                "schema_version": "1.0",
                "mode": "plan",
                "effective_config": effective_config.to_json_value(),
                "items": items.iter().map(PlanItem::to_json_value).collect::<Vec<Value>>(),
            });
            print_json_value(&payload)
        }
        OutputMode::Jsonl => {
            let effective_record = json!({
                "schema_version": "1.0",
                "record_type": "effective_config",
                "mode": "plan",
                "effective_config": effective_config.to_json_value(),
            });
            print_json_value(&effective_record)?;
            for item in items {
                print_json_value(&item.to_jsonl_record())?;
            }
            Ok(())
        }
    }
}

fn build_execute_items_from_success_plan(plan_items: &[PlanItem]) -> Vec<ExecuteItem> {
    plan_items
        .iter()
        .map(|item| match item.action {
            "skip" => ExecuteItem::skipped(&item.source, &item.destination),
            _ => ExecuteItem::copied(&item.source, &item.destination, item.source_size),
        })
        .collect()
}

fn build_execute_failure_item(error: &CliError, plan_items: &[PlanItem]) -> ExecuteItem {
    match error {
        CliError::CopyDirectory { path, source } | CliError::CopyFile { path, source } => {
            let destination = plan_items
                .iter()
                .find(|item| item.source == *path)
                .map(|item| display_path(&item.destination))
                .unwrap_or_default();
            ExecuteItem::failed(
                display_path(path),
                destination,
                source.code(),
                source.to_string(),
            )
        }
        _ => ExecuteItem::failed(
            String::new(),
            String::new(),
            error.code(),
            error.to_string(),
        ),
    }
}

fn emit_execute_output(
    output_mode: OutputMode,
    effective_config: &EffectiveConfig,
    items: &[ExecuteItem],
) -> CliResult<()> {
    match output_mode {
        OutputMode::Human => Ok(()),
        OutputMode::Json => {
            let payload = json!({
                "schema_version": "1.0",
                "mode": "execute",
                "effective_config": effective_config.to_json_value(),
                "items": items.iter().map(ExecuteItem::to_json_value).collect::<Vec<Value>>(),
            });
            print_json_value(&payload)
        }
        OutputMode::Jsonl => {
            let effective_record = json!({
                "schema_version": "1.0",
                "record_type": "effective_config",
                "mode": "execute",
                "effective_config": effective_config.to_json_value(),
            });
            print_json_value(&effective_record)?;
            for item in items {
                print_json_value(&item.to_jsonl_record())?;
            }
            Ok(())
        }
    }
}

fn resolve_actual_destination_path(
    src: &Path,
    dest: &PathBuf,
    dest_is_dir: bool,
    multi_source: bool,
    dest_created: &mut bool,
    create_destination_dir: bool,
) -> CliResult<PathBuf> {
    if dest_is_dir || multi_source {
        let filename = src
            .file_name()
            .ok_or_else(|| CliError::SourceHasNoFilename {
                path: src.to_path_buf(),
            })?;

        if create_destination_dir && !*dest_created {
            std::fs::create_dir_all(dest).map_err(|source| CliError::CreateDirectory {
                path: dest.clone(),
                source,
            })?;
            *dest_created = true;
        }

        Ok(dest.join(filename))
    } else {
        Ok(dest.clone())
    }
}

fn copy_sources(
    sources_with_meta: &[(PathBuf, Metadata)],
    dest: &PathBuf,
    options: &CopyOptions,
) -> CliResult<CopyStats> {
    let mut total_stats = CopyStats::default();
    let start_time = Instant::now();

    let (dest_is_dir, mut dest_created) = match dest.metadata() {
        Ok(m) => (m.is_dir(), true),
        Err(_) => (false, false),
    };
    let multi_source = sources_with_meta.len() > 1;

    for (src, src_meta) in sources_with_meta {
        let is_dir = src_meta.is_dir();
        let file_size = src_meta.len();

        let actual_dest = resolve_actual_destination_path(
            src,
            dest,
            dest_is_dir,
            multi_source,
            &mut dest_created,
            true,
        )?;

        if is_dir {
            let stats =
                copy_dir(src, &actual_dest, options).map_err(|source| CliError::CopyDirectory {
                    path: src.clone(),
                    source,
                })?;
            total_stats = merge_stats(total_stats, stats);
        } else {
            let copied =
                copy_file(src, &actual_dest, options).map_err(|source| CliError::CopyFile {
                    path: src.clone(),
                    source,
                })?;

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

fn merge_stats(mut a: CopyStats, b: CopyStats) -> CopyStats {
    a.files_copied += b.files_copied;
    a.files_skipped += b.files_skipped;
    a.symlinks_copied += b.symlinks_copied;
    a.symlinks_skipped += b.symlinks_skipped;
    a.dirs_created += b.dirs_created;
    a.bytes_copied += b.bytes_copied;
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

fn print_json_value(value: &Value) -> CliResult<()> {
    let serialized =
        serde_json::to_string(value).map_err(|source| CliError::JsonSerialize { source })?;
    println!("{serialized}");
    Ok(())
}

fn display_path(path: &Path) -> String {
    path.display().to_string()
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
