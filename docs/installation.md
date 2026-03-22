---
layout: default
title: Installation
---

# Installation

## Option 1: Install via Cargo (Recommended)

If you have Rust installed, you can install Synchi directly from [crates.io](https://crates.io/crates/synchi):

```bash
cargo install synchi
```

To install Rust, follow the instructions at [rustup.rs](https://rustup.rs/).

## Option 2: Download a Precompiled Binary

Go to the GitHub releases page and download the binary for your platform:

- [https://github.com/jakobkreft/synchi/releases/](https://github.com/jakobkreft/synchi/releases/)

Available binaries:

- `synchi-linux-x86_64` — Linux (x86_64)
- `synchi-linux-aarch64` — Linux (ARM64, Raspberry Pi, ARM servers)
- `synchi-macos-aarch64` — macOS (Apple Silicon)
- `synchi-macos-x86_64` — macOS (Intel)

Make the binary executable and place it somewhere on your `PATH`:

```bash
chmod +x synchi-linux-x86_64
mv synchi-linux-x86_64 /usr/local/bin/synchi
```

## Option 3: Build from Source

Clone and build Synchi:

```bash
git clone https://github.com/jakobkreft/synchi.git
cd synchi
cargo build --release
```

The compiled binary will be available at `target/release/synchi`. You can copy it to a directory on your `PATH`, or install directly:

```bash
cargo install --path .
```

## Android (Termux)

Synchi can be built from source inside [Termux](https://termux.dev/):

```bash
pkg install rust git
cargo install synchi
```

See [Android (Termux)](./termux.md) for the full setup guide including SSH configuration.

## System Requirements

Synchi relies on a few standard Unix tools on both the local machine and any remote hosts:

* `ssh`
* `tar`
* `find` (with `-printf` support)
* `sha256sum`

Most Linux systems already include them.

## Quick Sanity Check

```bash
synchi --version
synchi --help
```

## Next Steps

* [Configuration Reference](./configuration.md)
* [Android (Termux)](./termux.md) for syncing with Android devices
* [Troubleshooting](./troubleshooting.md) if something doesn't work
