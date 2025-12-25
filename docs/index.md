---
layout: default
title: Synchi
---

> **Note:** Synchi is under active development and currently in **alpha**.  
> Expect breaking changes as the interface and behavior settle.

Synchi is a tool for syncing files between two locations.  
You run it when you want, it figures out what changed since last time, and it asks you what to do when there are conflicts.

It works with local folders or over SSH, and it does not require any agent or service on the remote side.

If you’re curious *why* Synchi exists in the first place, see [Why Synchi?](./why.md).

## What Synchi is good at

- Detecting real changes between runs, not just timestamp noise
- Two-way sync with explicit conflict handling
- One-way mirroring when you want it
- Working over plain SSH or local paths
- Staying predictable across repeated runs

Synchi is designed to be run on demand.  
It does not watch files, run in the background, or try to hide what it’s doing. You are in control of all changes that are made.

## How it works

At a high level, Synchi does the following:

1. Scans both roots while applying include and ignore rules (include defines the sync scope)  
2. Detects changes since the previous run  
3. Classifies files as new, modified, deleted, or conflicting  
4. Plans the required operations  
5. Executes them safely, transferring only what actually changed  

Because Synchi keeps state between runs, repeated executions are fast and stable.

## Quick start

```bash
# Install Synchi (see installation docs for all options)
synchi --help

# Create your own config
# By default Synchi reads: ~/.config/synchi/config.toml
root_a = "./root_a"
root_b = "ssh://user@host/srv/data"
include = ["**"]
ignore = ["**/.venv/**"]
# Read configuration instuctions for other options.

# Remote roots must use ssh://user@host/path.
# scp-style user@host:/path is not supported.

# Run
synchi sync
```

## Requirements

* Root A must be local and writable (it stores Synchi’s state)
* Remote roots are accessed over SSH
* Standard tools must be available: `ssh`, `tar`, `find`, `sha256sum`

Most Linux systems already meet these requirements.

## Documentation

* [Installation Guide](./installation.md)
* [Configuration Reference](./configuration.md)
* [Troubleshooting & FAQ](./troubleshooting.md)
* [Why Synchi?](./why.md)
* [GitHub Repository](https://github.com/jakobkreft/synchi)

If you run into issues or have questions, open a GitHub issue with details and logs.
