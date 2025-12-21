#[cfg(test)]
mod tests {
    use crate::scan::filter::{Filter, ScanTargets};
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
}
