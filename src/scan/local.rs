use crate::roots::{EntryKind, LocalRoot, Root};
use crate::scan::filter::ScanTargets;
use crate::scan::Filter;
use crate::state::Entry;
use anyhow::Result;
use indicatif::ProgressBar;
use std::collections::HashSet;
use std::fs;
use std::os::unix::fs::{MetadataExt, PermissionsExt};
use std::path::Path;
use std::time::SystemTime;
use tracing::{debug, instrument, warn};
use walkdir::WalkDir;

pub struct LocalScanner<'a> {
    root: &'a LocalRoot,
    filter: &'a Filter,
    skip_hardlinks: bool,
}

impl<'a> LocalScanner<'a> {
    #[cfg(test)]
    pub fn new(root: &'a LocalRoot, filter: &'a Filter) -> Self {
        Self {
            root,
            filter,
            skip_hardlinks: false,
        }
    }

    pub fn with_skip_hardlinks(
        root: &'a LocalRoot,
        filter: &'a Filter,
        skip_hardlinks: bool,
    ) -> Self {
        Self {
            root,
            filter,
            skip_hardlinks,
        }
    }

    #[cfg(test)]
    #[instrument(skip(self))]
    pub fn scan(&self) -> Result<Vec<Entry>> {
        self.scan_with_progress(None)
    }

    #[instrument(skip(self))]
    pub fn scan_with_progress(&self, progress: Option<&ProgressBar>) -> Result<Vec<Entry>> {
        debug!("Starting local scan at {:?}", self.root.path());
        let root_path = self.root.path();
        let mut entries = Vec::new();
        let mut seen = HashSet::new();
        match self.filter.scan_targets() {
            ScanTargets::None => return Ok(entries),
            ScanTargets::All => {
                self.walk_from(root_path, root_path, &mut entries, &mut seen, progress)?
            }
            ScanTargets::Limited(prefixes) => {
                for prefix in prefixes {
                    let start = root_path.join(&prefix);
                    if !start.exists() {
                        continue;
                    }
                    self.walk_from(&start, root_path, &mut entries, &mut seen, progress)?;
                }
            }
        }
        Ok(entries)
    }

    fn walk_from(
        &self,
        start: &Path,
        root_path: &Path,
        entries: &mut Vec<Entry>,
        seen: &mut HashSet<String>,
        progress: Option<&ProgressBar>,
    ) -> Result<()> {
        for entry in WalkDir::new(start).min_depth(0).follow_links(false) {
            let entry = match entry {
                Ok(e) => e,
                Err(_) => continue,
            };

            let path = entry.path();
            let rel_path = match path.strip_prefix(root_path) {
                Ok(p) => p,
                Err(_) => continue,
            };

            if rel_path == Path::new("") {
                continue;
            }

            if self.filter.is_ignored(rel_path) || !self.filter.is_included(rel_path) {
                continue;
            }

            let meta = fs::symlink_metadata(path)?;

            if self.skip_hardlinks && meta.is_file() && meta.nlink() > 1 {
                let path_str = rel_path.to_string_lossy();
                warn!("Skipping hard link: {} (nlink={})", path_str, meta.nlink());
                continue;
            }

            let file_type = meta.file_type();
            let kind = if file_type.is_symlink() {
                EntryKind::Symlink
            } else if file_type.is_dir() {
                EntryKind::Dir
            } else if file_type.is_file() {
                EntryKind::File
            } else {
                let path_str = rel_path.to_string_lossy();
                warn!("Skipping special file: {}", path_str);
                continue;
            };

            let mut path_str = rel_path.to_string_lossy().to_string();
            if cfg!(windows) {
                path_str = path_str.replace('\\', "/");
            }
            let path_str = path_str.trim_start_matches("./").to_string();

            if path_str.is_empty() || !seen.insert(path_str.clone()) {
                continue;
            }

            let size = meta.len();
            let mtime = meta
                .modified()
                .unwrap_or(SystemTime::UNIX_EPOCH)
                .duration_since(SystemTime::UNIX_EPOCH)?
                .as_secs() as i64;
            let mode = meta.permissions().mode();

            let link_target = if kind == EntryKind::Symlink {
                let target = fs::read_link(path)?.to_string_lossy().to_string();
                if target.starts_with('/') {
                    warn!(
                        "Absolute symlink detected: {} -> {} (may break at destination)",
                        path_str, target
                    );
                }
                Some(target)
            } else {
                None
            };

            entries.push(Entry {
                path: path_str,
                kind,
                size,
                mtime,
                mode,
                hash: None,
                link_target,
                deleted: false,
            });

            if let Some(pb) = progress {
                pb.inc(1);
            }
        }

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs::File;
    use std::io::Write;
    use std::os::unix::fs::symlink;
    use tempfile::TempDir;

    #[test]
    fn test_local_scanner() -> Result<()> {
        let tmp = TempDir::new()?;
        let root_path = tmp.path();

        // Setup:
        // root/
        //   keep.txt
        //   ignore.txt
        //   dir/
        //     nested.txt
        //   .synchi/
        //     state.db

        File::create(root_path.join("keep.txt"))?.write_all(b"content")?;
        File::create(root_path.join("ignore.txt"))?.write_all(b"ignored")?;
        std::fs::create_dir(root_path.join("dir"))?;
        File::create(root_path.join("dir/nested.txt"))?.write_all(b"nested")?;
        std::fs::create_dir(root_path.join(".synchi"))?;
        File::create(root_path.join(".synchi/state.db"))?;
        symlink("keep.txt", root_path.join("keep_link"))?;

        let root = LocalRoot::new(root_path)?;
        let filter = Filter::new(&["**".to_string()], &["ignore.txt".to_string()])?;

        let scanner = LocalScanner::new(&root, &filter);
        let entries = scanner.scan()?;

        let paths: Vec<String> = entries.iter().map(|e| e.path.clone()).collect();
        // Should contain: keep.txt, dir, dir/nested.txt
        // Should NOT contain: ignore.txt, .synchi, .synchi/state.db

        assert!(paths.contains(&"keep.txt".to_string()));
        assert!(paths.contains(&"dir".to_string()));
        assert!(paths.contains(&"dir/nested.txt".to_string()));
        assert!(paths.contains(&"keep_link".to_string()));

        assert!(!paths.contains(&"ignore.txt".to_string()));
        assert!(!paths.iter().any(|p| p.contains(".synchi")));

        let link_entry = entries
            .iter()
            .find(|e| e.path == "keep_link")
            .expect("symlink not found");
        assert_eq!(link_entry.kind, EntryKind::Symlink);
        assert_eq!(link_entry.link_target.as_deref(), Some("keep.txt"));

        Ok(())
    }

    #[cfg(unix)]
    #[test]
    fn test_local_scanner_preserves_backslashes() -> Result<()> {
        let tmp = TempDir::new()?;
        let root_path = tmp.path();

        let file_name = "foo\\bar.txt";
        File::create(root_path.join(file_name))?.write_all(b"content")?;

        let root = LocalRoot::new(root_path)?;
        let filter = Filter::new(&["**".to_string()], &[])?;
        let scanner = LocalScanner::new(&root, &filter);
        let entries = scanner.scan()?;

        let paths: Vec<String> = entries.iter().map(|e| e.path.clone()).collect();
        assert!(paths.contains(&file_name.to_string()));
        assert!(!paths.contains(&"foo/bar.txt".to_string()));

        Ok(())
    }
}
