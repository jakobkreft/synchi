use crate::roots::EntryKind;
use crate::state::Entry;
use std::collections::{HashMap, HashSet};
use tracing::debug;

use crate::scan::Filter;

#[cfg(test)]
#[path = "diff_tests.rs"]
mod tests;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ChangeType {
    Unchanged,
    Created,
    Deleted,
    Modified,
    TypeChanged, // e.g. File -> Dir
}

#[derive(Debug, Clone)]
pub struct SideChange {
    pub change: ChangeType,
    pub entry_now: Option<Entry>,
    pub entry_prev: Option<Entry>, // From state DB
}

#[derive(Debug, Clone)]
pub enum SyncAction {
    CopyAtoB,
    CopyBtoA,
    DeleteA,
    DeleteB,
    NoOp,
    Conflict(ConflictReason),
    // Complex merges or renames could be added here
}

#[derive(Debug, Clone)]
pub enum ConflictReason {
    BothModified,
    DeleteVsModify,
    TypeMismatch,
}

#[derive(Debug, Clone)]
pub struct DiffResult {
    pub path: String,
    pub change_a: SideChange,
    pub change_b: SideChange,
    pub action: SyncAction,
}

pub struct DiffEngine;

impl DiffEngine {
    pub fn diff(
        scan_a: Vec<Entry>,
        state_a: Vec<Entry>,
        scan_b: Vec<Entry>,
        state_b: Vec<Entry>,
        filter: &Filter,
    ) -> Vec<DiffResult> {
        // Index everything by path
        debug!(
            "Diffing {} vs {} (State A: {}, State B: {})",
            scan_a.len(),
            scan_b.len(),
            state_a.len(),
            state_b.len()
        );
        let mut all_paths = HashSet::new();

        let map_scan_a = Self::index_entries(scan_a, &mut all_paths);
        let map_state_a = Self::index_entries(state_a, &mut all_paths);
        let map_scan_b = Self::index_entries(scan_b, &mut all_paths);
        let map_state_b = Self::index_entries(state_b, &mut all_paths);

        let mut results = Vec::new();
        let mut sorted_paths: Vec<_> = all_paths.into_iter().collect();
        sorted_paths.sort();

        for path in sorted_paths {
            // Check ignore first?
            // If path is ignored, we should ignore it unless it's in DB and we need to untrack?
            // If it's in DB but ignored now, Scan is None. State is Some.
            // We treat it as "Ignored".

            let path_ref = std::path::Path::new(&path);
            let included = filter.is_included(path_ref);
            let ignored = filter.is_ignored(path_ref) || !included;

            let change_a = Self::classify_side(
                &path,
                map_scan_a.get(&path),
                map_state_a.get(&path),
                ignored,
            );
            let change_b = Self::classify_side(
                &path,
                map_scan_b.get(&path),
                map_state_b.get(&path),
                ignored,
            );

            let action = if Self::is_directory_case(&change_a, &change_b) {
                Self::resolve_directory(&change_a, &change_b)
            } else {
                Self::resolve_conflict(&change_a, &change_b)
            };

            results.push(DiffResult {
                path,
                change_a,
                change_b,
                action,
            });
        }

        results
    }

    fn index_entries(
        entries: Vec<Entry>,
        all_paths: &mut HashSet<String>,
    ) -> HashMap<String, Entry> {
        let mut map = HashMap::new();
        for e in entries {
            all_paths.insert(e.path.clone());
            map.insert(e.path.clone(), e);
        }
        map
    }

    fn classify_side(
        _path: &str,
        current: Option<&Entry>,
        previous: Option<&Entry>,
        ignored: bool,
    ) -> SideChange {
        // If ignored, and Not in Scan (None), we treat as Unchanged/NoOp even if in State.
        // Effectively "Untracked".

        let (change, entry_now, entry_prev) = match (current, previous) {
            (Some(c), Some(p)) => {
                // If ignored but somehow in scan (scanner shouldn't return it),
                // we treat normally? Or force ignore?
                // Scanner should respect filter.

                if p.deleted && !c.deleted {
                    if p.deleted {
                        (ChangeType::Created, Some(c.clone()), Some(p.clone()))
                    } else if c.kind != p.kind {
                        (ChangeType::TypeChanged, Some(c.clone()), Some(p.clone()))
                    } else if Self::is_modified(c, p) {
                        (ChangeType::Modified, Some(c.clone()), Some(p.clone()))
                    } else {
                        (ChangeType::Unchanged, Some(c.clone()), Some(p.clone()))
                    }
                } else if !p.deleted && c.deleted {
                    panic!("Scan entry should not be deleted");
                } else if c.kind != p.kind {
                    (ChangeType::TypeChanged, Some(c.clone()), Some(p.clone()))
                } else if Self::is_modified(c, p) {
                    (ChangeType::Modified, Some(c.clone()), Some(p.clone()))
                } else {
                    (ChangeType::Unchanged, Some(c.clone()), Some(p.clone()))
                }
            }
            (Some(c), None) => (ChangeType::Created, Some(c.clone()), None),
            (None, Some(p)) => {
                if ignored {
                    // It's in state, not in scan, and is ignored.
                    // This means we started ignoring it.
                    // We should NOT mark as deleted. We mark as Unchanged (effectively).
                    // Or "Ignored".
                    // If we mark Unchanged, we keep it in DB? Or do we want to remove from DB?
                    // If we keep in DB, next sync will see (None, Some) again = Ignored.
                    // If we want to forgetting, we need a "Forget" action.
                    // For now, Unchanged prevents Deletion propagation.
                    (ChangeType::Unchanged, None, Some(p.clone()))
                } else if p.deleted {
                    (ChangeType::Unchanged, None, Some(p.clone()))
                } else {
                    (ChangeType::Deleted, None, Some(p.clone()))
                }
            }
            (None, None) => (ChangeType::Unchanged, None, None),
        };

        SideChange {
            change,
            entry_now,
            entry_prev,
        }
    }

