use crate::diff::{DiffResult, SyncAction};
use crate::roots::EntryKind;
use crate::state::Entry;

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

#[derive(Debug, Default)]
pub struct Plan {
    pub copy_a_to_b: Vec<Entry>,
    pub copy_b_to_a: Vec<Entry>,
    pub delete_a: Vec<DeleteOp>,
    pub delete_b: Vec<DeleteOp>,
    pub conflicts: Vec<DiffResult>,
}

impl Plan {
    pub fn total_operations(&self) -> usize {
        self.copy_a_to_b.len() + self.copy_b_to_a.len() + self.delete_a.len() + self.delete_b.len()
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
                        plan.copy_a_to_b.push(entry);
                    }
                }
                SyncAction::CopyBtoA => {
                    if let Some(entry) = diff.change_b.entry_now.clone() {
                        plan.copy_b_to_a.push(entry);
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
}
