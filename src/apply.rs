use crate::journal::{format_bytes, ExecutionStats, Journal, OpResult, Operation};
use crate::plan::{CopyDirection, DeleteSide};
use crate::output::Console;
use crate::roots::{EntryKind, Root, RootType, SshRoot};
use crate::shell::{shell_quote, shell_quote_path};
use crate::state::{CopyMetrics, PendingCopy, PendingDelete, PendingLink, StateDb};
use crate::transport::{CopyBehavior, CopyStream, Transport};
use anyhow::{bail, Context, Result};
use indicatif::ProgressBar;
use std::collections::VecDeque;
use std::path::Path;
use std::sync::{
    atomic::{AtomicBool, AtomicU64, Ordering},
    Arc,
};
use std::thread;
use std::time::{Duration, Instant};

const COPY_CHUNK: usize = 512;
const LINK_CHUNK: usize = 256;
const DELETE_CHUNK: usize = 256;

pub struct Executor<'a> {
    root_a: &'a dyn Root,
    root_b: &'a dyn Root,
    db: &'a StateDb,
    copy_chunk: usize,
    link_chunk: usize,
    delete_chunk: usize,
    behavior: CopyBehavior,
    console: &'a Console,
}

impl<'a> Executor<'a> {
    pub fn new(
        root_a: &'a dyn Root,
        root_b: &'a dyn Root,
        db: &'a StateDb,
        behavior: CopyBehavior,
        console: &'a Console,
    ) -> Self {
        Self {
            root_a,
            root_b,
            db,
            copy_chunk: COPY_CHUNK,
            link_chunk: LINK_CHUNK,
            delete_chunk: DELETE_CHUNK,
            behavior,
            console,
        }
    }

    pub fn execute(&self, _total_ops: usize, journal: &mut Journal) -> Result<ExecutionStats> {
        let mut stats = ExecutionStats::default();

        let metrics_ab = self.db.copy_metrics(CopyDirection::AtoB)?;
        self.process_copy_direction(
            CopyDirection::AtoB,
            "Copy A → B",
            metrics_ab,
            journal,
            &mut stats,
        )?;

        let links_ab = self.db.pending_link_count(CopyDirection::AtoB)?;
        self.process_links(
            CopyDirection::AtoB,
            "Link A → B",
            links_ab,
            journal,
        )?;

        let metrics_ba = self.db.copy_metrics(CopyDirection::BtoA)?;
        self.process_copy_direction(
            CopyDirection::BtoA,
            "Copy B → A",
            metrics_ba,
            journal,
            &mut stats,
        )?;

        let links_ba = self.db.pending_link_count(CopyDirection::BtoA)?;
        self.process_links(
            CopyDirection::BtoA,
            "Link B → A",
            links_ba,
            journal,
        )?;

        let delete_a = self.db.pending_delete_count(DeleteSide::RootA)?;
        self.process_deletes(
            DeleteSide::RootA,
            "Delete on A",
            delete_a,
            journal,
            &mut stats,
        )?;

        let delete_b = self.db.pending_delete_count(DeleteSide::RootB)?;
        self.process_deletes(
            DeleteSide::RootB,
            "Delete on B",
            delete_b,
            journal,
            &mut stats,
        )?;

        Ok(stats)
    }

