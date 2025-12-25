use super::{RemoteCaps, Root, RootMetadata, RootType};
use crate::shell::{shell_quote, shell_quote_path};
use anyhow::{bail, Context, Result};
use std::io::{self, Read};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdout, Command, Stdio};
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
        let url = Url::parse(spec)
            .with_context(|| format!("Invalid SSH spec '{}', use ssh://user@host/path", spec))?;
        Self::from_url(url)
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

    fn exec_checked(&self, cmd: &str) -> Result<(Vec<u8>, Vec<u8>)> {
        let (out, err, code) = self.exec(cmd)?;
        if code != 0 {
            bail!(
                "Remote command failed (code {}): {}",
                code,
                String::from_utf8_lossy(&err).trim()
            );
        }
        Ok((out, err))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_scp_like() {
        let err = SshRoot::new("user@example.com:/data")
            .err()
            .expect("expected ssh:// error");
        assert!(
            err.to_string().contains("ssh://"),
            "unexpected error: {err}"
        );
    }

    #[test]
    fn parses_url_with_port() {
        let root = SshRoot::new("ssh://user@example.com:2222/var/www").unwrap();
        assert_eq!(root.user_host(), "user@example.com");
        assert_eq!(root.port(), Some(2222));
        assert_eq!(root.path(), Path::new("/var/www"));
    }

    #[test]
    fn parse_sha256sum_zero_preserves_whitespace() {
        let hash1 = "a".repeat(64);
        let hash2 = "b".repeat(64);
        let out = format!("{hash1}  ./ leading.txt\0{hash2}  trail.txt \0");
        let map = parse_sha256sum_zero(out.as_bytes()).unwrap();
        assert_eq!(map.get(" leading.txt"), Some(&hash1));
        assert_eq!(map.get("trail.txt "), Some(&hash2));
    }

    #[test]
    fn parse_sha256sum_text_preserves_whitespace() {
        let hash = "c".repeat(64);
        let out = format!("{hash}  ./ spaced .txt \n");
        let map = parse_sha256sum_text(out.as_bytes()).unwrap();
        assert_eq!(map.get(" spaced .txt "), Some(&hash));
    }
}

struct SshRead {
    child: Child,
    stdout: ChildStdout,
}

impl Read for SshRead {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        self.stdout.read(buf)
    }
}

impl Drop for SshRead {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
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

