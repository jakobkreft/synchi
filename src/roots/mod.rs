use anyhow::Result;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

mod local;
mod ssh;

pub use local::LocalRoot;
pub use ssh::SshRoot;

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum RootType {
    Local,
    Ssh,
}

pub fn parse_root_type(spec: &str) -> Result<RootType> {
    let trimmed = spec.trim();
    if trimmed.is_empty() {
        anyhow::bail!("Root spec cannot be empty");
    }

    if trimmed.starts_with("ssh://") {
        return Ok(RootType::Ssh);
    }

    if trimmed.starts_with('/') || trimmed.starts_with("./") || trimmed.starts_with("../") {
        return Ok(RootType::Local);
    }

    if trimmed.starts_with("~") {
        return Ok(RootType::Local);
    }

    if let Some((left, right)) = trimmed.split_once(':') {
        let left = left.trim();
        let right = right.trim();
        if left.is_empty() || right.is_empty() {
            anyhow::bail!("Invalid root spec: {}", spec);
        }

        if left.contains('/') || left.contains('\\') {
            return Ok(RootType::Local);
        }

        if left.len() == 1 && right.starts_with('\\') {
            return Ok(RootType::Local);
        }

        anyhow::bail!(
            "SSH roots must use ssh://user@host/path. scp-style '{}' is not supported.",
            spec
        );
    }

    Ok(RootType::Local)
}

pub fn root_from_spec(spec: &str) -> Result<Box<dyn Root>> {
    match parse_root_type(spec)? {
        RootType::Local => Ok(Box::new(LocalRoot::new(spec)?)),
        RootType::Ssh => Ok(Box::new(SshRoot::new(spec)?)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_root_type_prefers_explicit_ssh() {
        assert_eq!(parse_root_type("ssh://host/path").unwrap(), RootType::Ssh);
    }

    #[test]
    fn parse_root_type_accepts_local_hints() {
        assert_eq!(parse_root_type("./dir:sub").unwrap(), RootType::Local);
        assert_eq!(parse_root_type("/abs:dir").unwrap(), RootType::Local);
        assert_eq!(parse_root_type("dir/sub:thing").unwrap(), RootType::Local);
        assert_eq!(parse_root_type("~/data").unwrap(), RootType::Local);
    }

    #[test]
    fn parse_root_type_rejects_scp_style() {
        let err = parse_root_type("user@host:/data").unwrap_err();
        assert!(
            err.to_string().contains("ssh://"),
            "unexpected error: {err}"
        );
    }
}

#[derive(Debug, Clone, Default)]
pub struct RemoteCaps {
    pub has_find_printf: bool,
}

#[derive(Debug, Clone)]
pub struct RootMetadata {
    pub mtime: SystemTime,
    pub mode: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EntryKind {
    File,
    Dir,
    Symlink, // and Other?
}

pub trait Root: Send + Sync {
    /// Return the type of root (Local/Ssh)
    fn kind(&self) -> RootType;

    /// Return the absolute path of the root
    fn path(&self) -> &Path;

    /// Normalize a path relative to the root (ensure it starts with ./ or is relative)
    /// and check bounds.
    fn normalize_path(&self, path: &Path) -> Result<PathBuf> {
        Ok(path.to_path_buf())
    }

    /// Get metadata for path (lstat - does not follow symlinks)
    fn lstat(&self, path: &Path) -> Result<RootMetadata>;

    /// Try to acquire a lock with the given info content (e.g. PID/Host).
    /// Returns Ok(()) if acquired, Err if already locked (or other error).
    fn try_lock(&self, path: &Path, info: &str) -> Result<()>;

    /// Release lock.
    fn unlock(&self, path: &Path) -> Result<()>;

    /// Open file for reading
    fn open_read(&self, path: &Path) -> Result<Box<dyn Read + Send>>;

    /// Write file atomically (write to temp then rename)
    /// content is a reader
    fn write_file(&self, path: &Path, content: &mut dyn Read) -> Result<()>;

    /// Set metadata (mode, mtime)
    fn set_meta(&self, path: &Path, mode: u32, mtime: SystemTime) -> Result<()>;

    /// Create a symlink at path pointing to target
    fn create_symlink(&self, target: &str, path: &Path) -> Result<()>;

    /// Create directory (mkdir -p behavior preferred or one level?)
    /// Blueprint says `mkdirs`.
    fn mkdirs(&self, path: &Path) -> Result<()>;

    /// Remove file
    fn remove_file(&self, path: &Path) -> Result<()>;

    /// Remove directory (must be empty? or recursive? standard fs::remove_dir usually empty)
    fn remove_dir(&self, path: &Path) -> Result<()>;

    /// Execute a command (SSH only)
    fn exec(&self, cmd: &str) -> Result<(Vec<u8>, Vec<u8>, i32)>;

    /// Compute hashes for a list of files (batched)
    /// Returns a map or list of hashes corresponding to paths.
    /// Order should match validation or use return type `Vec<(PathBuf, String)>`?
    /// Let's return `Vec<String>` in same order, or `Result<Vec<String>>`.
    /// If one fails, we usually fail the batch? Or return Option?
    /// Simplest: `Vec<Option<String>>`?
    /// For sync, if hashing fails (file gone), we might want to know.
    /// Let's return `Result<Vec<String>>` assuming all exist.
    fn hash_files(&self, _paths: &[PathBuf]) -> Result<Vec<String>> {
        anyhow::bail!("Hash files not implemented for this root");
    }

    /// Downcast helper
    fn as_any(&self) -> &dyn std::any::Any;

    /// Clone boxed root for long-lived tasks (locks/signal cleanup)
    fn box_clone(&self) -> Box<dyn Root>;
}