    fn process_copy_direction(
        &self,
        direction: CopyDirection,
        label: &str,
        metrics: CopyMetrics,
        journal: &mut Journal,
        stats: &mut ExecutionStats,
    ) -> Result<()> {
        if metrics.entries == 0 {
            return Ok(());
        }

        let (src_root, dest_root) = match direction {
            CopyDirection::AtoB => (self.root_a, self.root_b),
            CopyDirection::BtoA => (self.root_b, self.root_a),
        };

        let total_work = metrics.work_units.max(1);
        let pb = self.create_progress_bar(total_work, label);
        let mut bytes_done = 0u64;
        let mut entries_done = 0usize;
        let mut chunk_records: Vec<(Operation, OpResult)> = Vec::new();
        let nonbyte_progress = Arc::new(AtomicU64::new(0));
        let mut monitor = MonitorGuard::new();
        let mut pending_commits: Vec<PendingCopy> = Vec::new();
        let mut deferred_ops: Vec<Operation> = Vec::new();
        let mut stream = Transport::persistent_stream(src_root, dest_root, self.behavior)
            .context("initializing transfer stream")?;
        let streaming = stream.is_streaming();
        if streaming && !monitor.is_active() {
            if let Some(counter) = stream.progress_counter() {
                monitor.activate(ProgressMonitor::start(
                    pb.clone(),
                    total_work,
                    metrics.file_bytes,
                    label.to_string(),
                    counter,
                    nonbyte_progress.clone(),
                ));
            }
        }

        loop {
            let limit = if streaming {
                metrics.entries.max(1)
            } else {
                self.copy_chunk
            };
            let chunk = self.db.fetch_pending_copies(direction, limit)?;
            if chunk.is_empty() {
                break;
            }

            let progress = self.send_copy_chunk(
                &mut stream,
                direction,
                &chunk,
                stats,
                &mut chunk_records,
            );
            let progress = if streaming {
                match progress {
                    Ok(progress) => {
                        deferred_ops.extend(chunk_records.drain(..).map(|(op, _)| op));
                        progress
                    }
                    Err(err) => {
                        record_stream_failures(journal, &mut deferred_ops, &mut chunk_records, &err);
                        return Err(err);
                    }
                }
            } else {
                for (op, result) in chunk_records.drain(..) {
                    journal.record(op, result);
                }
                progress?
            };
            nonbyte_progress.fetch_add(progress.nonbyte_units, Ordering::Relaxed);
            if !monitor.is_active() {
                bytes_done += progress.bytes;
                entries_done += chunk.len();
                let chunk_work = progress.bytes + progress.nonbyte_units;
                pb.inc(chunk_work.max(1));
                let chunk_range = format!(
                    "[#{}..#{}]",
                    chunk.first().map(|c| c.id).unwrap_or_default(),
                    chunk.last().map(|c| c.id).unwrap_or_default()
                );
                if metrics.file_bytes > 0 {
                    pb.set_message(format!(
                        "{} {}/{} {}",
                        label,
                        format_bytes(bytes_done),
                        format_bytes(metrics.file_bytes),
                        chunk_range
                    ));
                } else {
                    pb.set_message(format!(
                        "{} {}/{} entries {}",
                        label, entries_done, metrics.entries, chunk_range
                    ));
                }
            }

            if streaming {
                pending_commits.extend(chunk.into_iter());
                break;
            } else {
                self.db
                    .complete_pending_copies(&chunk)
                    .context("committing copy results to state DB")?;
            }
        }

        if streaming {
            if let Err(err) = stream
                .finish()
                .context("finalizing streaming copy session")
            {
                record_stream_finish_failures(journal, &mut deferred_ops, &err);
                return Err(err);
            }
            if !pending_commits.is_empty() {
                for batch in pending_commits.chunks(self.copy_chunk) {
                    if let Err(err) = self
                        .db
                        .complete_pending_copies(batch)
                        .context("committing copy results to state DB")
                    {
                        record_stream_finish_failures(journal, &mut deferred_ops, &err);
                        return Err(err);
                    }
                }
            }
            for op in deferred_ops.drain(..) {
                journal.record(op, OpResult::Success);
            }
        } else {
            stream
                .finish()
                .context("finalizing streaming copy session")?;
        }

        monitor.stop();
        pb.set_position(total_work);
        pb.finish_with_message(format!("{} complete", label));
        Ok(())
    }

