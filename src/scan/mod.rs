mod filter;
mod local;
mod remote;

use crate::roots::EntryKind;
use std::collections::HashMap;

pub use filter::Filter;
pub use local::LocalScanner;
pub use remote::RemoteScanner;

#[derive(Debug, Clone)]
pub struct Entry {
    pub path: String,
    pub kind: EntryKind,
    pub size: u64,
    pub mtime: i64,
    pub mode: u32,
    pub nlink: u64,
    pub dev: u64,
    pub inode: u64,
    pub hash: Option<Vec<u8>>,
    pub link_target: Option<String>,
}

impl Entry {
    pub fn to_state(&self) -> crate::state::Entry {
        crate::state::Entry {
            path: self.path.clone(),
            kind: self.kind,
            size: self.size,
            mtime: self.mtime,
            mode: self.mode,
            hash: self.hash.clone(),
            link_target: self.link_target.clone(),
            deleted: false,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct HardlinkKey {
    pub dev: u64,
    pub inode: u64,
}

pub type HardlinkGroups = HashMap<HardlinkKey, Vec<String>>;

pub fn hardlink_groups(entries: &[Entry]) -> HardlinkGroups {
    let mut groups: HardlinkGroups = HashMap::new();
    for entry in entries {
        if entry.kind != EntryKind::File || entry.nlink <= 1 {
            continue;
        }
        if entry.dev == 0 || entry.inode == 0 {
            continue;
        }
        let key = HardlinkKey {
            dev: entry.dev,
            inode: entry.inode,
        };
        groups
            .entry(key)
            .or_insert_with(Vec::new)
            .push(entry.path.clone());
    }
    groups.retain(|_, paths| paths.len() > 1);
    groups
}

pub fn has_missing_inode(entries: &[Entry]) -> bool {
    entries.iter().any(|entry| {
        entry.kind == EntryKind::File && entry.nlink > 1 && (entry.dev == 0 || entry.inode == 0)
    })
}

#[cfg(test)]
mod tests;
