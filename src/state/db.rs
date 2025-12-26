use crate::plan::{CopyDirection, DeleteOp, DeleteSide, Plan};
use crate::roots::EntryKind;
use anyhow::{Context, Result};
use rusqlite::{params, params_from_iter, Connection, OpenFlags, OptionalExtension};
use std::path::Path;

#[derive(Debug, Clone)]
pub struct Entry {
    pub path: String,
    pub kind: EntryKind,
    pub size: u64,
    pub mtime: i64,
    pub mode: u32,
    pub hash: Option<Vec<u8>>,
    pub link_target: Option<String>,
    pub deleted: bool,
}

pub struct StateDb {
    conn: Connection,
}

#[derive(Debug, Clone)]
pub struct PendingCopy {
    pub id: i64,
    pub entry: Entry,
}

#[derive(Debug, Clone)]
pub struct PendingDelete {
    pub id: i64,
    pub path: String,
    pub kind: EntryKind,
}

#[derive(Debug, Clone)]
pub struct PendingLink {
    pub id: i64,
    pub path: String,
    pub target: String,
}

#[derive(Debug, Clone, Copy, Default)]
pub struct CopyMetrics {
    pub entries: usize,
    pub file_bytes: u64,
    pub work_units: u64,
}

impl StateDb {
    pub fn open(path: impl AsRef<Path>) -> Result<Self> {
        let conn = Connection::open(path)?;
        let db = Self { conn };
        db.init()?;
        Ok(db)
    }

