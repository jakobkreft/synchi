---
layout: default
title: Android (Termux)
---

# Android (Termux)

Synchi can also be used with Android through [Termux](https://termux.dev/).  
This makes it possible to sync files such as photos or project directories between an Android device and a Linux machine.

In practice, Synchi is usually run on the Linux side, with the Android device as "root B" over SSH. Running Synchi directly inside Termux may also work, but is not covered here.

## Accessing Android Storage

To allow Synchi (or SSH access from another machine) to see your Android files, Termux needs permission to access shared storage:

```bash
termux-setup-storage
````

This exposes Android’s shared storage under:

```
$HOME/storage/shared
```

## Example Configuration

Below is a minimal example showing a Linux and Android device sync over SSH. 

```toml
# Android shared storage via Termux SSH
root_a = "./local_files"
root_b = "ssh://user@android-device:8022/home/user/storage/shared/files"

include = ["**"]
ignore = ["**/cache/**"]

hash_mode = "balanced"
force = "none"

skip_hardlinks = true
preserve_owner = false
preserve_permissions = false
```

Android filesystems typically do not support POSIX ownership or permissions, so disabling ownership and permission preservation is recommended.

## Notes

* Android storage behaves more like FAT than a traditional Linux filesystem.
* SSH access is provided by Termux’s OpenSSH package.
* Large syncs may require keeping the device awake to avoid suspended connections.

## Further Reading

- [Installation Guide](./installation.md) – how to install Synchi on Linux.
- [Configuration Reference](./configuration.md) – full list of configuration options.
- [Troubleshooting](./troubleshooting.md) – common issues.
