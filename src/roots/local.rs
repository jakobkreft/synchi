use super::{Root, RootMetadata, RootType};
use anyhow::{Context, Result};
use std::fs::{self, File, OpenOptions};
use std::io::{self, Read, Write};
use std::os::unix::fs::{symlink, PermissionsExt};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::SystemTime;
use tempfile::NamedTempFile;

#[derive(Clone)]
pub struct LocalRoot {
    root_path: PathBuf,
}

impl LocalRoot {
    pub fn new(path: impl AsRef<Path>) -> Result<Self> {
        let expanded = expand_tilde(path.as_ref())?;
        let root_path =
            fs::canonicalize(&expanded).context("Failed to canonicalize local root path")?;
        Ok(Self { root_path })
    }

    fn resolve(&self, path: &Path) -> Result<PathBuf> {
        let joined = self.root_path.join(path);
        Ok(joined)
    }

    fn to_root_meta(meta: std::fs::Metadata) -> RootMetadata {
        RootMetadata {
            mtime: meta.modified().unwrap_or(SystemTime::UNIX_EPOCH),
            mode: meta.permissions().mode(),
        }
    }
}

fn expand_tilde(path: &Path) -> Result<PathBuf> {
    let raw = path.to_string_lossy();
    if raw == "~" {
        let home = dirs::home_dir().context("Failed to resolve ~ (home directory not set)")?;
        return Ok(home);
    }
    if let Some(rest) = raw.strip_prefix("~/") {
        let home = dirs::home_dir().context("Failed to resolve ~ (home directory not set)")?;
        return Ok(home.join(rest));
    }
    if raw.starts_with('~') {
        anyhow::bail!("Unsupported home expansion: {}", raw);
    }
    Ok(path.to_path_buf())
}

impl Root for LocalRoot {
    fn kind(&self) -> RootType {
        RootType::Local
    }

    fn path(&self) -> &Path {
        &self.root_path
    }

    fn lstat(&self, path: &Path) -> Result<RootMetadata> {
        let abs_path = self.resolve(path)?;
        let meta = fs::symlink_metadata(abs_path)?;
        Ok(Self::to_root_meta(meta))
    }