    fn is_modified(c: &Entry, p: &Entry) -> bool {
        if c.kind != p.kind {
            return true;
        }

        match c.kind {
            EntryKind::File => match (&c.hash, &p.hash) {
                (Some(curr), Some(prev)) => curr != prev,
                _ => c.size != p.size || c.mtime != p.mtime || c.mode != p.mode,
            },
            EntryKind::Dir => false,
            EntryKind::Symlink => {
                c.link_target != p.link_target || c.mode != p.mode || c.size != p.size
            }
        }
    }

    fn entry_has_dir(entry: &SideChange) -> bool {
        entry
            .entry_now
            .as_ref()
            .map(|e| e.kind == EntryKind::Dir)
            .unwrap_or(false)
            || entry
                .entry_prev
                .as_ref()
                .map(|e| e.kind == EntryKind::Dir)
                .unwrap_or(false)
    }

    fn entry_has_non_dir(entry: &SideChange) -> bool {
        entry
            .entry_now
            .as_ref()
            .map(|e| e.kind != EntryKind::Dir)
            .unwrap_or(false)
            || entry
                .entry_prev
                .as_ref()
                .map(|e| e.kind != EntryKind::Dir)
                .unwrap_or(false)
    }

    fn is_directory_case(a: &SideChange, b: &SideChange) -> bool {
        let has_dir = Self::entry_has_dir(a) || Self::entry_has_dir(b);
        let has_non_dir = Self::entry_has_non_dir(a) || Self::entry_has_non_dir(b);
        has_dir && !has_non_dir
    }

    fn resolve_directory(a: &SideChange, b: &SideChange) -> SyncAction {
        use ChangeType::*;
        let a_exists = a
            .entry_now
            .as_ref()
            .map(|e| e.kind == EntryKind::Dir)
            .unwrap_or(false);
        let b_exists = b
            .entry_now
            .as_ref()
            .map(|e| e.kind == EntryKind::Dir)
            .unwrap_or(false);

        match (&a.change, &b.change) {
            (Deleted, Deleted) => return SyncAction::NoOp,
            (Deleted, _) => return SyncAction::DeleteB,
            (_, Deleted) => return SyncAction::DeleteA,
            _ => {}
        }

        match (a_exists, b_exists) {
            (true, false) => SyncAction::CopyAtoB,
            (false, true) => SyncAction::CopyBtoA,
            _ => SyncAction::NoOp,
        }
    }

    fn resolve_conflict(a: &SideChange, b: &SideChange) -> SyncAction {
        use ChangeType::*;

        match (&a.change, &b.change) {
            (Unchanged, Unchanged) => SyncAction::NoOp,

            (Created, Unchanged) => SyncAction::CopyAtoB,
            (Modified, Unchanged) => SyncAction::CopyAtoB,
            (Deleted, Unchanged) => SyncAction::DeleteB,
            (Unchanged, Created) => SyncAction::CopyBtoA,
            (Unchanged, Modified) => SyncAction::CopyBtoA,
            (Unchanged, Deleted) => SyncAction::DeleteA,

            (Created, Created) => {
                if Self::entries_match(a, b) {
                    SyncAction::NoOp
                } else {
                    SyncAction::Conflict(ConflictReason::BothModified)
                }
            }
            (Created, Modified) => SyncAction::Conflict(ConflictReason::TypeMismatch),
            (Modified, Modified) => {
                if Self::entries_match(a, b) {
                    SyncAction::NoOp
                } else {
                    SyncAction::Conflict(ConflictReason::BothModified)
                }
            }
            (Deleted, Deleted) => SyncAction::NoOp,

            (Created, Deleted) => SyncAction::Conflict(ConflictReason::DeleteVsModify),
            (Deleted, Created) => SyncAction::Conflict(ConflictReason::DeleteVsModify),

            (Modified, Deleted) => SyncAction::Conflict(ConflictReason::DeleteVsModify),
            (Deleted, Modified) => SyncAction::Conflict(ConflictReason::DeleteVsModify),

            (TypeChanged, _) | (_, TypeChanged) => {
                SyncAction::Conflict(ConflictReason::TypeMismatch)
            }
            (Modified, Created) => SyncAction::Conflict(ConflictReason::BothModified),
        }
    }

    fn entries_match(a: &SideChange, b: &SideChange) -> bool {
        match (&a.entry_now, &b.entry_now) {
            (Some(ea), Some(eb)) => ea.kind == eb.kind && !Self::is_modified(ea, eb),
            _ => false,
        }
    }
}
