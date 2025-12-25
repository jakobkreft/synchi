use crate::roots::{EntryKind, LocalRoot, Root, RootType, SshRoot};
use crate::shell::shell_quote;
use crate::state::Entry;
use anyhow::{anyhow, bail, Context, Result};
use std::io::{Read, Write};
use std::process::{Child, ChildStderr, ChildStdin, ChildStdout, Command, Stdio};
use std::sync::{
    atomic::{AtomicU64, Ordering},
    Arc,
};
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use std::thread::JoinHandle;
use tracing::debug;

#[derive(Clone, Copy)]
pub struct CopyBehavior {
    pub preserve_owner: bool,
    pub preserve_permissions: bool,
}

impl Default for CopyBehavior {
    fn default() -> Self {
        Self {
            preserve_owner: true,
            preserve_permissions: true,
        }
    }
}

pub struct Transport;

impl Transport {
    pub fn copy_file(
        src_root: &dyn Root,
        dest_root: &dyn Root,
        path: &str,
        behavior: CopyBehavior,
    ) -> Result<()> {
        let p = std::path::Path::new(path);
        let mut reader = src_root
            .open_read(p)
            .context("Failed to open source file")?;
        dest_root
            .write_file(p, &mut reader)
            .context("Failed to write dest file")?;

        if behavior.preserve_permissions {
            let meta = src_root.lstat(p)?;
            dest_root.set_meta(p, meta.mode, meta.mtime)?;
        }
        Ok(())
    }

    pub fn copy_entry(
        src_root: &dyn Root,
        dest_root: &dyn Root,
        entry: &Entry,
        behavior: CopyBehavior,
    ) -> Result<()> {
        let path = std::path::Path::new(&entry.path);
        match entry.kind {
            EntryKind::File => Transport::copy_file(src_root, dest_root, &entry.path, behavior),
            EntryKind::Dir => {
                dest_root.mkdirs(path)?;
                if behavior.preserve_permissions {
                    let mtime = entry_mtime(entry.mtime);
                    dest_root.set_meta(path, entry.mode, mtime)?;
                }
                Ok(())
            }
            EntryKind::Symlink => {
                let target = entry
                    .link_target
                    .as_deref()
                    .ok_or_else(|| anyhow!("Missing symlink target for {}", entry.path))?;
                dest_root.create_symlink(target, path)?;
                Ok(())
            }
        }
    }

    pub fn persistent_stream<'a>(
        src_root: &'a dyn Root,
        dest_root: &'a dyn Root,
        behavior: CopyBehavior,
    ) -> Result<CopyStream<'a>> {
        match TarStream::new(src_root, dest_root, behavior) {
            Ok(stream) => Ok(CopyStream {
                channel: CopyChannel::Tar(stream),
                behavior,
            }),
            Err(err) => {
                debug!("Falling back to per-file transfer: unable to open tar stream: {err:?}");
                Ok(CopyStream {
                    channel: CopyChannel::Manual {
                        src_root,
                        dest_root,
                    },
                    behavior,
                })
            }
        }
    }
}

pub struct CopyStream<'a> {
    channel: CopyChannel<'a>,
    behavior: CopyBehavior,
}

enum CopyChannel<'a> {
    Tar(TarStream),
    Manual {
        src_root: &'a dyn Root,
        dest_root: &'a dyn Root,
    },
}

impl<'a> CopyStream<'a> {
    pub fn send_entries(&mut self, entries: &[Entry]) -> Result<()> {
        match &mut self.channel {
            CopyChannel::Tar(stream) => {
                let paths: Vec<String> = entries.iter().map(|e| e.path.clone()).collect();
                stream.send_paths(&paths)
            }
            CopyChannel::Manual {
                src_root,
                dest_root,
            } => {
                for entry in entries {
                    Transport::copy_entry(*src_root, *dest_root, entry, self.behavior)?;
                }
                Ok(())
            }
        }
    }

    pub fn finish(self) -> Result<()> {
        match self.channel {
            CopyChannel::Tar(stream) => stream.finish(),
            CopyChannel::Manual { .. } => Ok(()),
        }
    }

    pub fn progress_counter(&self) -> Option<Arc<AtomicU64>> {
        match &self.channel {
            CopyChannel::Tar(stream) => Some(stream.progress_counter()),
            _ => None,
        }
    }