    pub fn open_readonly(path: impl AsRef<Path>) -> Result<Self> {
        let conn = Connection::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_ONLY)?;
        Ok(Self { conn })
    }

    pub fn copy_metrics(&self, direction: CopyDirection) -> Result<CopyMetrics> {
        let mut stmt = self.conn.prepare(
            "SELECT 
                COUNT(*) as cnt,
                COALESCE(SUM(CASE WHEN kind = 0 THEN size ELSE 0 END), 0) as file_bytes,
                COALESCE(SUM(
                    CASE 
                        WHEN kind = 0 THEN CASE WHEN size > 0 THEN size ELSE 1 END
                        ELSE 1
                    END
                ), 0) as work_units
             FROM pending_copy_ops
             WHERE direction = ?1",
        )?;

        let metrics = stmt.query_row(params![copy_direction_to_int(direction)], |row| {
            Ok(CopyMetrics {
                entries: row.get::<_, i64>(0)? as usize,
                file_bytes: row.get::<_, i64>(1)? as u64,
                work_units: row.get::<_, i64>(2)? as u64,
            })
        })?;
        Ok(metrics)
    }

    pub fn pending_delete_count(&self, side: DeleteSide) -> Result<usize> {
        let mut stmt = self
            .conn
            .prepare("SELECT COUNT(*) FROM pending_delete_ops WHERE side = ?1")?;
        let count = stmt.query_row(params![delete_side_to_int(side)], |row| {
            let val: i64 = row.get(0)?;
            Ok(val as usize)
        })?;
        Ok(count)
    }

    pub fn pending_link_count(&self, direction: CopyDirection) -> Result<usize> {
        let mut stmt = self
            .conn
            .prepare("SELECT COUNT(*) FROM pending_link_ops WHERE direction = ?1")?;
        let count = stmt.query_row(params![copy_direction_to_int(direction)], |row| {
            let val: i64 = row.get(0)?;
            Ok(val as usize)
        })?;
        Ok(count)
    }

    #[cfg_attr(not(test), allow(dead_code))]
    pub fn open_memory() -> Result<Self> {
        let conn = Connection::open_in_memory()?;
        let db = Self { conn };
        db.init()?;
        Ok(db)
    }

    fn init(&self) -> Result<()> {
        self.conn.execute(
            "CREATE TABLE IF NOT EXISTS meta (
                key TEXT PRIMARY KEY,
                value TEXT NOT NULL
            )",
            [],
        )?;

        self.conn.execute(
            "CREATE TABLE IF NOT EXISTS entries (
                path TEXT PRIMARY KEY,
                kind INTEGER NOT NULL,
                size INTEGER NOT NULL,
                mtime INTEGER NOT NULL,
                mode INTEGER NOT NULL,
                hash BLOB,
                link_target TEXT,
                deleted INTEGER NOT NULL DEFAULT 0
            )",
            [],
        )?;

        self.conn.execute(
            "CREATE INDEX IF NOT EXISTS idx_entries_deleted ON entries(deleted)",
            [],
        )?;

        self.conn.execute(
            "CREATE TABLE IF NOT EXISTS pending_copy_ops (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                direction INTEGER NOT NULL,
                path TEXT NOT NULL,
                kind INTEGER NOT NULL,
                size INTEGER NOT NULL,
                mtime INTEGER NOT NULL,
                mode INTEGER NOT NULL,
                hash BLOB,
                link_target TEXT
            )",
            [],
        )?;

        self.conn.execute(
            "CREATE TABLE IF NOT EXISTS pending_delete_ops (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                side INTEGER NOT NULL,
                path TEXT NOT NULL,
                kind INTEGER NOT NULL
            )",
            [],
        )?;

        self.conn.execute(
            "CREATE TABLE IF NOT EXISTS pending_link_ops (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                direction INTEGER NOT NULL,
                path TEXT NOT NULL,
                target TEXT NOT NULL
            )",
            [],
        )?;

        Ok(())
    }

    #[cfg_attr(not(test), allow(dead_code))]
    pub fn get_meta(&self, key: &str) -> Result<Option<String>> {
        let mut stmt = self.conn.prepare("SELECT value FROM meta WHERE key = ?1")?;
        stmt.query_row(params![key], |row| row.get(0))
            .optional()
            .map_err(Into::into)
    }

    #[cfg_attr(not(test), allow(dead_code))]
    pub fn set_meta(&self, key: &str, value: &str) -> Result<()> {
        self.conn.execute(
            "INSERT OR REPLACE INTO meta (key, value) VALUES (?1, ?2)",
            params![key, value],
        )?;
        Ok(())
    }

    #[cfg_attr(not(test), allow(dead_code))]
    pub fn get_entry(&self, path: &str) -> Result<Option<Entry>> {
        let mut stmt = self.conn.prepare(
            "SELECT path, kind, size, mtime, mode, hash, link_target, deleted FROM entries WHERE path = ?1"
        )?;

        let entry = stmt
            .query_row(params![path], |row| {
                let kind_int: i32 = row.get(1)?;
                let kind = match kind_int {
                    1 => EntryKind::Dir,
                    2 => EntryKind::Symlink,
                    _ => EntryKind::File,
                };

                Ok(Entry {
                    path: row.get(0)?,
                    kind,
                    size: row.get(2)?,
                    mtime: row.get(3)?,
                    mode: row.get(4)?,
                    hash: row.get(5)?,
                    link_target: row.get(6)?,
                    deleted: row.get::<_, i32>(7)? != 0,
                })
            })
            .optional()?;

        Ok(entry)
    }

    #[cfg_attr(not(test), allow(dead_code))]
    pub fn upsert_entry(&self, entry: &Entry) -> Result<()> {
        let kind_int = match entry.kind {
            EntryKind::File => 0,
            EntryKind::Dir => 1,
            EntryKind::Symlink => 2,
        };

        self.conn.execute(
            "INSERT OR REPLACE INTO entries 
            (path, kind, size, mtime, mode, hash, link_target, deleted) 
            VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
            params![
                entry.path,
                kind_int,
                entry.size,
                entry.mtime,
                entry.mode,
                entry.hash,
                entry.link_target,
                if entry.deleted { 1 } else { 0 }
            ],
        )?;
        Ok(())
    }

    pub fn delete_entry(&self, path: &str) -> Result<()> {
        self.conn
            .execute("DELETE FROM entries WHERE path = ?1", params![path])?;
        Ok(())
    }

    pub fn list_entries(&self) -> Result<Vec<Entry>> {
        let mut stmt = self.conn.prepare(
            "SELECT path, kind, size, mtime, mode, hash, link_target, deleted FROM entries",
        )?;

        let rows = stmt.query_map([], |row| {
            let kind_int: i32 = row.get(1)?;
            let kind = match kind_int {
                1 => EntryKind::Dir,
                2 => EntryKind::Symlink,
                _ => EntryKind::File,
            };
            Ok(Entry {
                path: row.get(0)?,
                kind,
                size: row.get(2)?,
                mtime: row.get(3)?,
                mode: row.get(4)?,
                hash: row.get(5)?,
                link_target: row.get(6)?,
                deleted: row.get::<_, i32>(7)? != 0,
            })
        })?;

        let mut entries = Vec::new();
        for r in rows {
            entries.push(r?);
        }
        Ok(entries)
    }

    pub fn queue_plan(&self, plan: &Plan) -> Result<()> {
        self.conn.execute("BEGIN TRANSACTION", [])?;
        let result: Result<()> = (|| {
            self.conn.execute("DELETE FROM pending_copy_ops", [])?;
            self.conn.execute("DELETE FROM pending_delete_ops", [])?;
            self.conn.execute("DELETE FROM pending_link_ops", [])?;

            for entry in &plan.copy_a_to_b {
                self.insert_pending_copy(CopyDirection::AtoB, entry)?;
            }
            for entry in &plan.copy_b_to_a {
                self.insert_pending_copy(CopyDirection::BtoA, entry)?;
            }
            for del in &plan.delete_a {
                self.insert_pending_delete(DeleteSide::RootA, del)?;
            }
            for del in &plan.delete_b {
                self.insert_pending_delete(DeleteSide::RootB, del)?;
            }
            for link in &plan.hardlink_a_to_b {
                self.insert_pending_link(CopyDirection::AtoB, link)?;
            }
            for link in &plan.hardlink_b_to_a {
                self.insert_pending_link(CopyDirection::BtoA, link)?;
            }
            Ok(())
        })();

        if let Err(err) = result {
            let _ = self.conn.execute("ROLLBACK", []);
            return Err(err);
        }

        if let Err(err) = self.conn.execute("COMMIT", []) {
            let _ = self.conn.execute("ROLLBACK", []);
            return Err(err.into());
        }
        Ok(())
    }

    fn insert_pending_copy(&self, direction: CopyDirection, entry: &Entry) -> Result<()> {
        insert_pending_copy_conn(&self.conn, direction, entry)
    }

    fn insert_pending_delete(&self, side: DeleteSide, op: &DeleteOp) -> Result<()> {
        insert_pending_delete_conn(&self.conn, side, op)
    }

    fn insert_pending_link(&self, direction: CopyDirection, link: &crate::plan::LinkOp) -> Result<()> {
        insert_pending_link_conn(&self.conn, direction, link)
    }

    pub fn refresh_metadata(&self, entries: &[Entry]) -> Result<()> {
        if entries.is_empty() {
            return Ok(());
        }
        self.conn.execute("BEGIN TRANSACTION", [])?;
        let mut stmt = self.conn.prepare(
            "UPDATE entries
             SET size = ?2, mtime = ?3, mode = ?4, hash = ?5, deleted = 0
             WHERE path = ?1 AND hash IS NOT NULL AND hash = ?5",
        )?;
        for entry in entries {
            if entry.kind != EntryKind::File {
                continue;
            }
            if let Some(hash) = &entry.hash {
                if let Err(err) = stmt.execute(params![
                    entry.path,
                    entry.size,
                    entry.mtime,
                    entry.mode,
                    hash
                ]) {
                    let _ = self.conn.execute("ROLLBACK", []);
                    return Err(err.into());
                }
            }
        }
        self.conn.execute("COMMIT", [])?;
        Ok(())
    }

    pub fn fetch_pending_copies(
        &self,
        direction: CopyDirection,
        limit: usize,
    ) -> Result<Vec<PendingCopy>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, path, kind, size, mtime, mode, hash, link_target
             FROM pending_copy_ops
             WHERE direction = ?1
             ORDER BY size DESC, path ASC
             LIMIT ?2",
        )?;

        let rows = stmt.query_map(
            params![copy_direction_to_int(direction), limit as i64],
            |row| {
                let kind_int: i32 = row.get(2)?;
                let kind = match kind_int {
                    1 => EntryKind::Dir,
                    2 => EntryKind::Symlink,
                    _ => EntryKind::File,
                };
                Ok(PendingCopy {
                    id: row.get(0)?,
                    entry: Entry {
                        path: row.get(1)?,
                        kind,
                        size: row.get(3)?,
                        mtime: row.get(4)?,
                        mode: row.get(5)?,
                        hash: row.get(6)?,
                        link_target: row.get(7)?,
                        deleted: false,
                    },
                })
            },
        )?;

        let mut copies = Vec::new();
        for r in rows {
            copies.push(r?);
        }
        Ok(copies)
    }

    pub fn fetch_pending_deletes(
        &self,
        side: DeleteSide,
        limit: usize,
    ) -> Result<Vec<PendingDelete>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, path, kind FROM pending_delete_ops
             WHERE side = ?1
             ORDER BY id
             LIMIT ?2",
        )?;
        let rows = stmt.query_map(params![delete_side_to_int(side), limit as i64], |row| {
            let kind_int: i32 = row.get(2)?;
            let kind = match kind_int {
                1 => EntryKind::Dir,
                2 => EntryKind::Symlink,
                _ => EntryKind::File,
            };
            Ok(PendingDelete {
                id: row.get(0)?,
                path: row.get(1)?,
                kind,
            })
        })?;

        let mut deletes = Vec::new();
        for r in rows {
            deletes.push(r?);
        }
        Ok(deletes)
    }

    pub fn fetch_pending_links(
        &self,
        direction: CopyDirection,
        limit: usize,
    ) -> Result<Vec<PendingLink>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, path, target FROM pending_link_ops
             WHERE direction = ?1
             ORDER BY id
             LIMIT ?2",
        )?;
        let rows = stmt.query_map(
            params![copy_direction_to_int(direction), limit as i64],
            |row| {
                Ok(PendingLink {
                    id: row.get(0)?,
                    path: row.get(1)?,
                    target: row.get(2)?,
                })
            },
        )?;

        let mut links = Vec::new();
        for r in rows {
            links.push(r?);
        }
        Ok(links)
    }

    pub fn complete_pending_copies(&self, copies: &[PendingCopy]) -> Result<()> {
        if copies.is_empty() {
            return Ok(());
        }
        self.conn.execute("BEGIN TRANSACTION", [])?;
        if let Err(err) = insert_entries_from_pending(&self.conn, copies) {
            let _ = self.conn.execute("ROLLBACK", []);
            return Err(err);
        }
        if let Err(err) = delete_by_ids(&self.conn, "pending_copy_ops", copies.iter().map(|c| c.id))
        {
            let _ = self.conn.execute("ROLLBACK", []);
            return Err(err);
        }
        self.conn.execute("COMMIT", [])?;
        Ok(())
    }

    pub fn complete_pending_deletes(&self, deletes: &[PendingDelete]) -> Result<()> {
        if deletes.is_empty() {
            return Ok(());
        }
        self.conn.execute("BEGIN TRANSACTION", [])?;
        let result: Result<()> = (|| {
            for del in deletes {
                self.delete_entry(&del.path)?;
            }
            delete_by_ids(
                &self.conn,
                "pending_delete_ops",
                deletes.iter().map(|d| d.id),
            )?;
            Ok(())
        })();

        if let Err(err) = result {
            let _ = self.conn.execute("ROLLBACK", []);
            return Err(err);
        }

        if let Err(err) = self.conn.execute("COMMIT", []) {
            let _ = self.conn.execute("ROLLBACK", []);
            return Err(err.into());
        }
        Ok(())
    }

    pub fn complete_pending_links(&self, links: &[PendingLink]) -> Result<()> {
        if links.is_empty() {
            return Ok(());
        }
        self.conn.execute("BEGIN TRANSACTION", [])?;
        let result: Result<()> = (|| {
            for link in links {
                if link.path == link.target {
                    continue;
                }
                let mut entry = self
                    .get_entry(&link.target)?
                    .with_context(|| format!("Link target missing in state: {}", link.target))?;
                if entry.deleted {
                    anyhow::bail!("Link target marked deleted in state: {}", link.target);
                }
                entry.path = link.path.clone();
                entry.deleted = false;
                self.upsert_entry(&entry)?;
            }
            delete_by_ids(&self.conn, "pending_link_ops", links.iter().map(|l| l.id))?;
            Ok(())
        })();

        if let Err(err) = result {
            let _ = self.conn.execute("ROLLBACK", []);
            return Err(err);
        }

        self.conn.execute("COMMIT", [])?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::plan::LinkOp;

    #[test]
    fn test_db_operations() -> Result<()> {
        let db = StateDb::open_memory()?;

        // Meta
        db.set_meta("foo", "bar")?;
        assert_eq!(db.get_meta("foo")?, Some("bar".to_string()));
        assert_eq!(db.get_meta("baz")?, None);

        // Entry
        let entry = Entry {
            path: "a/b.txt".to_string(),
            kind: EntryKind::File,
            size: 100,
            mtime: 123456789,
            mode: 0o644,
            hash: Some(vec![0, 1, 2, 3]),
            link_target: None,
            deleted: false,
        };

        db.upsert_entry(&entry)?;

        let fetched = db.get_entry("a/b.txt")?.unwrap();
        assert_eq!(fetched.path, "a/b.txt");
        assert_eq!(fetched.kind, EntryKind::File);
        assert_eq!(fetched.hash, Some(vec![0, 1, 2, 3]));

        // Update to deleted
        let mut deleted_entry = entry.clone();
        deleted_entry.deleted = true;
        db.upsert_entry(&deleted_entry)?;

        let fetched_del = db.get_entry("a/b.txt")?.unwrap();
        assert!(fetched_del.deleted);

        // Refresh metadata when hash matches
        let mut refreshed = entry.clone();
        refreshed.mtime += 10;
        refreshed.mode = 0o600;
        let mut refresh_entry = refreshed.clone();
        refresh_entry.hash = Some(vec![0, 1, 2, 3]);
        db.refresh_metadata(&[refresh_entry])?;
        let updated = db.get_entry("a/b.txt")?.unwrap();
        assert_eq!(updated.mtime, refreshed.mtime);
        assert_eq!(updated.mode, refreshed.mode);

        Ok(())
    }

    #[test]
    fn link_ops_clone_target_entry() -> Result<()> {
        let db = StateDb::open_memory()?;
        let entry = Entry {
            path: "target.txt".to_string(),
            kind: EntryKind::File,
            size: 42,
            mtime: 123,
            mode: 0o644,
            hash: None,
            link_target: None,
            deleted: false,
        };
        db.upsert_entry(&entry)?;

        let mut plan = Plan::default();
        plan.hardlink_a_to_b.push(LinkOp {
            path: "linked.txt".to_string(),
            target: "target.txt".to_string(),
        });
        db.queue_plan(&plan)?;

        let pending = db.fetch_pending_links(CopyDirection::AtoB, 10)?;
        assert_eq!(pending.len(), 1);
        db.complete_pending_links(&pending)?;

        let linked = db.get_entry("linked.txt")?.expect("linked entry");
        assert_eq!(linked.kind, EntryKind::File);
        assert_eq!(linked.size, 42);
        assert_eq!(linked.mtime, 123);
        assert_eq!(linked.mode, 0o644);
        Ok(())
    }
}

