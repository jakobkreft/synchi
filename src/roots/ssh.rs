use super::{RemoteCaps, Root, RootMetadata, RootType};
use crate::shell::{shell_quote, shell_quote_path};
use anyhow::{bail, Context, Result};
use std::io::{self, Read};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdout, Command, Stdio};
use std::thread::JoinHandle;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

#[derive(Clone)]
pub struct SshRoot {
    user_host: String, // user@host or just host
    port: Option<u16>,
    root_path: PathBuf,
}

impl SshRoot {
    pub fn from_parts(
        user: Option<String>,
        host: String,
        port: Option<u16>,
        path: PathBuf,
    ) -> Result<Self> {
        if host.trim().is_empty() {
            bail!("Missing host in SSH spec");
        }
        let user_host = match user {
            Some(user) if !user.is_empty() => format!("{}@{}", user, host),
            _ => host,
        };
        let root_path = if path.as_os_str().is_empty() {
            PathBuf::from("/")
        } else {
            path
        };
        Ok(Self {
            user_host,
            port,
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
        if caps.has_find_printf && self.run_test_cmd("find . -maxdepth 0 -printf '%D %i'") {
            caps.has_find_inode = true;
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
    stderr_handle: Option<JoinHandle<io::Result<Vec<u8>>>>,
    done: bool,
    error: Option<io::Error>,
}

impl Read for SshRead {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        if let Some(err) = self.error.take() {
            self.error = Some(io::Error::new(err.kind(), err.to_string()));
            return Err(err);
        }
        if self.done {
            return Ok(0);
        }
        let read = self.stdout.read(buf)?;
        if read == 0 {
            if let Err(err) = self.finalize() {
                self.error = Some(io::Error::new(err.kind(), err.to_string()));
                return Err(err);
            }
            self.done = true;
        }
        Ok(read)
    }
}

impl Drop for SshRead {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
        if let Some(handle) = self.stderr_handle.take() {
            let _ = handle.join();
        }
    }
}

impl SshRead {
    fn finalize(&mut self) -> io::Result<()> {
        let status = self.child.wait()?;
        let stderr = match self.stderr_handle.take() {
            Some(handle) => match handle.join() {
                Ok(result) => result?,
                Err(_) => return Err(io::Error::other("ssh stderr thread panicked")),
            },
            None => Vec::new(),
        };
        if status.success() {
            return Ok(());
        }
        let stderr_str = String::from_utf8_lossy(&stderr);
        let trimmed = stderr_str.trim();
        let mut message = String::from("remote read failed");
        if !trimmed.is_empty() {
            message.push_str(": ");
            message.push_str(trimmed);
        } else if let Some(code) = status.code() {
            message.push_str(&format!(" (exit code {code})"));
        } else {
            message.push_str(" (terminated by signal)");
        }
        Err(io::Error::other(message))
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

        let mut child = cmd.stdout(Stdio::piped()).stderr(Stdio::piped()).spawn()?;
        let stdout = child.stdout.take().context("Failed to open stdout")?;
        let stderr = child.stderr.take().context("Failed to open stderr")?;
        let stderr_handle = std::thread::spawn(move || {
            let mut buf = Vec::new();
            let mut stderr = stderr;
            stderr.read_to_end(&mut buf)?;
            Ok(buf)
        });
        Ok(Box::new(SshRead {
            child,
            stdout,
            stderr_handle: Some(stderr_handle),
            done: false,
            error: None,
        }))
    }

    fn write_file(&self, path: &Path, content: &mut dyn Read) -> Result<()> {
        let abs_path = self.root_path.join(path);
        let abs_path_q = shell_quote_path(&abs_path);
        if let Some(parent) = abs_path.parent() {
            let parent_q = shell_quote_path(parent);
            self.exec_checked(&format!("mkdir -p -- {parent_q}"))?;
        }

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

    fn create_symlink(&self, target: &str, path: &Path) -> Result<()> {
        let abs_path = self.root_path.join(path);
        let abs_path_q = shell_quote_path(&abs_path);
        if let Some(parent) = abs_path.parent() {
            let parent_q = shell_quote_path(parent);
            self.exec_checked(&format!("mkdir -p -- {parent_q}"))?;
        }
        let target_q = shell_quote(target);
        self.exec_checked(&format!(
            "rm -f -- {abs_path_q} && ln -s -- {target_q} {abs_path_q}"
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
            if paths.iter().any(|p| p.to_string_lossy().contains('\n')) {
                anyhow::bail!(
                    "Remote sha256sum does not support --zero; filenames with newlines cannot be hashed safely"
                );
            }
            let cmd_text = build_sha256sum_cmd(&root_q, paths, false)?;
            let (out, err, code) = self.exec(&cmd_text)?;
            if code != 0 {
                anyhow::bail!("Remote hashing failed: {}", String::from_utf8_lossy(&err));
            }
            parse_sha256sum_text(&out)?
        } else {
            anyhow::bail!("Remote hashing failed: {}", String::from_utf8_lossy(&err));
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
