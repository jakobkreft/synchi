# Changelog

All notable changes to this project will be documented here.

## 0.1.1

- Two-way sync with conflict detection and interactive resolution.
- Local and SSH roots with batched transfers.
- Hardlink handling modes: copy, skip, preserve.
- Category policies with restore for delete actions.

## 0.1.2

- Eliminated Status/Sync code duplication by extracting shared pipeline.
- Removed dead code: `Plan::conflicts` field, `Root::normalize_path()`, incorrect dead_code annotations on `get_entry`/`upsert_entry`.
- Test-only functions (`prepare_hashes`, `open_memory`, `get_meta`, `set_meta`) now properly gated behind `#[cfg(test)]`.
- Fixed all clippy warnings (items_after_test_module, module_inception).
- Conflict resolution TUI now shows human-readable timestamps, file sizes, short hashes, and permission-only mode bits.
- `synchi status` and `--dry-run` now list conflicting file paths and reasons.
- Added `--version` integration test.
- Cleaned up comments across the codebase.
- Fixed doc formatting: broken code fences in installation.md, configuration.md, termux.md; typo in index.md.
- Added workflow diagram and terminal demo to docs and README.

## 0.1.3

- Published to [crates.io](https://crates.io/crates/synchi): `cargo install synchi` is now the recommended install method.
- Added release builds for Linux ARM64 (Raspberry Pi, ARM servers) and macOS x86_64 (Intel Macs).
- Updated installation docs to reflect crates.io and all four platform binaries.

## 0.1.4

- Fixed macOS x86_64 release build (cross-compile from ARM64 runner, replacing deprecated macos-13).
- Updated GitHub Actions to actions/checkout@v6 (Node.js 24, fixes deprecation warnings).
- Added CI and crates.io badges to README.
- Added GoatCounter analytics to docs site.
