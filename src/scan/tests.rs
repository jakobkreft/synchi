#[cfg(test)]
mod tests {
    use crate::scan::filter::{Filter, ScanTargets};
    use crate::scan::{hardlink_groups, Entry};
    use crate::roots::EntryKind;
    use std::path::{Path, PathBuf};

    #[test]
    fn test_filter_basic() {
        let f = Filter::new(&["src/**".to_string()], &["*.rs".to_string()]).unwrap();

        assert!(f.is_included(Path::new("src/main.rs")));
        // It is ignored because *.rs is ignored
        assert!(f.is_ignored(Path::new("src/main.rs")));

        assert!(f.is_included(Path::new("src/README.md")));
        assert!(!f.is_ignored(Path::new("src/README.md")));

        assert!(!f.is_included(Path::new("target/debug")));
    }

    #[test]
    fn scan_targets_limited_prefix() {
        let f = Filter::new(
            &["Pictures/**".to_string(), "Documents/**".to_string()],
            &[],
        )
        .unwrap();
        match f.scan_targets() {
            ScanTargets::Limited(prefixes) => {
                let expected: Vec<PathBuf> = vec!["Documents".into(), "Pictures".into()];
                assert_eq!(prefixes, expected);
            }
            other => panic!("expected Limited, got {:?}", other),
        }
    }

    #[test]
    fn scan_targets_all_for_wildcards() {
        let f = Filter::new(&["**/wiki/**".to_string()], &[]).unwrap();
        assert!(matches!(f.scan_targets(), ScanTargets::All));
    }

    #[test]
    fn scan_targets_none_when_empty() {
        let f = Filter::new(&[], &[]).unwrap();
        assert!(matches!(f.scan_targets(), ScanTargets::None));
    }

    #[test]
    fn hardlink_groups_collect_multiple_paths() {
        let entries = vec![
            Entry {
                path: "a.txt".to_string(),
                kind: EntryKind::File,
                size: 1,
                mtime: 0,
                mode: 0o644,
                nlink: 2,
                dev: 1,
                inode: 10,
                hash: None,
                link_target: None,
            },
            Entry {
                path: "b.txt".to_string(),
                kind: EntryKind::File,
                size: 1,
                mtime: 0,
                mode: 0o644,
                nlink: 2,
                dev: 1,
                inode: 10,
                hash: None,
                link_target: None,
            },
            Entry {
                path: "c.txt".to_string(),
                kind: EntryKind::File,
                size: 1,
                mtime: 0,
                mode: 0o644,
                nlink: 2,
                dev: 1,
                inode: 11,
                hash: None,
                link_target: None,
            },
            Entry {
                path: "dir".to_string(),
                kind: EntryKind::Dir,
                size: 0,
                mtime: 0,
                mode: 0o755,
                nlink: 2,
                dev: 1,
                inode: 12,
                hash: None,
                link_target: None,
            },
        ];

        let groups = hardlink_groups(&entries);
        assert_eq!(groups.len(), 1);
        let group = groups.values().next().unwrap();
        assert_eq!(group.len(), 2);
        assert!(group.contains(&"a.txt".to_string()));
        assert!(group.contains(&"b.txt".to_string()));
    }
}
