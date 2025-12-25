use crate::diff::{DiffResult, SyncAction};
use crate::roots::EntryKind;
use crate::scan::{Entry as ScanEntry, HardlinkGroups};
use crate::state::Entry;
use std::collections::{HashMap, HashSet};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CopyDirection {
    AtoB,
    BtoA,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DeleteSide {
    RootA,
    RootB,
}

#[derive(Debug, Clone)]
pub struct DeleteOp {
    pub path: String,
    pub kind: EntryKind,
}

#[derive(Debug, Clone)]
pub struct LinkOp {
    pub path: String,
    pub target: String,
}

#[derive(Debug, Default)]
pub struct Plan {
    pub copy_a_to_b: Vec<Entry>,
    pub copy_b_to_a: Vec<Entry>,
    pub delete_a: Vec<DeleteOp>,
    pub delete_b: Vec<DeleteOp>,
    pub hardlink_a_to_b: Vec<LinkOp>,
    pub hardlink_b_to_a: Vec<LinkOp>,
    pub conflicts: Vec<DiffResult>,
}

impl Plan {
    pub fn total_operations(&self) -> usize {
        self.copy_a_to_b.len()
            + self.copy_b_to_a.len()
            + self.delete_a.len()
            + self.delete_b.len()
            + self.hardlink_a_to_b.len()
            + self.hardlink_b_to_a.len()
    }

    pub fn add_copy(&mut self, direction: CopyDirection, entry: Entry) {
        match direction {
            CopyDirection::AtoB => self.copy_a_to_b.push(entry),
            CopyDirection::BtoA => self.copy_b_to_a.push(entry),
        }
    }

    pub fn add_delete(&mut self, side: DeleteSide, op: DeleteOp) {
        match side {
            DeleteSide::RootA => self.delete_a.push(op),
            DeleteSide::RootB => self.delete_b.push(op),
        }
    }

    pub fn add_link(&mut self, direction: CopyDirection, op: LinkOp) {
        match direction {
            CopyDirection::AtoB => self.hardlink_a_to_b.push(op),
            CopyDirection::BtoA => self.hardlink_b_to_a.push(op),
        }
    }
}

pub struct PlanBuilder;

impl PlanBuilder {
    pub fn build(diffs: Vec<DiffResult>) -> Plan {
        let mut plan = Plan::default();

        for diff in diffs {
            match diff.action {
                SyncAction::NoOp => {}
                SyncAction::Conflict(_) => plan.conflicts.push(diff),
                SyncAction::CopyAtoB => {
                    if let Some(entry) = diff.change_a.entry_now.clone() {
                        plan.copy_a_to_b.push(entry.to_state());
                    }
                }
                SyncAction::CopyBtoA => {
                    if let Some(entry) = diff.change_b.entry_now.clone() {
                        plan.copy_b_to_a.push(entry.to_state());
                    }
                }
                SyncAction::DeleteA => {
                    plan.delete_a.push(DeleteOp {
                        path: diff.path,
                        kind: diff
                            .change_b
                            .entry_prev
                            .as_ref()
                            .map(|e| e.kind)
                            .unwrap_or(EntryKind::File),
                    });
                }
                SyncAction::DeleteB => {
                    plan.delete_b.push(DeleteOp {
                        path: diff.path,
                        kind: diff
                            .change_a
                            .entry_prev
                            .as_ref()
                            .map(|e| e.kind)
                            .unwrap_or(EntryKind::File),
                    });
                }
            }
        }

        plan.copy_a_to_b.sort_by(|a, b| a.path.cmp(&b.path));
        plan.copy_b_to_a.sort_by(|a, b| a.path.cmp(&b.path));
        sort_deletes(&mut plan.delete_a);
        sort_deletes(&mut plan.delete_b);

        plan
    }
}

pub fn apply_hardlink_preserve(
    plan: &mut Plan,
    diffs: &[DiffResult],
    groups_a: &HardlinkGroups,
    groups_b: &HardlinkGroups,
    scan_a: &[ScanEntry],
    scan_b: &[ScanEntry],
    allow_copy_a_to_b: bool,
    allow_copy_b_to_a: bool,
) {
    let diff_map = diffs
        .iter()
        .map(|diff| (diff.path.as_str(), diff))
        .collect::<HashMap<_, _>>();

    let scan_a_map = scan_a
        .iter()
        .map(|entry| (entry.path.as_str(), entry))
        .collect::<HashMap<_, _>>();
    let scan_b_map = scan_b
        .iter()
        .map(|entry| (entry.path.as_str(), entry))
        .collect::<HashMap<_, _>>();

    let mut copy_a_set: HashSet<String> =
        plan.copy_a_to_b.iter().map(|e| e.path.clone()).collect();
    let mut copy_b_set: HashSet<String> =
        plan.copy_b_to_a.iter().map(|e| e.path.clone()).collect();
    let delete_a_set: HashSet<String> =
        plan.delete_a.iter().map(|e| e.path.clone()).collect();
    let delete_b_set: HashSet<String> =
        plan.delete_b.iter().map(|e| e.path.clone()).collect();

    if allow_copy_a_to_b {
        apply_hardlink_direction(
            plan,
            &diff_map,
            &scan_a_map,
            groups_a,
            CopyDirection::AtoB,
            &mut copy_a_set,
            &delete_b_set,
        );
    }

    if allow_copy_b_to_a {
        apply_hardlink_direction(
            plan,
            &diff_map,
            &scan_b_map,
            groups_b,
            CopyDirection::BtoA,
            &mut copy_b_set,
            &delete_a_set,
        );
    }

    plan.copy_a_to_b.sort_by(|a, b| a.path.cmp(&b.path));
    plan.copy_b_to_a.sort_by(|a, b| a.path.cmp(&b.path));
    plan.hardlink_a_to_b
        .sort_by(|a, b| a.path.cmp(&b.path));
    plan.hardlink_b_to_a
        .sort_by(|a, b| a.path.cmp(&b.path));
}

fn apply_hardlink_direction(
    plan: &mut Plan,
    diff_map: &HashMap<&str, &DiffResult>,
    scan_map: &HashMap<&str, &ScanEntry>,
    groups: &HardlinkGroups,
    direction: CopyDirection,
    copy_set: &mut HashSet<String>,
    delete_set: &HashSet<String>,
) {
    let mut remove_copy: HashSet<String> = HashSet::new();
    let mut add_copy: Vec<Entry> = Vec::new();
    let mut link_ops: Vec<LinkOp> = Vec::new();

    for paths in groups.values() {
        let mut sorted = paths.clone();
        sorted.sort();
        if sorted.is_empty() {
            continue;
        }

        let mut primary = None;
        for path in &sorted {
            if dest_will_exist(path, diff_map, copy_set, delete_set, direction) {
                primary = Some(path.clone());
                break;
            }
        }

        let primary = match primary {
            Some(path) => path,
            None => {
                let candidate = sorted[0].clone();
                if let Some(entry) = scan_map.get(candidate.as_str()) {
                    if !copy_set.contains(&candidate) {
                        add_copy.push(entry.to_state());
                        copy_set.insert(candidate.clone());
                    }
                    candidate
                } else {
                    continue;
                }
            }
        };

        for path in &sorted {
            if path == &primary {
                continue;
            }
            link_ops.push(LinkOp {
                path: path.clone(),
                target: primary.clone(),
            });
            remove_copy.insert(path.clone());
        }
    }

    if !remove_copy.is_empty() {
        match direction {
            CopyDirection::AtoB => {
                plan.copy_a_to_b.retain(|entry| !remove_copy.contains(&entry.path));
            }
            CopyDirection::BtoA => {
                plan.copy_b_to_a.retain(|entry| !remove_copy.contains(&entry.path));
            }
        }
    }

    if !add_copy.is_empty() {
        match direction {
            CopyDirection::AtoB => plan.copy_a_to_b.extend(add_copy),
            CopyDirection::BtoA => plan.copy_b_to_a.extend(add_copy),
        }
    }

    if !link_ops.is_empty() {
        for op in link_ops {
            plan.add_link(direction, op);
        }
    }
}

fn dest_will_exist(
    path: &str,
    diff_map: &HashMap<&str, &DiffResult>,
    copy_set: &HashSet<String>,
    delete_set: &HashSet<String>,
    direction: CopyDirection,
) -> bool {
    if delete_set.contains(path) {
        return false;
    }
    if copy_set.contains(path) {
        return true;
    }
    diff_map
        .get(path)
        .and_then(|diff| match direction {
            CopyDirection::AtoB => diff.change_b.entry_prev.as_ref(),
            CopyDirection::BtoA => diff.change_a.entry_prev.as_ref(),
        })
        .map(|entry| !entry.deleted)
        .unwrap_or(false)
}

fn sort_deletes(ops: &mut Vec<DeleteOp>) {
    ops.sort_by(|a, b| {
        let depth_a = delete_depth(&a.path);
        let depth_b = delete_depth(&b.path);
        depth_b.cmp(&depth_a).then_with(|| a.path.cmp(&b.path))
    });
}

fn delete_depth(path: &str) -> usize {
    path.split('/').filter(|seg| !seg.is_empty()).count()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::scan;

    #[test]
    fn delete_order_children_before_parents() {
        let mut ops = vec![
            DeleteOp {
                path: "a".to_string(),
                kind: EntryKind::Dir,
            },
            DeleteOp {
                path: "a/b".to_string(),
                kind: EntryKind::Dir,
            },
            DeleteOp {
                path: "a/b/c.txt".to_string(),
                kind: EntryKind::File,
            },
            DeleteOp {
                path: "x".to_string(),
                kind: EntryKind::Dir,
            },
            DeleteOp {
                path: "x/y".to_string(),
                kind: EntryKind::File,
            },
        ];

        sort_deletes(&mut ops);

        let ordered: Vec<String> = ops.into_iter().map(|op| op.path).collect();
        assert_eq!(
            ordered,
            vec![
                "a/b/c.txt".to_string(),
                "a/b".to_string(),
                "x/y".to_string(),
                "a".to_string(),
                "x".to_string(),
            ]
        );
    }

    #[test]
    fn hardlink_preserve_links_secondary_paths() {
        let scan_a = vec![
            ScanEntry {
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
            ScanEntry {
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
        ];
        let scan_b = Vec::new();
        let groups_a = scan::hardlink_groups(&scan_a);
        let groups_b = scan::hardlink_groups(&scan_b);

        let mut plan = Plan::default();
        plan.copy_a_to_b.push(scan_a[1].to_state());
        plan.copy_a_to_b.push(scan_a[0].to_state());

        apply_hardlink_preserve(
            &mut plan,
            &[],
            &groups_a,
            &groups_b,
            &scan_a,
            &scan_b,
            true,
            false,
        );

        assert_eq!(plan.copy_a_to_b.len(), 1);
        assert_eq!(plan.hardlink_a_to_b.len(), 1);
        let link = &plan.hardlink_a_to_b[0];
        assert_eq!(link.target, "a.txt");
        assert_eq!(link.path, "b.txt");
    }
}
