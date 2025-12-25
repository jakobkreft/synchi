use super::{RemoteCaps, Root, RootMetadata, RootType};
use crate::shell::{shell_quote, shell_quote_path};
use anyhow::{bail, Context, Result};
use std::io::{self, Read};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use url::Url;

#[derive(Clone)]
pub struct SshRoot {
    user_host: String, // user@host or just host
    port: Option<u16>,
    root_path: PathBuf,
}

impl SshRoot {
    pub fn new(spec: &str) -> Result<Self> {
        if let Ok(url) = Url::parse(spec) {
            return Self::from_url(url);
        }
        Self::from_scp_like(spec)
    }

    #[cfg(test)]
    pub fn user_host(&self) -> &str {
        &self.user_host
    }

    #[cfg(test)]
    pub fn port(&self) -> Option<u16> {
        self.port
    }

    fn from_url(url: Url) -> Result<Self> {
        if url.scheme() != "ssh" {
            bail!("Invalid scheme for SSH root, expected 'ssh': {}", url);
        }

        let host = url.host_str().context("Missing host in SSH URI")?;
        let user = url.username();
        let user_host = if !user.is_empty() {
            format!("{}@{}", user, host)
        } else {
            host.to_string()
        };

        let path_str = url.path();
        let root_path = PathBuf::from(if path_str.is_empty() { "/" } else { path_str });

        Ok(Self {
            user_host,
            port: url.port(),
            root_path,
        })
    }

    fn from_scp_like(spec: &str) -> Result<Self> {
        // Format: [user@]host:path
        let (user_host_part, path_part) = spec
            .split_once(':')
            .context("Invalid SSH target. Use user@host:/path or ssh://user@host/path")?;

        if user_host_part.trim().is_empty() {
            bail!("Missing host in SSH target: {}", spec);
        }

        let user_host = user_host_part.trim().to_string();
        let path = path_part.trim();
        let root_path = if path.is_empty() {
            PathBuf::from(".")
        } else {
            PathBuf::from(path)
        };

        Ok(Self {
            user_host,
            port: None,
            root_path,
        })
    }

    pub(crate) fn ssh_command(&self) -> Command {
        let mut cmd = Command::new("ssh");
        // Ensure batch mode to avoid interactive prompts hanging
        cmd.arg("-T").arg("-o").arg("BatchMode=yes");
        if let Some(p) = self.port {
            cmd.arg("-p").arg(p.to_string());
        }
        cmd.arg(&self.user_host);
        cmd
    }

    pub fn probe_caps(&self) -> Result<RemoteCaps> {
        let mut caps = RemoteCaps::default();
        if self.run_test_cmd("find . -maxdepth 0 -printf ''") {
            caps.has_find_printf = true;
        }
        Ok(caps)
    }