    pub fn is_streaming(&self) -> bool {
        matches!(&self.channel, CopyChannel::Tar(_))
    }
}

fn entry_mtime(mtime: i64) -> SystemTime {
    if mtime <= 0 {
        UNIX_EPOCH
    } else {
        UNIX_EPOCH + Duration::from_secs(mtime as u64)
    }
}

struct TarStream {
    pack_child: Child,
    pack_stdin: Option<ChildStdin>,
    pack_stderr: Option<ChildStderr>,
    unpack_child: Child,
    unpack_stderr: Option<ChildStderr>,
    pump: Option<JoinHandle<Result<()>>>,
    bytes_copied: Arc<AtomicU64>,
}

impl TarStream {
    fn new(src_root: &dyn Root, dest_root: &dyn Root, behavior: CopyBehavior) -> Result<Self> {
        let (mut pack_child, pack_stdin, pack_stdout, pack_stderr) =
            spawn_tar_pack(src_root).context("launching tar pack on source root")?;
        let (unpack_child, unpack_stdin, unpack_stderr) =
            match spawn_tar_unpack(dest_root, behavior).context("launching tar unpack on destination root") {
                Ok(result) => result,
                Err(err) => {
                    let _ = pack_child.kill();
                    let _ = pack_child.wait();
                    return Err(err);
                }
            };

        let counter = Arc::new(AtomicU64::new(0));
        let pump_counter = counter.clone();
        let pump = std::thread::spawn(move || -> Result<()> {
            let mut reader = pack_stdout;
            let mut writer = unpack_stdin;
            let mut buf = [0u8; 64 * 1024];
            loop {
                let read = reader.read(&mut buf)?;
                if read == 0 {
                    break;
                }
                writer.write_all(&buf[..read])?;
                pump_counter.fetch_add(read as u64, Ordering::Relaxed);
            }
            Ok(())
        });

        Ok(Self {
            pack_child,
            pack_stdin: Some(pack_stdin),
            pack_stderr,
            unpack_child,
            unpack_stderr,
            pump: Some(pump),
            bytes_copied: counter,
        })
    }

    fn send_paths(&mut self, paths: &[String]) -> Result<()> {
        if paths.is_empty() {
            return Ok(());
        }
        let stdin = self
            .pack_stdin
            .as_mut()
            .context("tar stream stdin already closed")?;
        for path in paths {
            stdin.write_all(path.as_bytes())?;
            stdin.write_all(&[0])?;
        }
        stdin.flush()?;
        Ok(())
    }

    fn finish(mut self) -> Result<()> {
        drop(self.pack_stdin.take());

        if let Some(pump) = self.pump.take() {
            let result = pump
                .join()
                .map_err(|e| anyhow!("tar pump thread panicked: {e:?}"))?;
            result?;
        }

        let pack_status = self.pack_child.wait()?;
        if !pack_status.success() {
            let mut err_msg = String::from("tar pack process failed");
            append_stderr(&mut err_msg, self.pack_stderr.take());
            bail!("{err_msg}");
        }

        let unpack_status = self.unpack_child.wait()?;
        if !unpack_status.success() {
            let mut err_msg = String::from("tar unpack process failed");
            append_stderr(&mut err_msg, self.unpack_stderr.take());
            bail!("{err_msg}");
        }

        Ok(())
    }

    fn progress_counter(&self) -> Arc<AtomicU64> {
        self.bytes_copied.clone()
    }
}

fn append_stderr(message: &mut String, stderr: Option<ChildStderr>) {
    if let Some(mut stderr) = stderr {
        let mut buf = String::new();
        if stderr.read_to_string(&mut buf).is_ok() && !buf.trim().is_empty() {
            message.push_str(": ");
            message.push_str(buf.trim());
        }
    }
}

fn spawn_tar_pack(
    root: &dyn Root,
) -> Result<(Child, ChildStdin, ChildStdout, Option<ChildStderr>)> {
    match root.kind() {
        RootType::Local => {
            let local = root
                .as_any()
                .downcast_ref::<LocalRoot>()
                .context("invalid local root downcast")?;
            spawn_local_tar_pack(local)
        }
        RootType::Ssh => {
            let ssh = root
                .as_any()
                .downcast_ref::<SshRoot>()
                .context("invalid ssh root downcast")?;
            spawn_ssh_tar_pack(ssh)
        }
    }
}

