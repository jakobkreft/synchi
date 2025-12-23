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
2. Remove the lock file manually:

```bash
   rm /path/to/root_a/.synchi/state.lock
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

If you tighten `include`, files that were previously tracked may show up as deletions. This is expected: the state database still remembers them.

You can either:

* Let Synchi apply the deletions, or
* Delete `.synchi/<state_db_name>.db` (default `state.db`) and run `synchi init` to reset the state

## Diagnostic tips

* Use `-v` for more detailed output.
* `synchi status` shows exactly what `synchi sync` would do.
* Check `.synchi/journal.log` (if present) for recent activity.

## Still stuck?

If none of the above helps, open a GitHub issue and include:

1. Your config file (with personal info redacted)
2. Command output with `-v`
3. Steps to recreate
3. Synchi version and platform
