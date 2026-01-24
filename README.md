# parcopy

[![Crates.io](https://img.shields.io/crates/v/parcopy.svg)](https://crates.io/crates/parcopy)
[![Documentation](https://docs.rs/parcopy/badge.svg)](https://docs.rs/parcopy/)
[![License](https://img.shields.io/crates/l/parcopy.svg)](LICENSE-MIT)

**Parallel, atomic, and safe file/directory copying for Rust.**

A production-grade library for copying files and directories with safety guarantees that go beyond the standard library.

## Features

- **Parallel copying** - Uses rayon for concurrent file operations
- **Atomic writes** - Uses temp file + rename pattern to ensure no partial files
- **TOCTOU safe** - Uses `persist_noclobber` to prevent race conditions
- **Incremental copy** - Only copy files newer than destination (`UpdateNewer`)
- **Reflink support** - Instant copy-on-write on btrfs/XFS/APFS
- **Timestamp preserving** - Copies file modification and access times
- **Permission preserving** - Copies file and directory permissions
- **Symlink aware** - Correctly handles symlinks without following them
- **Symlink loop detection** - Prevents infinite recursion from circular symlinks
- **Security hardened** - Detects and optionally blocks escaping symlinks

## Why parcopy?

| Feature                | `std::fs` | `fs_extra` | **parcopy** |
| ---------------------- | --------- | ---------- | ----------- |
| Parallel               | ❌        | ❌         | ✅          |
| Atomic writes          | ❌        | ❌         | ✅          |
| TOCTOU safe            | ❌        | ❌         | ✅          |
| Incremental copy       | ❌        | ❌         | ✅          |
| Reflink/CoW            | ❌        | ❌         | ✅          |
| Timestamp preservation | ❌        | ❌         | ✅          |
| Progress callbacks     | ❌        | ✅         | ✅          |

## Installation

```toml
[dependencies]
parcopy = "0.1"
```

### Optional Features

```toml
[dependencies]
parcopy = { version = "0.1", features = ["progress", "reflink"] }
```

| Feature    | Description                              |
| ---------- | ---------------------------------------- |
| `progress` | Progress bar support with indicatif      |
| `reflink`  | Copy-on-write support for btrfs/XFS/APFS |
| `tracing`  | Structured logging with tracing crate    |
| `serde`    | Serialize/Deserialize for CopyOptions    |
| `full`     | Enable all optional features             |

## Quick Start

### Builder API (Recommended)

The easiest way to use parcopy is with the `CopyBuilder`:

```rust
use parcopy::CopyBuilder;

// Simple copy with smart defaults
let stats = CopyBuilder::new("src", "dst").run()?;
println!("Copied {} files ({} bytes)", stats.files_copied, stats.bytes_copied);
```

### Incremental Backup

Only copy files that have changed:

```rust
use parcopy::CopyBuilder;

let stats = CopyBuilder::new("project", "backup")
    .update_newer()
    .run()?;

println!("Updated {} files, {} already up-to-date",
    stats.files_copied, stats.files_skipped);
```

### High-Performance Copy

Optimize for NFS or network filesystems:

```rust
use parcopy::CopyBuilder;

let stats = CopyBuilder::new("data", "backup")
    .parallel(32)      // More threads for NFS
    .overwrite()       // Replace existing files
    .no_fsync()        // Skip fsync for speed
    .run()?;
```

### Security-Hardened Copy

Copy untrusted directories safely:

```rust
use parcopy::CopyBuilder;

let stats = CopyBuilder::new("untrusted_upload", "safe_location")
    .block_escaping_symlinks()  // Block symlinks with "../"
    .max_depth(10)              // Limit directory depth
    .run()?;
```

## Function API

For more control, use the function API with `CopyOptions`:

```rust
use parcopy::{copy_dir, CopyOptions, OnConflict};
use std::path::Path;

let options = CopyOptions::default()
    .with_parallel(8)
    .with_on_conflict(OnConflict::Overwrite)
    .with_max_depth(100)
    .without_fsync();

let stats = copy_dir(Path::new("src"), Path::new("dst"), &options)?;
```

### Configuration Options

| Option                    | Default | Description                          |
| ------------------------- | ------- | ------------------------------------ |
| `parallel`                | 16      | Number of concurrent copy operations |
| `on_conflict`             | `Skip`  | How to handle existing files         |
| `fsync`                   | `true`  | Sync data to disk after each file    |
| `preserve_permissions`    | `true`  | Copy file permissions                |
| `preserve_timestamps`     | `true`  | Copy file timestamps                 |
| `max_depth`               | `None`  | Maximum directory depth              |
| `block_escaping_symlinks` | `false` | Block symlinks with `..`             |

### Conflict Strategies

| Strategy                  | Description                             |
| ------------------------- | --------------------------------------- |
| `OnConflict::Skip`        | Skip files that already exist (default) |
| `OnConflict::Overwrite`   | Replace existing files                  |
| `OnConflict::UpdateNewer` | Only copy if source is newer            |
| `OnConflict::Error`       | Return error if file exists             |

## Copy Statistics

All copy operations return `CopyStats`:

```rust
use parcopy::CopyBuilder;

let stats = CopyBuilder::new("src", "dst").run()?;

println!("Files copied:   {}", stats.files_copied);
println!("Files skipped:  {}", stats.files_skipped);
println!("Symlinks:       {}", stats.symlinks_copied);
println!("Directories:    {}", stats.dirs_created);
println!("Bytes copied:   {}", stats.bytes_copied);
println!("Duration:       {:?}", stats.duration);
```

## Safety Guarantees

### Atomic Writes

Files are written to a temporary file in the destination directory, then renamed atomically:

1. **No partial files** - Interrupted copies leave no garbage
2. **All-or-nothing** - Other processes see complete files or nothing
3. **Power failure safe** - With `fsync: true`, data survives crashes

### TOCTOU Protection

Uses `renameat2(RENAME_NOREPLACE)` on Linux to atomically fail if the destination was created between our existence check and the rename.

### Symlink Safety

- Symlinks are never followed during directory traversal
- Symlink loops are detected and reported
- Escaping symlinks (`../`) are warned or blocked

## Performance Notes

### NFS Optimization

This crate is optimized for NFS and network filesystems where many small files cause metadata storms. By parallelizing operations, multiple NFS RPCs can be in-flight simultaneously.

```rust
// For slow NFS, reduce parallelism to avoid overwhelming the server
let stats = CopyBuilder::new("src", "dst")
    .parallel(4)
    .run()?;
```

### Local SSD

For local SSDs, parallelism helps less but doesn't hurt:

```rust
// Default parallelism (16) works well for local storage too
let stats = CopyBuilder::new("src", "dst").run()?;
```

### Large Files

For large files, the `reflink` feature provides instant copy-on-write on supported filesystems (btrfs, XFS, APFS):

```toml
[dependencies]
parcopy = { version = "0.1", features = ["reflink"] }
```

## CLI Tool

A CLI tool `pcp` is available in the `cli` directory:

```bash
# Install
cargo install --path cli

# Usage
pcp -r src/ dst/              # Recursive copy
pcp --update-newer src/ dst/  # Incremental copy
pcp -j 8 src/ dst/            # 8 parallel threads
```

## License

Licensed under either of:

- Apache License, Version 2.0 ([LICENSE-APACHE](LICENSE-APACHE))
- MIT license ([LICENSE-MIT](LICENSE-MIT))

at your option.

## Contributing

Contributions are welcome! Please feel free to submit a Pull Request.