fn kind_to_int(kind: EntryKind) -> i32 {
    match kind {
        EntryKind::File => 0,
        EntryKind::Dir => 1,
        EntryKind::Symlink => 2,
    }
}

fn insert_pending_copy_conn(conn: &Connection, direction: CopyDirection, entry: &Entry) -> Result<()> {
    conn.execute(
        "INSERT INTO pending_copy_ops
            (direction, path, kind, size, mtime, mode, hash, link_target)
            VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
        params![
            copy_direction_to_int(direction),
            entry.path,
            kind_to_int(entry.kind),
            entry.size,
            entry.mtime,
            entry.mode,
            entry.hash,
            entry.link_target
        ],
    )?;
    Ok(())
}

fn insert_pending_delete_conn(conn: &Connection, side: DeleteSide, op: &DeleteOp) -> Result<()> {
    conn.execute(
        "INSERT INTO pending_delete_ops (side, path, kind) VALUES (?1, ?2, ?3)",
        params![delete_side_to_int(side), op.path, kind_to_int(op.kind)],
    )?;
    Ok(())
}

fn insert_pending_link_conn(
    conn: &Connection,
    direction: CopyDirection,
    link: &crate::plan::LinkOp,
) -> Result<()> {
    conn.execute(
        "INSERT INTO pending_link_ops (direction, path, target) VALUES (?1, ?2, ?3)",
        params![copy_direction_to_int(direction), link.path, link.target],
    )?;
    Ok(())
}

