---
layout: default
title: Why Synchi?
---

# Why Synchi?

There are already many tools for syncing files.  
Synchi exists because none of them quite fit the workflow I wanted.

This page is not meant to rank tools or claim that one is universally better than another. Each of these tools solves a different problem well. Synchi is just focused on a narrower set of trade-offs.

## The problem space

What I wanted was:

- A deterministic, on-demand sync
- Two-way syncing with explicit conflict handling
- No background daemons
- No requirement to install software on both ends
- Stable behavior across repeated runs

In other words: run a command, see what changed, decide what to do, and be done.

## rsync

`rsync` is the classic tool for file transfer, and it’s extremely good at what it does.

**Strengths**
- Ubiquitous and fast
- Works over SSH
- Very reliable for one-way mirroring and backups

**Limitations**
- No persistent state between runs
- Primarily one-directional
- No real concept of conflicts

Because rsync does not remember previous runs, every execution starts from scratch. For large trees, this means repeated scanning and comparison, and for bidirectional use cases you quickly end up scripting logic on top of it.

rsync is excellent for backups and deployments. It’s less comfortable as a two-way synchronizer.

## Unison

Unison is one of the few tools that actually supports true two-way synchronization with state tracking.

**Strengths**
- Bidirectional sync with conflict detection
- Persistent state
- Mature and battle-tested

**Limitations**
- Must be installed on both ends
- Sensitive to metadata changes
- Can feel noisy on systems where mtimes change frequently

In practice, Unison’s metadata-based change detection can lead to repeated reprocessing of files that have not actually changed in content. On systems where background processes touch files or update timestamps, this can make sequential runs feel “never finished”.

Unison works well in controlled environments, but it was not a great fit for mixed systems or remote machines where installing matching versions is inconvenient.

## Syncthing

Syncthing is a very different class of tool.

**Strengths**
- Continuous, real-time syncing
- Automatic peer discovery
- Strong cryptographic guarantees

**Limitations**
- Requires running daemons on all devices
- Overkill for simple or occasional sync
- Not well suited for ad-hoc local or mounted paths

Syncthing shines when you want always-on synchronization between multiple devices. It is less suitable when you want to sync a directory on demand, or when the “other side” is just a mounted filesystem, NAS, or remote server accessed via SSH.

## Where Synchi fits

Synchi sits somewhere between these tools.

It is:

- Run on demand, not always running
- Stateful, so it knows what changed since last time
- Content-aware, using hashes to avoid false positives
- Usable over SSH without installing anything remotely
- Explicit about conflicts instead of hiding them

Synchi does not try to be real-time, automatic, or invisible. It assumes you want to stay in control, see what will happen, and make decisions when conflicts arise.

## When Synchi makes sense

Synchi is a good fit if you:

- Sync between a Linux machine and servers or NAS over SSH
- Want two-way sync without daemons
- Care about repeated runs being stable and predictable
- Prefer explicit conflict resolution over silent overwrites

It is not meant to replace rsync for backups or Syncthing for live device syncing.

## Closing thoughts

Most file sync tools are optimized for automation.  
Synchi is optimized for clarity.

That trade-off won’t appeal to everyone, but it’s the reason the tool exists.

## Further reading

* [Installation Guide](./installation.md)
* [Configuration Reference](./configuration.md)
* [Android (Termux)](./termux.md)