    fn send_copy_chunk(
        &self,
        stream: &mut CopyStream,
        direction: CopyDirection,
        copies: &[PendingCopy],
        stats: &mut ExecutionStats,
        chunk_records: &mut Vec<(Operation, OpResult)>,
    ) -> Result<ChunkProgress> {
        if copies.is_empty() {
            return Ok(ChunkProgress {
                bytes: 0,
                nonbyte_units: 0,
            });
        }
        chunk_records.clear();
        let entries: Vec<crate::state::Entry> = copies.iter().map(|c| c.entry.clone()).collect();
        if let Err(err) = stream.send_entries(&entries) {
            for copy in copies {
                chunk_records.push((
                    Operation::new(
                        &copy.entry.path,
                        direction_label(direction),
                        "Streaming copy failed",
                    ),
                    OpResult::Failed(format!("{err:#}")),
                ));
            }
            return Err(err);
        }

        let mut chunk_bytes = 0u64;
        let mut nonbyte_units = 0u64;
        for copy in copies {
            match direction {
                CopyDirection::AtoB => {
                    stats.copies_a_to_b += 1;
                    if copy.entry.kind == EntryKind::File {
                        stats.bytes_a_to_b += copy.entry.size;
                        chunk_bytes += copy.entry.size;
                    }
                }
                CopyDirection::BtoA => {
                    stats.copies_b_to_a += 1;
                    if copy.entry.kind == EntryKind::File {
                        stats.bytes_b_to_a += copy.entry.size;
                        chunk_bytes += copy.entry.size;
                    }
                }
            }
            if copy.entry.kind != EntryKind::File || copy.entry.size == 0 {
                nonbyte_units += 1;
            }
            chunk_records.push((
                Operation::new(
                    &copy.entry.path,
                    direction_label(direction),
                    "Transferred entry",
                ),
                OpResult::Success,
            ));
        }
        Ok(ChunkProgress {
            bytes: chunk_bytes,
            nonbyte_units,
        })
    }

    fn process_deletes(
        &self,
        side: DeleteSide,
        label: &str,
        total_entries: usize,
        journal: &mut Journal,
        stats: &mut ExecutionStats,
    ) -> Result<()> {
        if total_entries == 0 {
            return Ok(());
        }
        let pb = self.create_progress_bar(total_entries as u64, label);
        let mut completed = 0usize;
        loop {
            let chunk = self.db.fetch_pending_deletes(side, self.delete_chunk)?;
            if chunk.is_empty() {
                break;
            }

            let root = match side {
                DeleteSide::RootA => self.root_a,
                DeleteSide::RootB => self.root_b,
            };

            if let Err(err) = self.delete_chunk(root, &chunk) {
                for del in &chunk {
                    record_delete_failure(del, side, &err, journal);
                }
                return Err(err);
            }

            for del in &chunk {
                record_delete_success(del, side, journal, stats);
            }
            completed += chunk.len();
            pb.inc(chunk.len() as u64);
            pb.set_message(format!("{} {}/{}", label, completed, total_entries));

            self.db
                .complete_pending_deletes(&chunk)
                .context("updating delete state")?;
        }

        pb.finish_with_message(format!("{} complete", label));
        Ok(())
    }

    fn process_links(
        &self,
        direction: CopyDirection,
        label: &str,
        total_entries: usize,
        journal: &mut Journal,
    ) -> Result<()> {
        if total_entries == 0 {
            return Ok(());
        }
        let pb = self.create_progress_bar(total_entries as u64, label);
        let mut completed = 0usize;
        loop {
            let chunk = self.db.fetch_pending_links(direction, self.link_chunk)?;
            if chunk.is_empty() {
                break;
            }

            let root = match direction {
                CopyDirection::AtoB => self.root_b,
                CopyDirection::BtoA => self.root_a,
            };

            if let Err(err) = self.validate_link_targets(&chunk) {
                for link in &chunk {
                    record_link_failure(link, direction, &err, journal);
                }
                return Err(err);
            }

            if let Err(err) = self.link_chunk(root, &chunk) {
                for link in &chunk {
                    record_link_failure(link, direction, &err, journal);
                }
                return Err(err);
            }

            for link in &chunk {
                record_link_success(link, direction, journal);
            }
            completed += chunk.len();
            pb.inc(chunk.len() as u64);
            pb.set_message(format!("{} {}/{}", label, completed, total_entries));

            self.db
                .complete_pending_links(&chunk)
                .context("updating link state")?;
        }

        pb.finish_with_message(format!("{} complete", label));
        Ok(())
    }

    fn validate_link_targets(&self, links: &[PendingLink]) -> Result<()> {
        for link in links {
            if link.path == link.target {
                continue;
            }
            let entry = self.db.get_entry(&link.target)?;
            let entry = match entry {
                Some(entry) => entry,
                None => bail!("Hardlink target missing in state: {}", link.target),
            };
            if entry.deleted {
                bail!("Hardlink target marked deleted in state: {}", link.target);
            }
        }
        Ok(())
    }

