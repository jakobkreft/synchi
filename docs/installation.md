---
layout: default
title: Installation
---

# Installation

Synchi is available as a precompiled binary on GitHub Releases.  
If you prefer, you can also build it yourself from source.

## Option 1: Download a Precompiled Binary (Recommended)

Go to the GitHub releases page and download the binary for your platform:

- [https://github.com/jakobkreft/synchi/releases/](https://github.com/jakobkreft/synchi/releases/)

Extract the archive and make the binary executable, then place it somewhere on your `PATH`, for example:

```bash
chmod +x synchi
mv synchi /usr/local/bin/
```

Verify the installation:

```bash
synchi --help
```

## Option 2: Build from Source

Building from source requires Rust.
Install Rust by following the instructions at:

* [https://rustup.rs/](https://rustup.rs/)

Then clone and build Synchi:

```bash
git clone https://github.com/jakobkreft/synchi.git
cd synchi
cargo build --release
```

The compiled binary will be available at:

```
target/release/synchi
```

You can copy it to a directory on your `PATH`, or install it via Cargo:

```bash
cargo install --path .
```

## System Requirements

Synchi relies on a few standard Unix tools.

On the machine running Synchi:

* `ssh`
* `tar`
* `find` (with `-printf` support)
* `sha256sum`

On remote systems (if using SSH sync), the same tools must be available.
Most Linux systems already include them.

## Quick Sanity Check

To confirm Synchi runs correctly:

```bash
synchi --version
synchi --help
```

For a full setup, continue with the configuration guide.

## Next Steps

* [Configuration Reference](./configuration.md)
* [Android (Termux)](./termux.md) for syncing with Android devices
* [Troubleshooting](./troubleshooting.md) if something doesn’t work