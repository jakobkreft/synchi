---
layout: default
title: Configuration
---

# Configuration

Synchi is configured using a TOML file.  
By default, it looks for `~/.config/synchi/config.toml`, or you can pass a file explicitly with `-c`.

Command-line flags can override any config value for a single run.

## Example Configuration

This is a simple, typical setup syncing a local folder with a remote one over SSH:

```toml
root_a = "./local_root"
root_b = "ssh://user@host:/path/to/remote_root"

include = ["**"]
ignore = ["**/cache/**", "**/build/**"]

hash_mode = "balanced"
force = "none"

skip_hardlinks = true
preserve_owner = false
preserve_permissions = true
````

Most users only need to adjust `root_a`, `root_b`, and possibly `include` / `ignore`.

## Configuration Options

| Key                    | Description                                                                                              | Default      |
| ---------------------- | -------------------------------------------------------------------------------------------------------- | ------------ |
| `root_a`               | First root directory. Stores Synchi’s state and is treated symmetrically unless `force` is used.         | required     |
| `root_b`               | Second root directory. Can be local or an SSH target (`ssh://user@host:port/path`). scp-style `user@host:/path` is not supported. | required     |
| `include`              | Glob patterns of paths to include. Patterns are relative to each root.                                   | `["**"]`     |
| `ignore`               | Glob patterns of paths to exclude. `.synchi` is always ignored.                                          | `[]`         |
| `force`                | `"root_a"`, `"root_b"`, or `"none"`. Forces one-way mirroring or allows two-way sync.                    | `"none"`     |
| `hash_mode`            | `"balanced"` or `"always"`. Controls how aggressively files are hashed.                                  | `"balanced"` |
| `skip_hardlinks`       | Skip files that have multiple hard links on both roots.                                                  | `false`      |
| `preserve_owner`       | Preserve file ownership during sync. Disable for filesystems that reject `chown`.                        | `true`       |
| `preserve_permissions` | Preserve file permissions and mtimes. Disable on non-POSIX filesystems.                                  | `true`       |
| `state_db_name`        | Optional label inside `.synchi/` for the state database. Synchi stores it as `<label>.db`. Use unique names per config if needed. | `state.db`   |

## Include and Ignore Patterns

Patterns use standard glob syntax:

* `*` matches within a single path segment
* `**` matches recursively

Patterns are evaluated relative to each root. For example:

```toml
include = ["Documents/**", "**/*.txt"]
ignore = ["**/node_modules/**"]
```

Include acts as a whitelist. Files not matched by `include` are ignored entirely.

## Hash Modes

* `balanced`
  Hashes files only when metadata changes (size or modification time), but still uses hashes to confirm real content changes. This is faster and safe for most setups. 

* `always`
  Hashes every file on every run. Slower, but useful if timestamps or file sizes cannot be trusted.

## Force Mode

By default, Synchi runs in two-way mode and detects conflicts.

Setting `force` enables one-way mirroring:

* `force = "root_a"` mirrors Root A to Root B
* `force = "root_b"` mirrors Root B to Root A
* `force = "none"` keeps two-way behavior

When force mode is active, conflicts are suppressed because one side always wins.

Without force, `synchi sync` prints the diff summary and then prompts separately for each category that still has work pending (`Copy A → B`, `Copy B → A`, `Delete on A`, `Delete on B`). Reply with `y`/`n`, type `dry` to list the pending paths before deciding, pre-approve via CLI (`--copy-a-to-b yes|no`, etc.), or pass `-y/--yes` to auto-approve all unanswered categories in one go.

## Ownership and Permissions

Some filesystems do not support POSIX ownership or permissions (for example: SMB shares, NAS devices, Android storage).

In those cases, set:

```toml
preserve_owner = false
preserve_permissions = false
```

This avoids errors during extraction and lets the destination filesystem apply its own defaults.

## Command-Line Overrides

Most options can be overridden via CLI flags, including:

* `--root-a`, `--root-b`
* `--state-db-name`
* `--hash-mode`
* `--force`
* `--dry-run`
* `-y / --yes`
* Category approvals: `--copy-a-to-b yes|no`, `--copy-b-to-a yes|no`, `--delete-on-a yes|no`, `--delete-on-b yes|no`

Both `synchi status` and `synchi sync` use the same logic.
Running `status` first shows exactly what `sync` would do.

## Android / Termux

Notes specific to Android live in [Android (Termux)](./termux.md).