    fn delete_chunk(&self, root: &dyn Root, deletes: &[PendingDelete]) -> Result<()> {
        if deletes.is_empty() {
            return Ok(());
        }
        match root.kind() {
            RootType::Local => {
                for del in deletes {
                    let path = Path::new(&del.path);
                    match del.kind {
                        EntryKind::Dir => root.remove_dir(path)?,
                        _ => root.remove_file(path)?,
                    }
                }
                Ok(())
            }
            RootType::Ssh => {
                let ssh = root
                    .as_any()
                    .downcast_ref::<SshRoot>()
                    .context("invalid SSH root handle")?;
                run_remote_delete(ssh, deletes)
            }
        }
    }

    fn link_chunk(&self, root: &dyn Root, links: &[PendingLink]) -> Result<()> {
        if links.is_empty() {
            return Ok(());
        }
        match root.kind() {
            RootType::Local => {
                let local = root
                    .as_any()
                    .downcast_ref::<crate::roots::LocalRoot>()
                    .context("invalid local root handle")?;
                run_local_links(local, links)
            }
            RootType::Ssh => {
                let ssh = root
                    .as_any()
                    .downcast_ref::<SshRoot>()
                    .context("invalid SSH root handle")?;
                run_remote_links(ssh, links)
            }
        }
    }
}

impl<'a> Executor<'a> {
    fn create_progress_bar(&self, total: u64, label: &str) -> ProgressBar {
        self.console.progress_bar(total, label)
    }
}

fn record_delete_success(
    del: &PendingDelete,
    side: DeleteSide,
    journal: &mut Journal,
    stats: &mut ExecutionStats,
) {
    match side {
        DeleteSide::RootA => stats.deletes_on_a += 1,
        DeleteSide::RootB => stats.deletes_on_b += 1,
    }
    journal.record(
        Operation::new(&del.path, delete_label(side), "Removed entry"),
        OpResult::Success,
    );
}

fn record_link_success(link: &PendingLink, direction: CopyDirection, journal: &mut Journal) {
    journal.record(
        Operation::new(&link.path, link_label(direction), "Linked entry"),
        OpResult::Success,
    );
}

fn record_link_failure(
    link: &PendingLink,
    direction: CopyDirection,
    err: &anyhow::Error,
    journal: &mut Journal,
) {
    journal.record(
        Operation::new(&link.path, link_label(direction), "Failed to link"),
        OpResult::Failed(format!("{err:#}")),
    );
}

fn record_delete_failure(
    del: &PendingDelete,
    side: DeleteSide,
    err: &anyhow::Error,
    journal: &mut Journal,
) {
    journal.record(
        Operation::new(&del.path, delete_label(side), "Failed to remove"),
        OpResult::Failed(format!("{err:#}")),
    );
}

fn direction_label(direction: CopyDirection) -> &'static str {
    match direction {
        CopyDirection::AtoB => "Copy A → B",
        CopyDirection::BtoA => "Copy B → A",
    }
}

fn delete_label(side: DeleteSide) -> &'static str {
    match side {
        DeleteSide::RootA => "Delete on A",
        DeleteSide::RootB => "Delete on B",
    }
}

fn link_label(direction: CopyDirection) -> &'static str {
    match direction {
        CopyDirection::AtoB => "Link A → B",
        CopyDirection::BtoA => "Link B → A",
    }
}

fn record_stream_failures(
    journal: &mut Journal,
    deferred_ops: &mut Vec<Operation>,
    chunk_records: &mut Vec<(Operation, OpResult)>,
    err: &anyhow::Error,
) {
    let message = format!("{err:#}");
    for op in deferred_ops.drain(..) {
        journal.record(op, OpResult::Failed(message.clone()));
    }
    for (op, result) in chunk_records.drain(..) {
        journal.record(op, result);
    }
}

fn record_stream_finish_failures(
    journal: &mut Journal,
    deferred_ops: &mut Vec<Operation>,
    err: &anyhow::Error,
) {
    let message = format!("{err:#}");
    for op in deferred_ops.drain(..) {
        journal.record(op, OpResult::Failed(message.clone()));
    }
}

