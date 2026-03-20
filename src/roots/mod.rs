use anyhow::{Context, Result};
use std::io::Read;
use std::path::{Path, PathBuf};
use std::time::SystemTime;
use url::Url;

mod local;
mod ssh;

pub use local::LocalRoot;
pub use ssh::SshRoot;

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum RootType {
    Local,
    Ssh,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RootSpec {
    Local {
        path: PathBuf,
    },
    Ssh {
        user: Option<String>,
        host: String,
        port: Option<u16>,
        path: PathBuf,
    },
}

impl RootSpec {
    pub fn parse(spec: &str) -> Result<Self> {
        let trimmed = spec.trim();
        if trimmed.is_empty() {
            anyhow::bail!("Root spec cannot be empty");
        }

        if trimmed.starts_with("ssh://") {
            let url = Url::parse(trimmed).with_context(|| {
                format!("Invalid SSH spec '{}', use ssh://user@host/path", spec)
            })?;
            if url.scheme() != "ssh" {
                anyhow::bail!("Invalid SSH scheme: {}", url.scheme());
            }
            let host = url
                .host_str()
                .context("Missing host in SSH spec")?
                .to_string();
            let user = if url.username().is_empty() {
                None
            } else {
                Some(url.username().to_string())
            };
            let path_str = url.path();
            let path = if path_str.is_empty() {
                PathBuf::from("/")
            } else {
                PathBuf::from(path_str)
            };
            return Ok(RootSpec::Ssh {
                user,
                host,
                port: url.port(),
                path,
            });
        }

        if looks_like_scp(trimmed) {
            anyhow::bail!(
                "SSH roots must use ssh://user@host/path. scp-style '{}' is not supported.",
                spec
            );
        }

        Ok(RootSpec::Local {
            path: PathBuf::from(trimmed),
        })
    }

    pub fn is_local(&self) -> bool {
        matches!(self, RootSpec::Local { .. })
    }

    pub fn local_path(&self) -> Option<&Path> {
        match self {
            RootSpec::Local { path } => Some(path.as_path()),
            _ => None,
        }
    }

    pub fn display(&self) -> String {
        match self {
            RootSpec::Local { path } => path.to_string_lossy().to_string(),
            RootSpec::Ssh {
                user,
                host,
                port,
                path,
            } => {
                let user_part = user.as_ref().map(|u| format!("{u}@")).unwrap_or_default();
                let port_part = port.map(|p| format!(":{p}")).unwrap_or_default();
                let path_str = path.to_string_lossy();
                format!("ssh://{user_part}{host}{port_part}{path_str}")
            }
        }
    }

    pub fn root(&self) -> Result<Box<dyn Root>> {
        match self {
            RootSpec::Local { path } => Ok(Box::new(LocalRoot::new(path)?)),
            RootSpec::Ssh {
                user,
                host,
                port,
                path,
            } => Ok(Box::new(SshRoot::from_parts(
                user.clone(),
                host.clone(),
                *port,
                path.clone(),
            )?)),
        }
    }
}

fn looks_like_scp(spec: &str) -> bool {
    if let Some((left, right)) = spec.split_once(':') {
        if left.contains('/') || left.contains('\\') {
            return false;
        }
        if left.len() == 1 && right.starts_with('\\') {
            return false;
        }
        return left.contains('@');
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_root_spec_prefers_explicit_ssh() {
        let spec = RootSpec::parse("ssh://user@host/path").unwrap();
        assert!(matches!(spec, RootSpec::Ssh { .. }));
    }

    #[test]
    fn parse_root_spec_accepts_local() {
        assert!(matches!(
            RootSpec::parse("./dir:sub").unwrap(),
            RootSpec::Local { .. }
        ));
        assert!(matches!(
            RootSpec::parse("/abs:dir").unwrap(),
            RootSpec::Local { .. }
        ));
        assert!(matches!(
            RootSpec::parse("dir/sub:thing").unwrap(),
            RootSpec::Local { .. }
        ));
        assert!(matches!(
            RootSpec::parse("~/data").unwrap(),
            RootSpec::Local { .. }
        ));
    }

    #[test]
    fn parse_root_spec_rejects_scp_style() {
        let err = RootSpec::parse("user@host:/data").unwrap_err();
        assert!(
            err.to_string().contains("ssh://"),
            "unexpected error: {err}"
        );
    }
}

#[derive(Debug, Clone, Default)]
pub struct RemoteCaps {
    pub has_find_printf: bool,
    pub has_find_inode: bool,
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
    Symlink,
}

pub trait Root: Send + Sync {
    /// Return the type of root (Local/Ssh)
    fn kind(&self) -> RootType;

    /// Return the absolute path of the root
    fn path(&self) -> &Path;

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

    /// Create directories recursively (mkdir -p).
    fn mkdirs(&self, path: &Path) -> Result<()>;

    /// Remove file
    fn remove_file(&self, path: &Path) -> Result<()>;

    /// Remove an empty directory.
    fn remove_dir(&self, path: &Path) -> Result<()>;

    /// Execute a command (SSH only)
    fn exec(&self, cmd: &str) -> Result<(Vec<u8>, Vec<u8>, i32)>;

    /// Compute SHA-256 hashes for a batch of files. Returns hex strings in the same order as paths.
    fn hash_files(&self, _paths: &[PathBuf]) -> Result<Vec<String>> {
        anyhow::bail!("Hash files not implemented for this root");
    }

    /// Downcast helper
    fn as_any(&self) -> &dyn std::any::Any;

    /// Clone boxed root for long-lived tasks (locks/signal cleanup)
    fn box_clone(&self) -> Box<dyn Root>;
}
