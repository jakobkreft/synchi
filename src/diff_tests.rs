use super::*;
use crate::roots::EntryKind;
use crate::scan::Filter;

fn make_entry(path: &str, mtime: i64) -> Entry {
    Entry {
        path: path.to_string(),
        kind: EntryKind::File,
        size: 100,
        mtime,
        mode: 0o644,
        nlink: 1,
        hash: None,
        link_target: None,
        deleted: false,
    }
}

fn include_all_filter() -> Filter {
    Filter::new(&["**".to_string()], &[]).unwrap()
}

#[test]
fn test_diff_basic_created() {
    let scan_a = vec![make_entry("new.txt", 100)];
    let state_a = vec![];
    let scan_b = vec![];
    let state_b = vec![];

    let filter = include_all_filter();
    let diff = DiffEngine::diff(scan_a, state_a, scan_b, state_b, &filter);
    assert_eq!(diff.len(), 1);
    let d = &diff[0];
    assert_eq!(d.path, "new.txt");
    assert_eq!(d.change_a.change, ChangeType::Created);
    assert!(matches!(d.action, SyncAction::CopyAtoB));
}

#[test]
fn test_diff_both_modified_conflict() {
    let e_state = make_entry("file.txt", 100);
    let e_a = make_entry("file.txt", 200);
    let e_b = make_entry("file.txt", 300); // Different mtime

    let scan_a = vec![e_a.clone()];
    let state_a = vec![e_state.clone()];
    let scan_b = vec![e_b.clone()];
    let state_b = vec![e_state.clone()];

    let filter = include_all_filter();
    let diff = DiffEngine::diff(scan_a, state_a, scan_b, state_b, &filter);
    assert_eq!(diff.len(), 1);
    let d = &diff[0];
    assert!(matches!(
        d.action,
        SyncAction::Conflict(ConflictReason::BothModified)
    ));
}

#[test]
fn test_diff_propagated_delete() {
    // A deleted "file.txt". B unchanged.
    let e_state = make_entry("file.txt", 100);

    let scan_a = vec![]; // Deleted physically
    let state_a = vec![e_state.clone()];

    let scan_b = vec![e_state.clone()];
    let state_b = vec![e_state.clone()];

    let filter = include_all_filter();
    let diff = DiffEngine::diff(scan_a, state_a, scan_b, state_b, &filter);
    assert_eq!(diff.len(), 1);
    let d = &diff[0];
    assert_eq!(d.change_a.change, ChangeType::Deleted);
    assert_eq!(d.change_b.change, ChangeType::Unchanged);
    // Should propagate delete to B
    assert!(matches!(d.action, SyncAction::DeleteB));
}

#[test]
fn test_diff_delete_vs_create_conflict() {
    // A recreated file, B deleted -> conflict
    let prev = make_entry("file.txt", 100);
    let scan_a = vec![make_entry("file.txt", 200)];
    let scan_b = vec![];
    let state = vec![prev.clone()];

    let filter = include_all_filter();
    let diff = DiffEngine::diff(scan_a, state.clone(), scan_b, state, &filter);
    assert_eq!(diff.len(), 1);
    assert!(matches!(
        diff[0].action,
        SyncAction::Conflict(ConflictReason::DeleteVsModify)
    ));
}

#[test]
fn test_diff_both_created_identical_no_conflict() {
    let mut entry = make_entry("same.txt", 100);
    entry.hash = Some(vec![1, 2, 3]);
    let scan_a = vec![entry.clone()];
    let scan_b = vec![entry.clone()];
    let filter = include_all_filter();
    let diffs = DiffEngine::diff(scan_a, vec![], scan_b, vec![], &filter);
    assert_eq!(diffs.len(), 1);
    assert!(matches!(diffs[0].action, SyncAction::NoOp));
}

#[test]
fn test_diff_both_modified_identical_no_conflict() {
    let mut prev = make_entry("same.txt", 50);
    prev.hash = Some(vec![0, 0, 0]);
    let mut updated = make_entry("same.txt", 200);
    updated.hash = Some(vec![5, 5, 5]);
    let scan_a = vec![updated.clone()];
    let scan_b = vec![updated.clone()];
    let state = vec![prev];
    let filter = include_all_filter();
    let diffs = DiffEngine::diff(scan_a, state.clone(), scan_b, state, &filter);
    assert_eq!(diffs.len(), 1);
    assert!(matches!(diffs[0].action, SyncAction::NoOp));
}

#[test]
fn test_balanced_detects_hash_change() {
    let mut state_entry = make_entry("file.txt", 100);
    state_entry.hash = Some(vec![1, 2, 3]);
    let mut scan_entry = state_entry.clone();
    scan_entry.hash = Some(vec![9, 9, 9]);
    let scan_a = vec![scan_entry];
    let state_a = vec![state_entry.clone()];
    let scan_b = vec![state_entry.clone()];
    let state_b = vec![state_entry];

    let filter = include_all_filter();
    let diff_hash = DiffEngine::diff(scan_a, state_a, scan_b, state_b, &filter);
    assert!(matches!(diff_hash[0].action, SyncAction::CopyAtoB));
}

#[test]
fn test_balanced_skips_false_positive_when_hash_matches() {
    let mut state_entry = make_entry("file.txt", 100);
    state_entry.hash = Some(vec![1, 2, 3]);
    let mut scan_entry = state_entry.clone();
    scan_entry.mtime = 200;
    scan_entry.hash = Some(vec![1, 2, 3]);

    let scan_a = vec![scan_entry];
    let state_a = vec![state_entry.clone()];
    let scan_b = vec![state_entry.clone()];
    let state_b = vec![state_entry];
    let filter = include_all_filter();

    let diffs = DiffEngine::diff(scan_a, state_a, scan_b, state_b, &filter);
    assert!(matches!(diffs[0].action, SyncAction::NoOp));
}

#[test]
fn test_diff_ignore_safe() {
    // A ignored "file.txt" (exists in state, missing in scan).
    // Should NOT be treated as Deleted.
    let e_state = make_entry("file.txt", 100);

    let scan_a = vec![];
    let state_a = vec![e_state.clone()];

    // B has it (Unchanged)
    let scan_b = vec![e_state.clone()];
    let state_b = vec![e_state.clone()];

    // Filter ignores "file.txt"
    let filter = Filter::new(&[], &["file.txt".into()]).unwrap();

    let diff = DiffEngine::diff(scan_a, state_a, scan_b, state_b, &filter);

    // Should result in NoOp for this file, or Unchanged-Unchanged.
    assert_eq!(diff.len(), 1);
    let d = &diff[0];
    assert_eq!(d.change_a.change, ChangeType::Unchanged); // Treated as Unchanged because Ignored
    assert_eq!(d.change_b.change, ChangeType::Unchanged);
    assert!(matches!(d.action, SyncAction::NoOp));
}

#[test]
fn test_diff_include_empty_ignores_all() {
    let e_state = make_entry("file.txt", 100);
    let scan_a = vec![];
    let scan_b = vec![];
    let state_a = vec![e_state.clone()];
    let state_b = vec![e_state.clone()];

    let filter = Filter::new(&[], &[]).unwrap();
    let diff = DiffEngine::diff(scan_a, state_a, scan_b, state_b, &filter);
    assert_eq!(diff.len(), 1);
    let d = &diff[0];
    assert_eq!(d.change_a.change, ChangeType::Unchanged);
    assert_eq!(d.change_b.change, ChangeType::Unchanged);
    assert!(matches!(d.action, SyncAction::NoOp));
}