    fn try_lock(&self, path: &Path, info: &str) -> Result<()> {
        let abs_path = self.resolve(path)?;
        if let Some(parent) = abs_path.parent() {
            fs::create_dir_all(parent)?;
        }
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&abs_path)
            .context("Failed to acquire lock (file exists)")?;
        file.write_all(info.as_bytes())?;
        Ok(())
    }

    fn unlock(&self, path: &Path) -> Result<()> {
        let abs_path = self.resolve(path)?;
        if abs_path.exists() {
            fs::remove_file(abs_path).context("Failed to remove lock content")?;
        }
        Ok(())
    }

    fn open_read(&self, path: &Path) -> Result<Box<dyn Read + Send>> {
        let abs_path = self.resolve(path)?;
        let file = File::open(abs_path)?;
        Ok(Box::new(file))
    }

    fn write_file(&self, path: &Path, content: &mut dyn Read) -> Result<()> {
        let abs_path = self.resolve(path)?;
        if let Some(parent) = abs_path.parent() {
            fs::create_dir_all(parent)?;
        }

        let parent_dir = abs_path
            .parent()
            .context("Failed to resolve parent directory for write")?;
        let mut temp_file = NamedTempFile::new_in(parent_dir)?;

        io::copy(content, &mut temp_file)?;
        match temp_file.persist(&abs_path) {
            Ok(_) => Ok(()),
            Err(err) => {
                if err.error.kind() == io::ErrorKind::AlreadyExists {
                    fs::remove_file(&abs_path)?;
                    err.file.persist(&abs_path)?;
                    Ok(())
                } else {
                    Err(err.error.into())
                }
            }
        }
    }

    fn create_symlink(&self, target: &str, path: &Path) -> Result<()> {
        let abs_path = self.resolve(path)?;
        if let Some(parent) = abs_path.parent() {
            fs::create_dir_all(parent)?;
        }
        if let Err(err) = fs::remove_file(&abs_path) {
            if err.kind() != io::ErrorKind::NotFound {
                return Err(err.into());
            }
        }
        symlink(target, abs_path).map_err(Into::into)
    }

    fn set_meta(&self, path: &Path, mode: u32, mtime: SystemTime) -> Result<()> {
        let abs_path = self.resolve(path)?;

        let mut perms = fs::metadata(&abs_path)?.permissions();
        perms.set_mode(mode);
        fs::set_permissions(&abs_path, perms)?;

        let mtime_filetime = filetime::FileTime::from_system_time(mtime);
        filetime::set_symlink_file_times(&abs_path, mtime_filetime, mtime_filetime)?;

        Ok(())
    }

    fn mkdirs(&self, path: &Path) -> Result<()> {
        let abs_path = self.resolve(path)?;
        fs::create_dir_all(abs_path).map_err(Into::into)
    }

    fn remove_file(&self, path: &Path) -> Result<()> {
        let abs_path = self.resolve(path)?;
        fs::remove_file(abs_path).map_err(Into::into)
    }

    fn remove_dir(&self, path: &Path) -> Result<()> {
        let abs_path = self.resolve(path)?;
        fs::remove_dir(abs_path).map_err(Into::into)
    }

    fn exec(&self, cmd: &str) -> Result<(Vec<u8>, Vec<u8>, i32)> {
        let output = Command::new("sh")
            .arg("-c")
            .arg(cmd)
            .current_dir(&self.root_path)
            .output()
            .context("Failed to exec local command")?;

        Ok((
            output.stdout,
            output.stderr,
            output.status.code().unwrap_or(-1),
        ))
    }

    fn as_any(&self) -> &dyn std::any::Any {
        self
    }

    fn box_clone(&self) -> Box<dyn Root> {
        Box::new(self.clone())
    }

    fn hash_files(&self, paths: &[PathBuf]) -> Result<Vec<String>> {
        use sha2::Digest;
        let mut hashes = Vec::with_capacity(paths.len());
        for path in paths {
            let p = self.resolve(path)?;
            let mut file = fs::File::open(p)?;
            let mut hasher = sha2::Sha256::new();
            io::copy(&mut file, &mut hasher)?;
            let hash = format!("{:x}", hasher.finalize());
            hashes.push(hash);
        }
        Ok(hashes)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use tempfile::TempDir;

    #[test]
    fn test_local_hash_files() -> Result<()> {
        let tmp = TempDir::new()?;
        let root = LocalRoot::new(tmp.path())?;

        let p1 = tmp.path().join("f1");
        let p2 = tmp.path().join("f2");

        {
            let mut f1 = fs::File::create(&p1)?;
            f1.write_all(b"hello")?;
            let mut f2 = fs::File::create(&p2)?;
            f2.write_all(b"world")?;
        }

        let paths = vec![PathBuf::from("f1"), PathBuf::from("f2")];
        let hashes = root.hash_files(&paths)?;

        assert_eq!(hashes.len(), 2);
        // echo -n "hello" | sha256sum
        // 2cf24dba5fb0a30e26e83b2ac5b9e29e1b161e5c1fa7425e73043362938b9824
        assert_eq!(
            hashes[0],
            "2cf24dba5fb0a30e26e83b2ac5b9e29e1b161e5c1fa7425e73043362938b9824"
        );
        // echo -n "world" | sha256sum
        // 486ea46224d1bb4fb680f34f7c9ad96a8f24ec88be73ea8e5a6c65260e9cb8a7
        assert_eq!(
            hashes[1],
            "486ea46224d1bb4fb680f34f7c9ad96a8f24ec88be73ea8e5a6c65260e9cb8a7"
        );

        Ok(())
    }
}
