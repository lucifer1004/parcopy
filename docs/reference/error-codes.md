# pcp/parcopy Error Codes

Generated from `parcopy::error::error_code_specs()`. Do not edit manually.

## Stability

- Meanings of existing codes are stable within a major version.
- New codes may be added in minor releases.
- Removing or changing a code meaning requires a major release.

## CLI Exit Status Mapping

- `0`: success
- `1`: runtime failure (`error_code != invalid_input`)
- `2`: invalid usage/input (`error_code = invalid_input`)
- `130`: cancelled by signal/user interruption

## Error Code Reference

| `error_code`        | Meaning                                            | Typical triggers                                              | Recommended remediation                                |
| ------------------- | -------------------------------------------------- | ------------------------------------------------------------- | ------------------------------------------------------ |
| `invalid_input`     | User input or invocation is invalid.               | Missing destination operand, unsupported source/target shape. | Correct CLI arguments or input paths and retry.        |
| `source_not_found`  | Source path does not exist.                        | Missing file/directory or stale path.                         | Verify the source path and retry.                      |
| `already_exists`    | Destination conflict under selected policy.        | Conflict policy is error and destination exists.              | Choose overwrite/update policy or remove destination.  |
| `permission_denied` | OS denied filesystem access.                       | Read/write blocked by permissions or ACL rules.               | Adjust permissions/ownership and retry.                |
| `no_space`          | Destination storage is full.                       | Disk quota exceeded or filesystem out of free space.          | Free space and rerun; copy is resumable by default.    |
| `cancelled`         | Operation cancelled by user or cancellation token. | Ctrl+C or explicit cancellation request.                      | Rerun with the same command to resume.                 |
| `partial_copy`      | Some items copied, some failed.                    | Batch copy with mixed per-item outcomes.                      | Inspect item-level failures and retry remaining items. |
| `symlink_loop`      | Symlink traversal would recurse infinitely.        | Circular symlink graph detected.                              | Remove/fix the loop or adjust symlink policy.          |
| `io_error`          | Generic I/O error.                                 | Transient filesystem/network I/O failures.                    | Retry and inspect optional low-level error details.    |
| `internal`          | Unexpected internal failure.                       | Invariant breakage or uncategorized internal path.            | Collect context/logs and file a bug report.            |