fn copy_direction_to_int(direction: CopyDirection) -> i32 {
    match direction {
        CopyDirection::AtoB => 0,
        CopyDirection::BtoA => 1,
    }
}

fn delete_side_to_int(side: DeleteSide) -> i32 {
    match side {
        DeleteSide::RootA => 0,
        DeleteSide::RootB => 1,
    }
}

fn delete_by_ids<I>(conn: &Connection, table: &str, ids: I) -> Result<()>
where
    I: IntoIterator<Item = i64>,
{
    let ids_vec: Vec<i64> = ids.into_iter().collect();
    if ids_vec.is_empty() {
        return Ok(());
    }
    let placeholders = vec!["?"; ids_vec.len()].join(",");
    let sql = format!("DELETE FROM {} WHERE id IN ({})", table, placeholders);
    conn.execute(&sql, params_from_iter(ids_vec.iter()))?;
    Ok(())
}

fn insert_entries_from_pending(conn: &Connection, copies: &[PendingCopy]) -> Result<()> {
    if copies.is_empty() {
        return Ok(());
    }
    let mut stmt = conn.prepare(
        "INSERT OR REPLACE INTO entries
            (path, kind, size, mtime, mode, hash, link_target, deleted)
            VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, 0)",
    )?;
    for copy in copies {
        let entry = &copy.entry;
        stmt.execute(params![
            &entry.path,
            kind_to_int(entry.kind),
            entry.size,
            entry.mtime,
            entry.mode,
            entry.hash.as_deref(),
            entry.link_target.as_deref()
        ])?;
    }
    Ok(())
}
