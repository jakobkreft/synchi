use super::filter::ScanTargets;
use super::Filter;
use crate::roots::{EntryKind, RemoteCaps, Root, SshRoot};
use crate::scan::Entry;
use crate::shell::shell_quote;
use anyhow::{bail, Result};
use indicatif::ProgressBar;
use std::path::Path;
use tracing::{debug, instrument};

pub struct RemoteScanner<'a> {
    root: &'a SshRoot,
    filter: &'a Filter,
    caps: RemoteCaps,
}

fn kind_mode_bits(kind: EntryKind) -> u32 {
    match kind {
        EntryKind::File => 0o100000,
        EntryKind::Dir => 0o040000,
        EntryKind::Symlink => 0o120000,
    }
}

impl<'a> RemoteScanner<'a> {
    pub fn new(root: &'a SshRoot, filter: &'a Filter, caps: RemoteCaps) -> Self {
        Self { root, filter, caps }
    }

    #[instrument(skip(self))]
    pub fn scan_with_progress(&self, progress: Option<&ProgressBar>) -> Result<Vec<Entry>> {
        debug!("Starting remote scan at {:?}", self.root.path());
        let targets = match self.filter.scan_targets() {
            ScanTargets::None => return Ok(Vec::new()),
            other => other,
        };
        if self.caps.has_find_printf {
            return self.scan_find_printf(targets, progress);
        }

        bail!("Remote host must provide `find` with -printf support (GNU/BSD/BusyBox).")
    }

    fn scan_find_printf(
        &self,
        targets: ScanTargets,
        progress: Option<&ProgressBar>,
    ) -> Result<Vec<Entry>> {
        const PRINTF_FMT_BASIC: &str = "'%p\\0%y\\0%s\\0%n\\0%T@\\0%m\\0%l\\0'";
        const PRINTF_FMT_INODE: &str = "'%p\\0%y\\0%s\\0%n\\0%T@\\0%m\\0%l\\0%D\\0%i\\0'";
        let printf_fmt = if self.caps.has_find_inode {
            PRINTF_FMT_INODE
        } else {
            PRINTF_FMT_BASIC
        };
        let root_str = self.root.path().to_string_lossy();
        let root_q = shell_quote(root_str.as_ref());
        let find_cmd = match targets {
            ScanTargets::All => format!("find . -printf {printf_fmt}"),
            ScanTargets::Limited(prefixes) => {
                if prefixes.is_empty() {
                    "true".to_string()
                } else {
                    let mut segments = Vec::new();
                    for prefix in prefixes {
                        let rel = prefix.to_string_lossy().replace('\\', "/");
                        let rel = format!("./{rel}");
                        let quoted = shell_quote(&rel);
                        segments.push(format!(
                            "if [ -e {path} ] || [ -L {path} ]; then find {path} -printf {printf}; else true; fi",
                            path = quoted,
                            printf = printf_fmt
                        ));
                    }
                    segments.join("; ")
                }
            }
            ScanTargets::None => "true".to_string(),
        };
        let cmd = format!("cd {root_q} && {find_cmd}");
        let (out, err, code) = self.root.exec(&cmd)?;
        if code != 0 {
            bail!("Remote find failed: {}", String::from_utf8_lossy(&err));
        }

        let mut entries = Vec::new();
        let parts: Vec<&[u8]> = out.split(|&b| b == 0).collect();

        let parts = if let Some(last) = parts.last() {
            if last.is_empty() {
                &parts[..parts.len() - 1]
            } else {
                &parts[..]
            }
        } else {
            &parts[..]
        };

        let fields = if self.caps.has_find_inode { 9 } else { 7 };
        for chunk in parts.chunks(fields) {
            if chunk.len() < fields {
                break;
            }

            let path_bytes = chunk[0];
            let type_bytes = chunk[1];
            let size_bytes = chunk[2];
            let link_count_bytes = chunk[3];
            let mtime_bytes = chunk[4];
            let mode_bytes = chunk[5];
            let link_bytes = chunk[6];
            let (dev, inode) = if self.caps.has_find_inode {
                let dev_bytes = chunk[7];
                let inode_bytes = chunk[8];
                let dev = String::from_utf8_lossy(dev_bytes).parse().unwrap_or(0);
                let inode = String::from_utf8_lossy(inode_bytes).parse().unwrap_or(0);
                (dev, inode)
            } else {
                (0, 0)
            };

            let path_str = String::from_utf8_lossy(path_bytes).to_string();
            let rel_path_str = path_str.strip_prefix("./").unwrap_or(&path_str).to_string();
            let rel_path_p = Path::new(&rel_path_str);

            if rel_path_str.is_empty() || rel_path_str == "." {
                continue;
            }

            if self.filter.is_ignored(rel_path_p) || !self.filter.is_included(rel_path_p) {
                continue;
            }

            let kind_str = String::from_utf8_lossy(type_bytes);
            let kind = match kind_str.chars().next() {
                Some('f') => EntryKind::File,
                Some('d') => EntryKind::Dir,
                Some('l') => EntryKind::Symlink,
                _ => continue,
            };

            let size: u64 = String::from_utf8_lossy(size_bytes).parse().unwrap_or(0);
            let nlink: u64 = String::from_utf8_lossy(link_count_bytes)
                .parse()
                .unwrap_or(1);

            let mtime_str = String::from_utf8_lossy(mtime_bytes);
            let mtime = mtime_str
                .split('.')
                .next()
                .unwrap_or("0")
                .parse::<i64>()
                .unwrap_or(0);

            let mode_str = String::from_utf8_lossy(mode_bytes);
            let perm_bits = u32::from_str_radix(&mode_str, 8).unwrap_or(0);
            let mode = perm_bits | kind_mode_bits(kind);

            let link_target = if kind == EntryKind::Symlink {
                Some(String::from_utf8_lossy(link_bytes).to_string())
            } else {
                None
            };

            entries.push(Entry {
                path: rel_path_str,
                kind,
                size,
                mtime,
                mode,
                nlink,
                dev,
                inode,
                hash: None,
                link_target,
            });

            if let Some(pb) = progress {
                pb.inc(1);
            }
        }

        Ok(entries)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn combines_permission_and_type_bits() {
        assert_eq!(kind_mode_bits(EntryKind::File) | 0o644, 0o100644);
        assert_eq!(kind_mode_bits(EntryKind::Dir) | 0o755, 0o040755);
        assert_eq!(kind_mode_bits(EntryKind::Symlink) | 0o777, 0o120777);
    }
}