    fn run_test_cmd(&self, cmd: &str) -> bool {
        matches!(self.exec(cmd), Ok((_, _, 0)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_scp_like() {
        let root = SshRoot::new("user@example.com:/data").unwrap();
        assert_eq!(root.user_host(), "user@example.com");
        assert_eq!(root.port(), None);
        assert_eq!(root.path(), Path::new("/data"));
    }

    #[test]
    fn parses_url_with_port() {
        let root = SshRoot::new("ssh://user@example.com:2222/var/www").unwrap();
        assert_eq!(root.user_host(), "user@example.com");
        assert_eq!(root.port(), Some(2222));
        assert_eq!(root.path(), Path::new("/var/www"));
    }
}

impl Root for SshRoot {
    fn kind(&self) -> RootType {
        RootType::Ssh
    }

    fn path(&self) -> &Path {
        &self.root_path
    }

    fn lstat(&self, path: &Path) -> Result<RootMetadata> {
        let abs_path = self.root_path.join(path);
        // stat -c "%F %s %Y %f" path
        // output ex: "regular file 1024 1678888888 81a4"
        // Note: %f gives raw mode in hex.
        let abs_path_q = shell_quote_path(&abs_path);
        let cmd_str = format!("stat -c '%F|%s|%Y|%f' -- {abs_path_q}");

        let (out, err, code) = self.exec(&cmd_str)?;
        if code != 0 {
            bail!("Remote stat failed: {}", String::from_utf8_lossy(&err));
        }
        let out_str = String::from_utf8(out)?;
        let parts: Vec<&str> = out_str.trim().split('|').collect();
        if parts.len() < 4 {
            bail!("Unexpected stat output: {}", out_str);
        }

        let _type_str = parts[0];
        let _size: u64 = parts[1].parse().context("parsing size")?;
        let mtime_secs: u64 = parts[2].parse().context("parsing mtime")?;
        let mode_hex = parts[3];
        let mode = u32::from_str_radix(mode_hex, 16).context("parsing mode")?;

        Ok(RootMetadata {
            mtime: UNIX_EPOCH + Duration::from_secs(mtime_secs),
            mode,
        })
    }

    fn try_lock(&self, path: &Path, info: &str) -> Result<()> {
        let abs_path = self.root_path.join(path);
        let abs_path_q = shell_quote_path(&abs_path);
        // Remote locking: mkdir .synchi/lockdir
        // mkdir is atomic on POSIX.
        // We also want to write the info inside.
        // Step 1: mkdir lock_path
        let (_out, err, code) = self.exec(&format!("mkdir -- {abs_path_q}"))?;
        if code != 0 {
            bail!("Failed to acquire lock: {}", String::from_utf8_lossy(&err));
        }
        // Step 2: write info to lock_path/owner
        let info_path = abs_path.join("owner");
        let info_str = info.to_string();
        let info_q = shell_quote(&info_str);
        let info_path_q = shell_quote_path(&info_path);
        // Use a temp implementation of write call? Or just echo.
        // Warning: echo info > path is risky if info has special chars, but PID/Host is usually safe.
        // We'll trust info is simple for now, or sanitize it.
        // Assuming strictly alphanumeric + logic.
        self.exec(&format!("printf '%s' {info_q} > {info_path_q}"))?;
        Ok(())
    }

    fn unlock(&self, path: &Path) -> Result<()> {
        let abs_path = self.root_path.join(path);
        let abs_path_q = shell_quote_path(&abs_path);
        // rm -rf lock_path
        self.exec(&format!("rm -rf -- {abs_path_q}"))?;
        Ok(())
    }

    fn open_read(&self, path: &Path) -> Result<Box<dyn Read + Send>> {
        let abs_path = self.root_path.join(path);
        let abs_path_q = shell_quote_path(&abs_path);
        // ssh user@host "cat 'path'"
        let mut cmd = self.ssh_command();
        cmd.arg(format!("cat -- {abs_path_q}"));

        let child = cmd.stdout(Stdio::piped()).spawn()?;
        let stdout = child.stdout.context("Failed to open stdout")?;

        // We need to keep child alive?
        // `stdout` is a `ChildStdout`. It doesn't keep the child alive by itself but when dropped pipe closes?
        // The child process needs to be reaped.
        // For short reads it's okay, but strictly we should wrap this.
        // But for now, returning Box<dyn Read> is fine.
        Ok(Box::new(stdout))
    }

    fn write_file(&self, path: &Path, content: &mut dyn Read) -> Result<()> {
        let abs_path = self.root_path.join(path);
        let abs_path_q = shell_quote_path(&abs_path);
        let _parent = abs_path.parent().unwrap();

        // Ensure parent exists? optional but good.
        // Write to temp: path.synchi-tmp
        let tmp_path = format!("{}.synchi-tmp", abs_path.display());
        let tmp_path_q = shell_quote(&tmp_path);

        let mut cmd = self.ssh_command();
        cmd.arg(format!("cat > {tmp_path_q}"));
        cmd.stdin(Stdio::piped());

        let mut child = cmd.spawn()?;
        let mut stdin = child.stdin.take().context("Failed to open stdin")?;

        io::copy(content, &mut stdin)?;
        drop(stdin); // Close stdin to signal EOF to cat

        let status = child.wait()?;
        if !status.success() {
            bail!("Failed to write file via ssh cat");
        }

        // Rename
        self.exec(&format!("mv -- {tmp_path_q} {abs_path_q}"))?;

        Ok(())
    }

    fn set_meta(&self, path: &Path, mode: u32, mtime: SystemTime) -> Result<()> {
        let abs_path = self.root_path.join(path);
        let abs_path_q = shell_quote_path(&abs_path);
        let ts = mtime.duration_since(UNIX_EPOCH)?.as_secs();
        // chmod and touch
        // chmod 0755 path
        // touch -d @ts path (GNU) or -t (POSIX)
        // busybox supports -d @seconds usually.
        self.exec(&format!(
            "chmod {:o} -- {abs_path_q} && touch -d @{ts} -- {abs_path_q}",
            mode
        ))?;
        Ok(())
    }

    fn mkdirs(&self, path: &Path) -> Result<()> {
        let abs_path = self.root_path.join(path);
        let abs_path_q = shell_quote_path(&abs_path);
        self.exec(&format!("mkdir -p -- {abs_path_q}"))?;
        Ok(())
    }

    fn remove_file(&self, path: &Path) -> Result<()> {
        let abs_path = self.root_path.join(path);
        let abs_path_q = shell_quote_path(&abs_path);
        self.exec(&format!("rm -- {abs_path_q}"))?;
        Ok(())
    }

    fn remove_dir(&self, path: &Path) -> Result<()> {
        let abs_path = self.root_path.join(path);
        let abs_path_q = shell_quote_path(&abs_path);
        self.exec(&format!("rmdir -- {abs_path_q}"))?; // rmdir for empty dir
        Ok(())
    }

    fn exec(&self, cmd: &str) -> Result<(Vec<u8>, Vec<u8>, i32)> {
        let mut command = self.ssh_command();
        command.arg(cmd);

        let output = command.output()?;
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
        if paths.is_empty() {
            return Ok(Vec::new());
        }

        // Construct command: cd root && sha256sum "path1" "path2" ...
        let root_str = self.root_path.to_string_lossy();
        let root_q = shell_quote(root_str.as_ref());
        let mut cmd = format!("cd {root_q} && sha256sum --");

        for path in paths {
            // path is relative to root (from Entry)
            // normalize_path returns it relative if default, which is good.
            let p = self.normalize_path(path)?;
            let p_q = shell_quote(p.to_string_lossy().as_ref());
            cmd.push(' ');
            cmd.push_str(&p_q);
        }

        let (out, _, code) = self.exec(&cmd)?;
        if code != 0 {
            anyhow::bail!("Remote hashing failed with code {}", code);
        }

        let out_str = String::from_utf8(out)?;
        let mut hash_map = std::collections::HashMap::new();

        for line in out_str.lines() {
            // output usually: "hash  ./path" or "hash  path"
            let parts: Vec<&str> = line.splitn(2, "  ").collect();
            if parts.len() == 2 {
                let h = parts[0].trim().to_string();
                let p = parts[1].trim().to_string();
                // strip leading ./ if any
                let p_clean = p.strip_prefix("./").unwrap_or(&p).to_string();
                hash_map.insert(p_clean, h);
            }
        }

        // Reconstruct result vector
        let mut result = Vec::new();
        for path in paths {
            let p = self.normalize_path(path)?.to_string_lossy().to_string();
            // Try strict match or match with ./ prefix?
            // Since we CD'd and passed relative paths, output should match.
            // But normalize_path returns what?
            // If we passed "foo.txt", p is "foo.txt". output is "foo.txt".
            // If hash_map has "foo.txt", great.

            if let Some(h) = hash_map.get(&p) {
                result.push(h.clone());
            } else if let Some(h) = hash_map.get(&format!("./{}", p)) {
                result.push(h.clone());
            } else {
                anyhow::bail!("Missing hash for {}", p);
            }
        }

        Ok(result)
    }
}
