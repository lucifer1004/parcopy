# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## Unreleased

## [v0.2.0] - 2026-02-23

### Added

- "No space left on device" error detection and cleanup
  - New `is_no_space_error()` function to detect ENOSPC/ERROR_DISK_FULL errors
  - New `Error::NoSpace` variant with detailed statistics
  - Automatically clean up successfully copied files when disk runs out of space
  - Fixes [#3](https://github.com/lucifer1004/parcopy/issues/3)

- Verbose output support
  - New `verbose_handler` option in `CopyOptions` for detailed file operation logging
  - New `verbose()` method in `CopyBuilder` for fluent API
  - Reports source and destination paths for copied, skipped, and failed files
  - Uses the previously unused `src` field in internal error tracking

### Changed

- MSRV bumped to 1.83.0

## [v0.1.2] - 2026-02-12

### Added

- Graceful cancellation support
  - New option `cancel_token` in `CopyOptions` for cooperative cancellation
  - New builder method `cancel_token()` in `CopyBuilder`
  - New `Error::Cancelled` variant with partial statistics
  - Two-stage Ctrl+C handling in `pcp` CLI:
    - First press: graceful cancel (finish in-flight files)
    - Second press: hard abort (immediate exit)
  - Re-run with same command to resume (uses default `Skip` strategy)
  - Fixes [#2](https://github.com/lucifer1004/parcopy/issues/2)

## [v0.1.1] - 2026-02-09

### Added

- Windows file attribute preservation (hidden, system, archive, readonly)
  - New option `preserve_windows_attributes` in `CopyOptions` (default: true)
  - New builder method `no_windows_attributes()` in `CopyBuilder`
  - New CLI flag `--no-win-attrs` in `pcp`
  - Fixes [#1](https://github.com/lucifer1004/parcopy/issues/1)

## [v0.1.0] - 2026-01-24

### Added

- Initial release.
