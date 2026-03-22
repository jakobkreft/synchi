---
layout: default
title: Android (Termux)
---

# Android (Termux)

Synchi can sync files between a Linux machine and an Android device using [Termux](https://termux.dev/), a terminal emulator that provides a Linux environment on your phone.

The sync is bidirectional, the only question is where you run Synchi from:

- **Run Synchi from Linux** (recommended): Your Linux machine runs Synchi and connects to Android over SSH. **Synchi does not need to be installed on Android.** You only need Termux with SSH and storage access. Follow the [Termux SSH setup](#termux-ssh-setup) below, then see the [example configuration](#run-from-linux).
- **Run Synchi from Android**: Synchi runs inside Termux and connects to your Linux machine over SSH. This requires [building Synchi from source on the phone](#run-from-android).

---

## Termux SSH setup

Regardless of where you run Synchi, the Android device needs Termux with SSH and storage access. All of the following steps are done inside Termux on the Android device.

### Install OpenSSH

```bash
pkg install openssh
```

### Grant storage access

To access Android's shared storage (photos, downloads, etc.):

```bash
termux-setup-storage
```

This creates `~/storage/shared` which maps to Android's internal storage (`/sdcard`).

### Start the SSH server

```bash
sshd
```

Termux runs sshd on **port 8022** (not 22), because Android restricts ports below 1024.

### Authentication

Set up key-based authentication or password authentication:

```bash
# On your Android device (Termux):
mkdir -p ~/.ssh

# On your Linux machine, copy your public key:
ssh-copy-id -p 8022 USERNAME@ANDROID_IP
```

Or manually paste your public key into `~/.ssh/authorized_keys` on the Termux side.

If you prefer password auth for initial setup:

```bash
passwd
```

This sets a Termux-specific password. Use it with `ssh -p 8022 USERNAME@ANDROID_IP`.

### Find your username and IP

```bash
whoami
```

Termux usernames look like `u0_a123` — this is normal on Android.

```bash
ifconfig
```

Look for the `wlan0 inet` address (e.g., `192.168.1.105`). This is your phone's local IP.

Test the connection from your Linux machine:

```bash
ssh -p 8022 u0_a123@192.168.1.105
```

### Networking options

**Same Wi-Fi network**: Use the IP from `ifconfig`. Simple, but the IP may change.

**Tailscale** (recommended for reliability): Install Tailscale on both devices. It assigns stable IPs that work across networks.

- Android: Install from Play Store
- Linux: [tailscale.com/download](https://tailscale.com/download)

With Tailscale, use the Tailscale IP instead of the local one. No port forwarding or firewall configuration needed.

---

## Run from Linux

Synchi runs on your Linux machine and connects to the Android device over SSH. **Nothing needs to be installed on Android besides Termux with SSH.**

Example configuration (on your Linux machine):

```toml
root_a = "/home/user/photos"
root_b = "ssh://u0_a123@192.168.1.105:8022/data/data/com.termux/files/home/storage/shared/DCIM"

include = ["**"]
ignore = ["**/cache/**", "**/.thumbnails/**"]

preserve_owner = false
preserve_permissions = false
```

```bash
synchi sync
```

---

## Run from Android

If you want to run Synchi from your phone, you need to build it from source inside Termux.

### Install Synchi inside Termux

```bash
pkg install rust git openssh
cargo install synchi
```

Add the cargo bin directory to your PATH:

```bash
echo 'export PATH="$PATH:$HOME/.cargo/bin"' >> ~/.bashrc
source ~/.bashrc
```

Verify:

```bash
synchi --version
```

> **Note:** The Linux ARM64 binary from GitHub Releases does not work in Termux (different C library). You must build from source.

### Example configuration

```toml
root_a = "/data/data/com.termux/files/home/storage/shared/Documents"
root_b = "ssh://user@192.168.1.100/home/user/Documents"

include = ["**"]

preserve_owner = false
preserve_permissions = false
```

Root A must be local, so when running from Termux, the Android path is root A.

---

## Android-specific notes

- **Disable ownership and permissions**: Android storage does not support POSIX ownership or permissions. Always set `preserve_owner = false` and `preserve_permissions = false`.
- **Keep Termux awake**: Long syncs may fail if Android suspends Termux. Run `termux-wake-lock` before syncing, or acquire a wake lock in Termux settings.
- **Port 8022**: Termux sshd uses port 8022 by default. Include `:8022` in your SSH root spec.

## Further reading

- [Installation Guide](./installation.md)
- [Configuration Reference](./configuration.md)
- [Troubleshooting](./troubleshooting.md)