        let (out, _err) = self.exec_checked(&cmd_str)?;
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
        self.exec_checked(&format!("mkdir -- {abs_path_q}"))?;
        // Step 2: write info to lock_path/owner
        let info_path = abs_path.join("owner");
        let info_str = info.to_string();
        let info_q = shell_quote(&info_str);
        let info_path_q = shell_quote_path(&info_path);
        // Use a temp implementation of write call? Or just echo.
        // Warning: echo info > path is risky if info has special chars, but PID/Host is usually safe.
        // We'll trust info is simple for now, or sanitize it.
        // Assuming strictly alphanumeric + logic.
        self.exec_checked(&format!("printf '%s' {info_q} > {info_path_q}"))?;
        Ok(())
    }

    fn unlock(&self, path: &Path) -> Result<()> {
        let abs_path = self.root_path.join(path);
        let abs_path_q = shell_quote_path(&abs_path);
        // rm -rf lock_path
        self.exec_checked(&format!("rm -rf -- {abs_path_q}"))?;
        Ok(())
    }

    fn open_read(&self, path: &Path) -> Result<Box<dyn Read + Send>> {
        let abs_path = self.root_path.join(path);
        let abs_path_q = shell_quote_path(&abs_path);
        // ssh user@host "cat 'path'"
        let mut cmd = self.ssh_command();
        cmd.arg(format!("cat -- {abs_path_q}"));

        let mut child = cmd.stdout(Stdio::piped()).spawn()?;
        let stdout = child.stdout.take().context("Failed to open stdout")?;
        Ok(Box::new(SshRead { child, stdout }))
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
        self.exec_checked(&format!("mv -- {tmp_path_q} {abs_path_q}"))?;

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
        self.exec_checked(&format!(
            "chmod {:o} -- {abs_path_q} && touch -d @{ts} -- {abs_path_q}",
            mode
        ))?;
        Ok(())
    }

    fn mkdirs(&self, path: &Path) -> Result<()> {
        let abs_path = self.root_path.join(path);
        let abs_path_q = shell_quote_path(&abs_path);
        self.exec_checked(&format!("mkdir -p -- {abs_path_q}"))?;
        Ok(())
    }

    fn remove_file(&self, path: &Path) -> Result<()> {
        let abs_path = self.root_path.join(path);
        let abs_path_q = shell_quote_path(&abs_path);
        self.exec_checked(&format!("rm -- {abs_path_q}"))?;
        Ok(())
    }

    fn remove_dir(&self, path: &Path) -> Result<()> {
        let abs_path = self.root_path.join(path);
        let abs_path_q = shell_quote_path(&abs_path);
        self.exec_checked(&format!("rmdir -- {abs_path_q}"))?; // rmdir for empty dir
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

        let root_str = self.root_path.to_string_lossy();
        let root_q = shell_quote(root_str.as_ref());
        let cmd_zero = build_sha256sum_cmd(&root_q, paths, true)?;
        let (out, err, code) = self.exec(&cmd_zero)?;
        let hash_map = if code == 0 {
            parse_sha256sum_zero(&out)?
        } else if should_fallback_sha256sum(&err) {
            if paths
                .iter()
                .any(|p| p.to_string_lossy().contains('\n'))
            {
                anyhow::bail!(
                    "Remote sha256sum does not support --zero; filenames with newlines cannot be hashed safely"
                );
            }
            let cmd_text = build_sha256sum_cmd(&root_q, paths, false)?;
            let (out, err, code) = self.exec(&cmd_text)?;
            if code != 0 {
                anyhow::bail!(
                    "Remote hashing failed: {}",
                    String::from_utf8_lossy(&err)
                );
            }
            parse_sha256sum_text(&out)?
        } else {
            anyhow::bail!(
                "Remote hashing failed: {}",
                String::from_utf8_lossy(&err)
            );
        };

        let mut result = Vec::with_capacity(paths.len());
        for path in paths {
            let p = self.normalize_path(path)?.to_string_lossy().to_string();
            let key = p.strip_prefix("./").unwrap_or(&p);
            if let Some(h) = hash_map.get(key) {
                result.push(h.clone());
            } else {
                anyhow::bail!("Missing hash for {}", p);
            }
        }

        Ok(result)
    }
}

fn build_sha256sum_cmd(root_q: &str, paths: &[PathBuf], zero: bool) -> Result<String> {
    let mut cmd = if zero {
        format!("cd {root_q} && sha256sum --zero --")
    } else {
        format!("cd {root_q} && sha256sum --")
    };
    for path in paths {
        let p_q = shell_quote(path.to_string_lossy().as_ref());
        cmd.push(' ');
        cmd.push_str(&p_q);
    }
    Ok(cmd)
}

fn should_fallback_sha256sum(err: &[u8]) -> bool {
    let msg = String::from_utf8_lossy(err).to_ascii_lowercase();
    msg.contains("unrecognized option")
        || msg.contains("unknown option")
        || msg.contains("illegal option")
        || msg.contains("invalid option")
}

fn parse_sha256sum_zero(out: &[u8]) -> Result<std::collections::HashMap<String, String>> {
    let mut map = std::collections::HashMap::new();
    for record in out.split(|b| *b == 0) {
        if record.is_empty() {
            continue;
        }
        let sep = find_double_space(record)
            .ok_or_else(|| anyhow::anyhow!("Unexpected sha256sum --zero output"))?;
        let hash = std::str::from_utf8(&record[..sep])?;
        let path_bytes = &record[sep + 2..];
        let mut path = String::from_utf8_lossy(path_bytes).to_string();
        if let Some(stripped) = path.strip_prefix("./") {
            path = stripped.to_string();
        }
        map.insert(path, hash.to_string());
    }
    Ok(map)
}

fn parse_sha256sum_text(out: &[u8]) -> Result<std::collections::HashMap<String, String>> {
    let mut map = std::collections::HashMap::new();
    let out_str = std::str::from_utf8(out)?;
    for line in out_str.lines() {
        if line.is_empty() {
            continue;
        }
        let sep = line
            .find("  ")
            .ok_or_else(|| anyhow::anyhow!("Unexpected sha256sum output"))?;
        let hash = &line[..sep];
        let mut path = line[sep + 2..].to_string();
        if let Some(stripped) = path.strip_prefix("./") {
            path = stripped.to_string();
        }
        map.insert(path, hash.to_string());
    }
    Ok(map)
}

fn find_double_space(bytes: &[u8]) -> Option<usize> {
    bytes.windows(2).position(|pair| pair == b"  ")
}
