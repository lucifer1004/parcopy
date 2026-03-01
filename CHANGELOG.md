# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

## [0.3.1] - 2026-03-01

### Added

- Mid-file cancellation works on Linux (copy_file_range) (WI-2026-03-01-001)
- Mid-file cancellation works on non-Linux platforms (WI-2026-03-01-001)

## [0.3.0] - 2026-02-25

### Added

- Define profile-based CLI behavior for modern, compat-cp, safe, and fast modes (WI-2026-02-24-001)
- Specify plan and structured output modes for machine-readable automation (WI-2026-02-24-001)
- Specify library-level planner/executor API and typed event model (WI-2026-02-24-001)
- Define stable CLI error_code taxonomy and versioning rules (WI-2026-02-24-005)
- Require public library APIs to return typed enum errors instead of anyhow (WI-2026-02-24-005)
- Align execution JSON error payload with stable code and optional low-level detail (WI-2026-02-24-005)
- Implement RFC-0001 C-PROFILES with built-in modern/safe/fast profiles and effective_config surfacing (WI-2026-02-24-006)
- Implement RFC-0001 C-PLAN-OUTPUT with --plan and stable human/json/jsonl schema contracts (WI-2026-02-24-006)
- Implement RFC-0001 C-LIBRARY-API with plan/execute APIs, policy/runtime separation, and typed events (WI-2026-02-24-006)
- Implement RFC-0001 C-ERROR-MODEL with stable error_code mapping and typed enum errors across public APIs (WI-2026-02-24-006)
- Implement RFC-0001 C-COMPATIBILITY requirements in docs and release migration notes (WI-2026-02-24-006)
- Test for multi-branch symlink to same directory under -L (WI-2026-02-24-010)

### Changed

- Amend RFC-0001 C-ERROR-MODEL to define code metadata as canonical SSOT (WI-2026-02-24-007)
- Remove markdown error-code reference artifact and associated sync-test overhead (WI-2026-02-24-007)
- **BREAKING**: Removed cleanup logic on "no space" errors
  - Files copied before a "no space" error are now retained instead of being deleted
  - Enables resumable copy operations when combined with `OnConflict::Skip` (the default)
  - Re-running the same copy command will skip already-copied files and continue from where it left off
  - `Error::NoSpace` field changed from `cleaned_up: usize` to `remaining: usize`
  - Error message now shows remaining files and suggests re-running to resume
- **BREAKING**: Disallowed overwriting directories with files
  - Prevents accidental data loss from recursive directory deletion
  - Returns `Error::IsADirectory` when attempting to overwrite a directory with a file
  - Previously, `OnConflict::Overwrite` would delete the entire directory tree
- Improved atomic file operations
  - File overwrite now uses atomic `persist()` instead of delete-then-create pattern
  - Eliminates window where neither old nor new file exists on failure
  - Works for overwriting existing files and symlinks

### Fixed

- Make --plan JSON/JSONL contract fully testable with explicit schema and enums (WI-2026-02-24-002)
- Remove ambiguous deprecation window and clarify pre-1.0 breaking-change policy (WI-2026-02-24-002)
- Define where/how effective profile overrides are surfaced for human and machine outputs (WI-2026-02-24-002)
- Define objective compat-cp baseline and required behavior matrix (WI-2026-02-24-002)
- Scope JSON mode=plan requirement to planning context only (WI-2026-02-24-003)
- Align --explain requirement strength across C-PROFILES and C-COMPATIBILITY (WI-2026-02-24-003)
- Remove mandatory compat-cp profile requirement from RFC-0001 (WI-2026-02-24-004)
- Specify execution-mode JSON/JSONL schema alongside planning schema (WI-2026-02-24-004)
- Keep --explain optional while preserving effective-config output guarantees (WI-2026-02-24-004)
- no_space resume test uses sequential copy to avoid parallel race on tiny tmpfs (WI-2026-02-24-008)
- Windows nonexistent-path test accepts os error 123 message (WI-2026-02-24-008)
- Stack-based ancestor detection replaces global visited set (WI-2026-02-24-010)

## [0.2.1] - 2026-02-23

### Fixed

- Windows long path support for files with names >125 characters ([#4](https://github.com/lucifer1004/parcopy/issues/4))
  - Added extended-length path syntax support using `\\?\` prefix on Windows
  - Fixed issue where copying files with long names failed on Windows
  - All file operations (copy, create, remove, symlink) now support long paths
  - Temp file creation and persist operations use extended-length paths
  - Directory creation and removal operations use extended-length paths
  - Windows attribute operations use extended-length paths
  - Added comprehensive integration tests for long path scenarios:
    - Long filename components (150 and 254 characters)
    - Long total paths exceeding 500 characters (deep nesting)
    - Very long total paths exceeding 1000 characters (deep nesting)
    - Paths specifically exceeding old MAX_PATH limit (260 chars)
    - Directory copying with long destination paths
    - Overwrite scenarios with long filenames
    - All tests properly validate extended-length path syntax support

## [0.2.0] - 2026-02-23

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

## [0.1.2] - 2026-02-12

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

## [0.1.1] - 2026-02-09

### Added

- Windows file attribute preservation (hidden, system, archive, readonly)
  - New option `preserve_windows_attributes` in `CopyOptions` (default: true)
  - New builder method `no_windows_attributes()` in `CopyBuilder`
  - New CLI flag `--no-win-attrs` in `pcp`
  - Fixes [#1](https://github.com/lucifer1004/parcopy/issues/1)

## [0.1.0] - 2026-01-24

### Added

- Initial release.