fn run_remote_delete(root: &SshRoot, deletes: &[PendingDelete]) -> Result<()> {
    if deletes.is_empty() {
        return Ok(());
    }

    let (rm_cmd, rmdir_cmd) = build_remote_delete_commands(root.path(), deletes);

    if let Some(cmd) = rm_cmd {
        let (_out, err, code) = root.exec(&cmd)?;
        if code != 0 {
            let message = String::from_utf8_lossy(&err);
            bail!("remote remove failed: {}", message.trim());
        }
    }

    if let Some(cmd) = rmdir_cmd {
        let (_out, err, code) = root.exec(&cmd)?;
        if code != 0 {
            let message = String::from_utf8_lossy(&err);
            bail!("remote rmdir failed: {}", message.trim());
        }
    }
    Ok(())
}

fn build_remote_delete_commands(
    root_path: &Path,
    deletes: &[PendingDelete],
) -> (Option<String>, Option<String>) {
    let mut files = Vec::new();
    let mut dirs = Vec::new();
    for del in deletes {
        match del.kind {
            EntryKind::Dir => dirs.push(del.path.as_str()),
            _ => files.push(del.path.as_str()),
        }
    }

    let root_q = shell_quote_path(root_path);
    let rm_cmd = if files.is_empty() {
        None
    } else {
        let mut cmd = format!("cd {root_q} && rm --");
        for path in files {
            cmd.push(' ');
            cmd.push_str(&shell_quote(path));
        }
        Some(cmd)
    };

    let rmdir_cmd = if dirs.is_empty() {
        None
    } else {
        let mut cmd = format!("cd {root_q} && rmdir --");
        for path in dirs {
            cmd.push(' ');
            cmd.push_str(&shell_quote(path));
        }
        Some(cmd)
    };

    (rm_cmd, rmdir_cmd)
}

fn run_local_links(root: &crate::roots::LocalRoot, links: &[PendingLink]) -> Result<()> {
    use std::io;
    for link in links {
        if link.path == link.target {
            continue;
        }
        let path = root.path().join(&link.path);
        let target = root.path().join(&link.target);
        if let Err(err) = std::fs::remove_file(&path) {
            if err.kind() != io::ErrorKind::NotFound {
                return Err(err.into());
            }
        }
        std::fs::hard_link(&target, &path)?;
    }
    Ok(())
}

fn run_remote_links(root: &SshRoot, links: &[PendingLink]) -> Result<()> {
    if links.is_empty() {
        return Ok(());
    }
    let root_q = shell_quote_path(root.path());
    let mut cmd = format!("cd {root_q} && ");
    let targets: std::collections::HashSet<&str> =
        links.iter().map(|link| link.target.as_str()).collect();
    let mut remove_any = false;
    for link in links {
        if targets.contains(link.path.as_str()) {
            continue;
        }
        cmd.push_str("rm -f -- ");
        cmd.push_str(&shell_quote(&link.path));
        cmd.push_str(" && ");
        remove_any = true;
    }
    if !remove_any {
        cmd.push_str("true && ");
    }
    for link in links {
        if link.path == link.target {
            continue;
        }
        cmd.push_str("ln -- ");
        cmd.push_str(&shell_quote(&link.target));
        cmd.push(' ');
        cmd.push_str(&shell_quote(&link.path));
        cmd.push_str(" && ");
    }
    cmd.push_str("true");

    let (_out, err, code) = root.exec(&cmd)?;
    if code != 0 {
        let message = String::from_utf8_lossy(&err);
        bail!("remote link failed: {}", message.trim());
    }
    Ok(())
}

struct ChunkProgress {
    bytes: u64,
    nonbyte_units: u64,
}

struct ProgressMonitor {
    stop: Arc<AtomicBool>,
    handle: thread::JoinHandle<()>,
}