fn spawn_tar_unpack(
    root: &dyn Root,
    behavior: CopyBehavior,
) -> Result<(Child, ChildStdin, Option<ChildStderr>)> {
    match root.kind() {
        RootType::Local => {
            let local = root
                .as_any()
                .downcast_ref::<LocalRoot>()
                .context("invalid local root downcast")?;
            spawn_local_tar_unpack(local, behavior)
        }
        RootType::Ssh => {
            let ssh = root
                .as_any()
                .downcast_ref::<SshRoot>()
                .context("invalid ssh root downcast")?;
            spawn_ssh_tar_unpack(ssh, behavior)
        }
    }
}

fn spawn_local_tar_pack(
    root: &LocalRoot,
) -> Result<(Child, ChildStdin, ChildStdout, Option<ChildStderr>)> {
    let mut cmd = Command::new("tar");
    cmd.arg("-cf")
        .arg("-")
        .arg("--null")
        .arg("--no-recursion")
        .arg("-T")
        .arg("-")
        .current_dir(root.path());
    cmd.stdin(Stdio::piped());
    cmd.stdout(Stdio::piped());
    cmd.stderr(Stdio::piped());
    let mut child = cmd.spawn().context("spawning local tar pack")?;
    let stdin = child.stdin.take().context("missing tar stdin")?;
    let stdout = child.stdout.take().context("missing tar stdout")?;
    let stderr = child.stderr.take();
    Ok((child, stdin, stdout, stderr))
}

fn spawn_local_tar_unpack(
    root: &LocalRoot,
    behavior: CopyBehavior,
) -> Result<(Child, ChildStdin, Option<ChildStderr>)> {
    let mut cmd = Command::new("tar");
    cmd.arg("-xpf").arg("-");
    if behavior.preserve_permissions {
        cmd.arg("--preserve-permissions");
    } else {
        cmd.arg("--no-same-permissions");
    }
    if behavior.preserve_owner {
        cmd.arg("--same-owner");
    } else {
        cmd.arg("--no-same-owner");
    }
    cmd.current_dir(root.path());
    cmd.stdin(Stdio::piped());
    cmd.stderr(Stdio::piped());
    let mut child = cmd.spawn().context("spawning local tar unpack")?;
    let stdin = child.stdin.take().context("missing unpack stdin")?;
    let stderr = child.stderr.take();
    Ok((child, stdin, stderr))
}

fn spawn_ssh_tar_pack(
    root: &SshRoot,
) -> Result<(Child, ChildStdin, ChildStdout, Option<ChildStderr>)> {
    let mut cmd = root.ssh_command();
    let root_str = root.path().to_string_lossy();
    let root_q = shell_quote(root_str.as_ref());
    cmd.arg(format!(
        "cd {root_q} && tar -cf - --null --no-recursion -T -"
    ));
    cmd.stdin(Stdio::piped());
    cmd.stdout(Stdio::piped());
    cmd.stderr(Stdio::piped());
    let mut child = cmd.spawn().context("spawning ssh tar pack")?;
    let stdin = child.stdin.take().context("missing ssh tar stdin")?;
    let stdout = child.stdout.take().context("missing ssh tar stdout")?;
    let stderr = child.stderr.take();
    Ok((child, stdin, stdout, stderr))
}

fn spawn_ssh_tar_unpack(
    root: &SshRoot,
    behavior: CopyBehavior,
) -> Result<(Child, ChildStdin, Option<ChildStderr>)> {
    let mut cmd = root.ssh_command();
    let root_str = root.path().to_string_lossy();
    let root_q = shell_quote(root_str.as_ref());
    let mut tar_cmd = format!("cd {root_q} && tar -xpf -");
    if behavior.preserve_permissions {
        tar_cmd.push_str(" --preserve-permissions");
    } else {
        tar_cmd.push_str(" --no-same-permissions");
    }
    if behavior.preserve_owner {
        tar_cmd.push_str(" --same-owner");
    } else {
        tar_cmd.push_str(" --no-same-owner");
    }
    cmd.arg(tar_cmd);
    cmd.stdin(Stdio::piped());
    cmd.stderr(Stdio::piped());
    let mut child = cmd.spawn().context("spawning ssh tar unpack")?;
    let stdin = child.stdin.take().context("missing ssh unpack stdin")?;
    let stderr = child.stderr.take();
    Ok((child, stdin, stderr))
}
