---
layout: default
title: Troubleshooting & FAQ
---

# Troubleshooting & FAQ

Common issues and how to fix them.

## Hashing fails with `No such file or directory (os error 2)`

This usually means a file disappeared between the scan and the hashing step, or your `include` rules no longer cover a path that’s still tracked in the state database.

Things to try:

- Re-run when the directory tree is stable (no builds or cleanup jobs running).
- If you changed `include` significantly, delete `.synchi/<state_db_name>.db` (default `state.db`) and re-initialize so the state matches your new scope.
- Run with `-v` to see which path failed and consider adding it to `ignore`.

## `Failed to acquire lock (file exists)`

If Synchi was interrupted or crashed, the lock file may be left behind.

Fix:

1. Make sure no other Synchi process is running.
2. Remove the lock manually (it is a directory on SSH roots and a file on local roots):

```bash
   rm -rf /path/to/root_a/.synchi/<state_db_name>.lock
```

3. Run Synchi again.

Synchi installs a signal handler, so a single `Ctrl-C` should clean up correctly in normal cases.

## Remote scan errors involving `-printf`

Errors like:

* `find: unknown predicate -printf`
* `find: illegal option -- p`

mean the remote system’s `find` does not support `-printf`.

To fix this:

* Install GNU `findutils` on the remote host, or
* Ensure the BusyBox version of `find` includes `-printf`

## Hardlink mode errors

If you see errors like:

* `Hardlink modes require inode/device IDs`
* `Hardlink modes require remote find with %D/%i support`

then your platform does not provide inode/device IDs for scanning. Skip and preserve need these IDs to build link groups.

Fixes:

* For SSH roots, install GNU `findutils` (so `find -printf` supports `%D` and `%i`).
* If the destination filesystem does not support hardlinks, use `hardlinks = "copy"` or `"skip"`.


## Ownership or permission errors on NAS / SMB / Android

Errors such as:

```
tar: Cannot change ownership ... Function not implemented
```

mean the destination filesystem does not support `chown` or POSIX permissions.

Set the following in your config:

```toml
preserve_owner = false
preserve_permissions = false
```

This avoids ownership and permission operations entirely.

## SSH connections drop during sync

If transfers fail with `Broken pipe` or similar errors:

* Configure SSH keep-alives (`ServerAliveInterval` / `ClientAliveInterval`)
* Prevent the system from sleeping during long syncs
* Reduce the amount of data by tightening `include` patterns

On Android, keeping Termux awake during syncs is often necessary.

## Include or ignore changes behave unexpectedly

If you tighten `include` or add `ignore` patterns, previously tracked paths may still exist in the state database, but they are treated as out of scope. Synchi will not scan or delete them.

If you want to completely reset the tracked scope:

* Delete `.synchi/<state_db_name>.db` (default `state.db`) and run `synchi init` to rebuild state for the new scope.

## Diagnostic tips

* Use `-v` for more detailed output.
* `synchi status` shows exactly what `synchi sync` would do.
* Sync reports are printed to stdout after each run. Synchi does not persist a journal log file.

## Still stuck?

If none of the above helps, open a [GitHub issue](https://github.com/jakobkreft/synchi/issues) and include:

1. Your config file (with personal info redacted)
2. Command output with `-v`
3. Steps to recreate
4. Synchi version and platform

## Further reading

* [Configuration Reference](./configuration.md)
* [Android (Termux)](./termux.md)
* [Why Synchi?](./why.md)