impl ProgressMonitor {
    fn start(
        pb: ProgressBar,
        total_work: u64,
        total_bytes: u64,
        label: String,
        counter: Arc<AtomicU64>,
        nonbyte_progress: Arc<AtomicU64>,
    ) -> Self {
        let stop = Arc::new(AtomicBool::new(false));
        let stop_flag = stop.clone();
        let handle = thread::spawn(move || {
            let mut samples: VecDeque<(Instant, u64)> = VecDeque::new();
            const WINDOW: Duration = Duration::from_secs(10);
            loop {
                if stop_flag.load(Ordering::Relaxed) {
                    break;
                }
                let bytes = counter.load(Ordering::Relaxed);
                let extra = nonbyte_progress.load(Ordering::Relaxed);
                let position = (bytes + extra).min(total_work);
                pb.set_position(position);
                if total_bytes > 0 {
                    let now = Instant::now();
                    samples.push_back((now, bytes));
                    while let Some(&(t, _)) = samples.front() {
                        if now.duration_since(t) > WINDOW {
                            samples.pop_front();
                        } else {
                            break;
                        }
                    }
                    let rate = if let Some(&(t0, b0)) = samples.front() {
                        let dt = now.duration_since(t0).as_secs_f64();
                        if dt > 0.0 {
                            (bytes.saturating_sub(b0)) as f64 / dt
                        } else {
                            0.0
                        }
                    } else {
                        0.0
                    };
                    let eta = if rate > 0.0 {
                        Duration::from_secs_f64(total_bytes.saturating_sub(bytes) as f64 / rate)
                    } else {
                        Duration::ZERO
                    };
                    let msg = if rate > 0.0 {
                        format!(
                            "{} {}/{} {}/s ETA {}",
                            label,
                            format_bytes(bytes),
                            format_bytes(total_bytes),
                            format_bytes(rate as u64),
                            format_duration(eta)
                        )
                    } else {
                        format!(
                            "{} {}/{} ETA --",
                            label,
                            format_bytes(bytes),
                            format_bytes(total_bytes)
                        )
                    };
                    pb.set_message(msg);
                } else {
                    pb.set_message(format!("{} {} units", label, position));
                }
                thread::sleep(Duration::from_millis(200));
            }
        });
        Self { stop, handle }
    }

    fn stop(self) {
        self.stop.store(true, Ordering::Relaxed);
        let _ = self.handle.join();
    }
}

struct MonitorGuard {
    inner: Option<ProgressMonitor>,
}

impl MonitorGuard {
    fn new() -> Self {
        Self { inner: None }
    }

    fn is_active(&self) -> bool {
        self.inner.is_some()
    }

    fn activate(&mut self, monitor: ProgressMonitor) {
        self.inner = Some(monitor);
    }

    fn stop(&mut self) {
        if let Some(monitor) = self.inner.take() {
            monitor.stop();
        }
    }
}

impl Drop for MonitorGuard {
    fn drop(&mut self) {
        if let Some(monitor) = self.inner.take() {
            monitor.stop();
        }
    }
}

fn format_duration(duration: Duration) -> String {
    let secs = duration.as_secs();
    let hours = secs / 3600;
    let minutes = (secs % 3600) / 60;
    let seconds = secs % 60;
    if hours > 0 {
        format!("{:02}:{:02}:{:02}", hours, minutes, seconds)
    } else {
        format!("{:02}:{:02}", minutes, seconds)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn build_remote_delete_commands_splits_files_and_dirs() {
        let deletes = vec![
            PendingDelete {
                id: 1,
                path: "a.txt".to_string(),
                kind: EntryKind::File,
            },
            PendingDelete {
                id: 2,
                path: "dir".to_string(),
                kind: EntryKind::Dir,
            },
            PendingDelete {
                id: 3,
                path: "link".to_string(),
                kind: EntryKind::Symlink,
            },
        ];

        let (rm_cmd, rmdir_cmd) =
            build_remote_delete_commands(Path::new("/root"), &deletes);

        let rm_cmd = rm_cmd.expect("rm command");
        let rmdir_cmd = rmdir_cmd.expect("rmdir command");

        assert!(rm_cmd.contains("rm --"));
        assert!(rm_cmd.contains("'a.txt'"));
        assert!(rm_cmd.contains("'link'"));
        assert!(!rm_cmd.contains("'dir'"));

        assert!(rmdir_cmd.contains("rmdir --"));
        assert!(rmdir_cmd.contains("'dir'"));
        assert!(!rmdir_cmd.contains("'a.txt'"));
    }
}
