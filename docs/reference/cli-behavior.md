# pcp Canonical CLI Behavior (RFC-0001)

This document defines the canonical CLI behavior model for `pcp`.

## Command Shape

`pcp` supports:

- `pcp SOURCE DEST`
- `pcp SOURCE... DIRECTORY`
- `pcp -t DIRECTORY SOURCE...`

When multiple sources are provided, the destination is treated as a target directory.

## Profiles

`pcp` exposes profile-driven defaults with `--profile <name>`.

Built-in profiles:

- `modern` (default)
- `safe`
- `fast`

Explicit CLI flags always override profile defaults.

## Modes

`pcp` has two execution modes:

- `--plan`: plan-only mode; no filesystem mutation is allowed.
- execution mode (default): performs copy operations.

`--dry-run` is accepted as an alias of `--plan`, while `--plan` is the canonical name.

## Output Contract

Use `--output human|json|jsonl` in either mode.

Machine-readable contract:

- `schema_version` is `"1.0"`.
- `mode` is `"plan"` or `"execute"`.
- `effective_config` is always included in machine output.

JSON:

- Emits one top-level object with keys:
  - `schema_version`
  - `mode`
  - `effective_config`
  - `items`

JSONL:

- Emits exactly one `record_type: "effective_config"` record first.
- Plan items use `record_type: "plan_item"`.
- Execute items use `record_type: "execute_item"`.

## Effective Configuration Visibility

`effective_config` contains at least:

- `profile`
- `conflict_policy`
- `preserve_timestamps`
- `preserve_permissions`
- `fsync`
- `symlink_mode`
- `output_mode`

For human output, `effective_config` is printed to `stderr` when verbose output is enabled.

## Error Surface

- Human errors include stable `error_code` in the format: `error[<error_code>]: ...`
- Stable error code definitions: [`error-codes.md`](./error-codes.md)

Exit statuses:

- `0`: success
- `1`: runtime failure
- `2`: invalid usage/input
- `130`: cancellation by signal/user interruption
