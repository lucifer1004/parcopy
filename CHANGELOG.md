# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Added

- Windows file attribute preservation (hidden, system, archive, readonly)
  - New option `preserve_windows_attributes` in `CopyOptions` (default: true)
  - New builder method `no_windows_attributes()` in `CopyBuilder`
  - New CLI flag `--no-win-attrs` in `pcp`
  - Fixes [#1](https://github.com/lucifer1004/parcopy/issues/1)

## [v0.1.0] - 2026-01-24

### Added

- Initial release.
