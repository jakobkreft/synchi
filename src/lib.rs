use anyhow::{Context, Result};
use hostname::get;
use indicatif::ProgressBar;
use std::collections::HashMap;
use std::io::{self, Cursor};
use std::path::{Path, PathBuf};
use std::sync::Once;
use std::time::Instant;
use tracing::info;
use tracing_subscriber::FmtSubscriber;

mod apply;
pub mod cli;
mod config;
mod diff;
mod journal;
mod output;
mod plan;
mod progress;
mod roots;
mod scan;
mod shell;
mod state;
mod transport;
mod ui;

pub use cli::Cli;
use output::Console;
use roots::{Root, RootSpec};
use transport::CopyBehavior;
use crate::cli::Commands;

pub fn run(cli: Cli) -> Result<()> {
    install_signal_handler();
    let mut console = Console::stdio();

    let log_level = if cli.verbose { "debug" } else { "warn" };

    let env_filter = std::env::var("RUST_LOG").unwrap_or_else(|_| format!("synchi={}", log_level));

    let subscriber = FmtSubscriber::builder()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new(&env_filter)),
        )
        .finish();
    tracing::subscriber::set_global_default(subscriber).expect("setting default subscriber failed");

    let default_config_path = dirs::config_dir()
        .map(|d| d.join("synchi").join("config.toml"))
        .unwrap_or_else(|| PathBuf::from("config.toml"));

    let config_path = cli.config.as_ref().unwrap_or(&default_config_path);

    let file_config = config::Config::load_from_file(config_path)?;

    let mut config = file_config;
    if let Some(r) = cli.root_a {
        config.root_a = Some(r);
    }
    if let Some(r) = cli.root_b {
        config.root_b = Some(r);
    }
    if let Some(mode) = cli.hash_mode {
        config.hash_mode = mode.into();
    }
    if let Some(force_arg) = cli.force {
        config.force = force_arg.as_config_value();
    }
    if let Some(name) = cli.state_db_name {
        config.state_db_name = Some(name);
    }

    let root_a_spec = config
        .root_a
        .as_deref()
        .map(RootSpec::parse)
        .transpose()?;
    let root_b_spec = config
        .root_b
        .as_deref()
        .map(RootSpec::parse)
        .transpose()?;

    print_effective_config(&config, root_a_spec.as_ref(), root_b_spec.as_ref(), &mut console)?;

    match &cli.command {
        Commands::Init => {
            info!("Initializing synchi...");

            let root_a_spec = root_a_spec
                .as_ref()
                .context("Root A not defined in config or CLI")?;
            let root_b_spec = root_b_spec
                .as_ref()
                .context("Root B not defined in config or CLI")?;
            let db_filename = config.state_db_filename();

            let root_a = match root_a_spec {
                RootSpec::Local { path } => roots::LocalRoot::new(path)?,
                _ => anyhow::bail!("Root A must be a local path"),
            };
            if root_b_spec.is_local() {
                let local_path = root_b_spec
                    .local_path()
                    .context("Root B local path missing")?;
                let _ = roots::LocalRoot::new(local_path)?;
            }

            let synchi_dir = root_a.path().join(".synchi");
            std::fs::create_dir_all(&synchi_dir).context("Failed to create .synchi directory")?;

            let state_db_path = synchi_dir.join(&db_filename);
            let _db = state::StateDb::open(&state_db_path)
                .context("Failed to initialize state database")?;

            console.out(&format!(
                "✓ Initialized synchi in {}",
                root_a_spec.display()
            ))?;
            console.out(&format!("  State DB: {:?}", state_db_path))?;
            console.out(&format!("  Root A: {}", root_a_spec.display()))?;
            console.out(&format!("  Root B: {}", root_b_spec.display()))?;
        }
        Commands::Status => {
            info!("Checking status...");

            let db_filename = config.state_db_filename();

            let root_a_spec = root_a_spec
                .as_ref()
                .context("Root A not defined in config or CLI")?;
            let root_b_spec = root_b_spec
                .as_ref()
                .context("Root B not defined in config or CLI")?;

            let root_a = match root_a_spec {
                RootSpec::Local { path } => roots::LocalRoot::new(path)?,
                _ => anyhow::bail!("Root A must be a local path"),
            };
            let root_b = root_b_spec.root()?;
            ensure_root_ready(&root_a)?;
            ensure_root_ready(root_b.as_ref())?;
            let lock_info = lock_info_string();
            let lock_name = format!("{}.lock", db_filename);
            let _lock_a = state::Lock::acquire(&root_a, &lock_name, &lock_info)?;
            let _lock_b = state::Lock::acquire(root_b.as_ref(), &lock_name, &lock_info)?;

            let state_db_path = root_a.path().join(".synchi").join(&db_filename);
            if !state_db_path.exists() {
                console.out("Not initialized. Run 'synchi init' first.")?;
                return Ok(());
            }
            let db = state::StateDb::open(&state_db_path)?;
            let state_entries = db.list_entries()?;
            let state_count = state_entries.len();
            let state_hint = state_count as u64;
            let state_map: HashMap<String, state::Entry> = state_entries
                .iter()
                .map(|e| (e.path.clone(), e.clone()))
                .collect();

            let default_include = vec!["**".to_string()];
            let include_patterns = config.include.as_deref().unwrap_or(&default_include);
            let default_ignore = vec![];
            let ignore_patterns = config.ignore.as_deref().unwrap_or(&default_ignore);
            let filter = scan::Filter::new(include_patterns, ignore_patterns)?;

            let label_a = format!("Root A ({})", root_a.path().display());
            info!("Scanning {}", label_a);
            let mut scan_a = run_scan_with_progress(&label_a, Some(state_hint), |pb| {
                scan::LocalScanner::with_skip_hardlinks(&root_a, &filter, config.skip_hardlinks)
                    .scan_with_progress(Some(pb))
            }, &console)?;
            let mut scan_b = if root_b.kind() == roots::RootType::Ssh {
                let ssh_root = root_b.as_any().downcast_ref::<roots::SshRoot>().unwrap();
                let caps = ssh_root.probe_caps()?;
                let label_b = format!("Root B ({})", ssh_root.path().display());
                info!("Scanning {}", label_b);
                run_scan_with_progress(&label_b, Some(state_hint), |pb| {
                    scan::RemoteScanner::new(ssh_root, &filter, caps, config.skip_hardlinks)
                        .scan_with_progress(Some(pb))
                }, &console)?
            } else {
                let local_b = root_b.as_any().downcast_ref::<roots::LocalRoot>().unwrap();
                let label_b = format!("Root B ({})", local_b.path().display());
                info!("Scanning {}", label_b);
                run_scan_with_progress(&label_b, Some(state_hint), |pb| {
                    scan::LocalScanner::with_skip_hardlinks(local_b, &filter, config.skip_hardlinks)
                        .scan_with_progress(Some(pb))
                }, &console)?
            };
            let scan_a_count = scan_a.len();
            let scan_b_count = scan_b.len();

            hash_with_logging(
                "Root A",
                &root_a,
                &mut scan_a,
                &state_map,
                config.hash_mode,
                &console,
            )?;
            hash_with_logging(
                "Root B",
                root_b.as_ref(),
                &mut scan_b,
                &state_map,
                config.hash_mode,
                &console,
            )?;

            let state_a = state_entries.clone();
            let state_b = state_entries;

            let mut diffs = diff::DiffEngine::diff(scan_a, state_a, scan_b, state_b, &filter);

            if let Some(force_side) = config.force_side()? {
                match force_side {
                    config::ForceSide::RootA => {
                        console.out("Forcing State to match Root A (Mirror A -> B)")?
                    }
                    config::ForceSide::RootB => {
                        console.out("Forcing State to match Root B (Mirror B -> A)")?
                    }
                }
                apply_force_mirror(&mut diffs, force_side);
            }

            let summary = summarize_diffs(&diffs);
            print_status_summary(
                scan_a_count,
                scan_b_count,
                state_count,
                &summary,
                true,
                &mut console,
            )?;
        }
        Commands::Sync {
            dry_run,
            auto_yes,
            copy_a_to_b,
            copy_b_to_a,
            delete_on_a,
            delete_on_b,
        } => {
            let overall_start = Instant::now();
            let db_filename = config.state_db_filename();

            let root_a_spec = root_a_spec
                .as_ref()
                .context("Root A not defined in config or CLI")?;
            let root_b_spec = root_b_spec
                .as_ref()
                .context("Root B not defined in config or CLI")?;

            info!(
                "Syncing from {} to {}",
                root_a_spec.display(),
                root_b_spec.display()
            );

            let root_a = match root_a_spec {
                RootSpec::Local { path } => roots::LocalRoot::new(path)?,
                _ => anyhow::bail!("Root A must be a local path"),
            };
            let root_b = root_b_spec.root()?;
            ensure_root_ready(&root_a)?;
            ensure_root_ready(root_b.as_ref())?;
            let lock_info = lock_info_string();
            let lock_name = format!("{}.lock", db_filename);
            let _lock_a = state::Lock::acquire(&root_a, &lock_name, &lock_info)?;
            let _lock_b = state::Lock::acquire(root_b.as_ref(), &lock_name, &lock_info)?;

            let state_db_path = root_a.path().join(".synchi").join(&db_filename);
            std::fs::create_dir_all(state_db_path.parent().unwrap())?;
            let db = state::StateDb::open(&state_db_path)?;
            let state_snapshot = db.list_entries()?;
            let state_hint = state_snapshot.len() as u64;
            let state_map: HashMap<String, state::Entry> = state_snapshot
                .iter()
                .map(|e| (e.path.clone(), e.clone()))
                .collect();

            let default_include = vec!["**".to_string()];
            let include_patterns = config.include.as_deref().unwrap_or(&default_include);
            let default_ignore = vec![];
            let ignore_patterns = config.ignore.as_deref().unwrap_or(&default_ignore);

            let filter = scan::Filter::new(include_patterns, ignore_patterns)?;

            let label_a = format!("Root A ({})", root_a.path().display());
            info!("Scanning {}", label_a);
            let mut scan_a = run_scan_with_progress(&label_a, Some(state_hint), |pb| {
                scan::LocalScanner::with_skip_hardlinks(&root_a, &filter, config.skip_hardlinks)
                    .scan_with_progress(Some(pb))
            }, &console)?;
            let mut scan_b = if root_b.kind() == roots::RootType::Ssh {
                let ssh_root = root_b
                    .as_any()
                    .downcast_ref::<roots::SshRoot>()
                    .expect("Should be SSH root");
                let caps = ssh_root.probe_caps()?;
                let label_b = format!("Root B ({})", ssh_root.path().display());
                info!("Scanning {}", label_b);
                run_scan_with_progress(&label_b, Some(state_hint), |pb| {
                    scan::RemoteScanner::new(ssh_root, &filter, caps, config.skip_hardlinks)
                        .scan_with_progress(Some(pb))
                }, &console)?
            } else if let Some(local_b) = root_b.as_any().downcast_ref::<roots::LocalRoot>() {
                let label_b = format!("Root B ({})", local_b.path().display());
                info!("Scanning {}", label_b);
                run_scan_with_progress(&label_b, Some(state_hint), |pb| {
                    scan::LocalScanner::with_skip_hardlinks(local_b, &filter, config.skip_hardlinks)
                        .scan_with_progress(Some(pb))
                }, &console)?
            } else {
                anyhow::bail!("Unsupported root type for B");
            };
            let scan_a_count = scan_a.len();
            let scan_b_count = scan_b.len();

            hash_with_logging(
                "Root A",
                &root_a,
                &mut scan_a,
                &state_map,
                config.hash_mode,
                &console,
            )?;
            if !*dry_run {
                db.refresh_metadata(&scan_a)?;
            }
            hash_with_logging(
                "Root B",
                root_b.as_ref(),
                &mut scan_b,
                &state_map,
                config.hash_mode,
                &console,
            )?;

            let state_entries = db.list_entries()?;
            let state_count = state_entries.len();
            let state_a = state_entries.clone();
            let state_b = state_entries;

            let mut diffs = diff::DiffEngine::diff(scan_a, state_a, scan_b, state_b, &filter);

            if let Some(force_side) = config.force_side()? {
                match force_side {
                    config::ForceSide::RootA => {
                        console.out("Forcing State to match Root A (Mirror A -> B)")?
                    }
                    config::ForceSide::RootB => {
                        console.out("Forcing State to match Root B (Mirror B -> A)")?
                    }
                }
                apply_force_mirror(&mut diffs, force_side);
            }

            let summary = summarize_diffs(&diffs);
            print_status_summary(
                scan_a_count,
                scan_b_count,
                state_count,
                &summary,
                false,
                &mut console,
            )?;

            let mut plan = plan::PlanBuilder::build(diffs);

            if !plan.conflicts.is_empty() {
                console.out(&format!("Found {} conflicts.", plan.conflicts.len()))?;
                if *dry_run {
                    console.out("Dry run: Skipping conflict resolution.")?;
                } else {
                    plan = ui::Ui::resolve_conflicts(plan)?;
                }
            }

            let copy_a_to_b_choice = (*copy_a_to_b).map(|val| val.as_bool());
            let allow_copy_a_to_b = resolve_category_setting(
                copy_a_to_b_choice,
                "Copy A → B",
                PendingView::copy(&plan.copy_a_to_b),
                *dry_run,
                *auto_yes,
                &mut console,
            )?;
            if !allow_copy_a_to_b {
                plan.copy_a_to_b.clear();
            }

            let copy_b_to_a_choice = (*copy_b_to_a).map(|val| val.as_bool());
            let allow_copy_b_to_a = resolve_category_setting(
                copy_b_to_a_choice,
                "Copy B → A",
                PendingView::copy(&plan.copy_b_to_a),
                *dry_run,
                *auto_yes,
                &mut console,
            )?;
            if !allow_copy_b_to_a {
                plan.copy_b_to_a.clear();
            }

            let delete_on_a_choice = (*delete_on_a).map(|val| val.as_bool());
            let allow_delete_on_a = resolve_category_setting(
                delete_on_a_choice,
                "Delete on A",
                PendingView::delete(&plan.delete_a),
                *dry_run,
                *auto_yes,
                &mut console,
            )?;
            if !allow_delete_on_a {
                plan.delete_a.clear();
            }

            let delete_on_b_choice = (*delete_on_b).map(|val| val.as_bool());
            let allow_delete_on_b = resolve_category_setting(
                delete_on_b_choice,
                "Delete on B",
                PendingView::delete(&plan.delete_b),
                *dry_run,
                *auto_yes,
                &mut console,
            )?;
            if !allow_delete_on_b {
                plan.delete_b.clear();
            }

            let total_ops = plan.total_operations();
            console.out(&format!("Executing {} operations...", total_ops))?;
            if *dry_run {
                for entry in &plan.copy_a_to_b {
                    console.out(&format!("Copy A → B {}", entry.path))?;
                }
                for entry in &plan.copy_b_to_a {
                    console.out(&format!("Copy B → A {}", entry.path))?;
                }
                for del in &plan.delete_b {
                    console.out(&format!("Delete in B {}", del.path))?;
                }
                for del in &plan.delete_a {
                    console.out(&format!("Delete in A {}", del.path))?;
                }
                console.out(&format!("Overall Duration: {:.2?}", overall_start.elapsed()))?;
            } else {
                db.queue_plan(&plan)?;
                let mut journal = journal::Journal::new();
                let copy_behavior = CopyBehavior {
                    preserve_owner: config.preserve_owner,
                    preserve_permissions: config.preserve_permissions,
                };
                let executor = apply::Executor::new(
                    &root_a,
                    root_b.as_ref(),
                    &db,
                    copy_behavior,
                    &console,
                );
                match executor.execute(total_ops, &mut journal) {
                    Ok(stats) => {
                        journal.set_stats(stats);
                        journal.set_overall_duration(overall_start.elapsed());
                        console.out(&format!("\n{}", journal))?;
                    }
                    Err(err) => {
                        journal.set_overall_duration(overall_start.elapsed());
                        console.out(&format!("\n{}", journal))?;
                        return Err(err);
                    }
                }
            }

            info!("Sync complete.");
        }
    }

    Ok(())
}

fn install_signal_handler() {
    static INIT: Once = Once::new();
    INIT.call_once(|| {
        ctrlc::set_handler(|| {
            tracing::warn!("Received Ctrl-C, cleaning up...");
            crate::state::force_unlock_all();
            std::process::exit(130);
        })
        .expect("failed to install Ctrl-C handler");
    });
}

fn print_effective_config(
    config: &config::Config,
    root_a: Option<&RootSpec>,
    root_b: Option<&RootSpec>,
    console: &mut Console,
) -> Result<()> {
    console.out("=== Configuration ===")?;
    console.out(&format!(
        "Root A: {}",
        root_a
            .map(|spec| spec.display())
            .unwrap_or_else(|| "<not set>".to_string())
    ))?;
    console.out(&format!(
        "Root B: {}",
        root_b
            .map(|spec| spec.display())
            .unwrap_or_else(|| "<not set>".to_string())
    ))?;

    let include = config
        .include
        .clone()
        .unwrap_or_else(|| vec!["**".to_string()]);
    let ignore = config.ignore.clone().unwrap_or_default();

    console.out(&format!("Include patterns: {}", format_list(&include)))?;
    console.out(&format!("Ignore patterns: {}", format_list(&ignore)))?;

    let force_display = match config.force.as_deref() {
        Some(val) if !val.trim().is_empty() => val.trim(),
        _ => "none (default)",
    };
    console.out(&format!("Force mode: {}", force_display))?;
    console.out(&format!("Skip hardlinks: {}", config.skip_hardlinks))?;
    console.out(&format!("Hash mode: {:?}", config.hash_mode))?;
    console.out(&format!("Preserve owner: {}", config.preserve_owner))?;
    console.out(&format!(
        "Preserve permissions: {}",
        config.preserve_permissions
    ))?;
    console.out(&format!("State DB file: {}", config.state_db_filename()))?;
    console.out("")?;
    Ok(())
}

fn format_list(list: &[String]) -> String {
    if list.is_empty() {
        "none".to_string()
    } else {
        list.join(", ")
    }
}

fn lock_info_string() -> String {
    let host = get()
        .ok()
        .and_then(|h| h.into_string().ok())
        .unwrap_or_else(|| "unknown".to_string());
    format!("pid={} host={}", std::process::id(), host)
}

fn run_scan_with_progress<F>(
    label: &str,
    expected: Option<u64>,
    runner: F,
    console: &Console,
) -> Result<Vec<state::Entry>>
where
    F: FnOnce(&ProgressBar) -> Result<Vec<state::Entry>>,
{
    let pb = start_scan_progress(label, expected, console);
    let result = runner(&pb);
    match result {
        Ok(entries) => {
            let len = entries.len() as u64;
            if len > 0 {
                pb.set_length(len);
                pb.set_position(len);
            }
            pb.finish_with_message(format!("Scanned {label}: {} entries", entries.len()));
            Ok(entries)
        }
        Err(err) => {
            pb.finish_and_clear();
            Err(err)
        }
    }
}

fn start_scan_progress(label: &str, expected: Option<u64>, console: &Console) -> ProgressBar {
    console.progress_bar(expected.unwrap_or(1), &format!("Scanning {label}"))
}

fn start_hash_progress(label: &str, console: &Console) -> ProgressBar {
    console.spinner(&format!("Hashing {label}"))
}

fn apply_force_mirror(diffs: &mut [diff::DiffResult], side: config::ForceSide) {
    use diff::ChangeType;
    match side {
        config::ForceSide::RootA => {
            for diff in diffs {
                let differs = diff.change_a.change != ChangeType::Unchanged
                    || diff.change_b.change != ChangeType::Unchanged;
                if !differs {
                    diff.action = diff::SyncAction::NoOp;
                    continue;
                }
                diff.action = if diff.change_a.entry_now.is_some() {
                    diff::SyncAction::CopyAtoB
                } else if diff.change_b.entry_now.is_some() {
                    diff::SyncAction::DeleteB
                } else {
                    diff::SyncAction::NoOp
                };
            }
        }
        config::ForceSide::RootB => {
            for diff in diffs {
                let differs = diff.change_a.change != ChangeType::Unchanged
                    || diff.change_b.change != ChangeType::Unchanged;
                if !differs {
                    diff.action = diff::SyncAction::NoOp;
                    continue;
                }
                diff.action = if diff.change_b.entry_now.is_some() {
                    diff::SyncAction::CopyBtoA
                } else if diff.change_a.entry_now.is_some() {
                    diff::SyncAction::DeleteA
                } else {
                    diff::SyncAction::NoOp
                };
            }
        }
    }
}

#[derive(Default)]
struct PendingSummary {
    copy_a_to_b: usize,
    copy_b_to_a: usize,
    delete_a: usize,
    delete_b: usize,
    conflicts: usize,
    no_ops: usize,
}

impl PendingSummary {
    fn total_pending(&self) -> usize {
        self.copy_a_to_b + self.copy_b_to_a + self.delete_a + self.delete_b + self.conflicts
    }
}

fn summarize_diffs(diffs: &[diff::DiffResult]) -> PendingSummary {
    let mut summary = PendingSummary::default();
    for diff in diffs {
        match diff.action {
            diff::SyncAction::CopyAtoB => summary.copy_a_to_b += 1,
            diff::SyncAction::CopyBtoA => summary.copy_b_to_a += 1,
            diff::SyncAction::DeleteA => summary.delete_a += 1,
            diff::SyncAction::DeleteB => summary.delete_b += 1,
            diff::SyncAction::Conflict(_) => summary.conflicts += 1,
            diff::SyncAction::NoOp => summary.no_ops += 1,
        }
    }
    summary
}

fn print_status_summary(
    scan_a_count: usize,
    scan_b_count: usize,
    state_count: usize,
    summary: &PendingSummary,
    show_guidance: bool,
    console: &mut Console,
) -> Result<()> {
    console.out("\n=== Status ===")?;
    console.out(&format!("Files in A: {}", scan_a_count))?;
    console.out(&format!("Files in B: {}", scan_b_count))?;
    console.out(&format!("State entries: {}", state_count))?;
    console.out("")?;
    console.out("Pending changes:")?;
    console.out(&format!("  Copy A → B: {}", summary.copy_a_to_b))?;
    console.out(&format!("  Copy B → A: {}", summary.copy_b_to_a))?;
    console.out(&format!("  Delete in A: {}", summary.delete_a))?;
    console.out(&format!("  Delete in B: {}", summary.delete_b))?;
    console.out(&format!("  Conflicts: {}", summary.conflicts))?;
    console.out(&format!("  No change: {}", summary.no_ops))?;

    if show_guidance {
        if summary.total_pending() == 0 {
            console.out("\n✓ Everything is in sync!")?;
        } else {
            console.out("\nRun 'synchi sync' to synchronize.")?;
        }
    }
    Ok(())
}

fn ensure_root_ready(root: &dyn roots::Root) -> Result<()> {
    root.mkdirs(Path::new(".synchi"))?;
    let mut cursor = Cursor::new(Vec::from("synchi access check"));
    root.write_file(Path::new(".synchi/.access_check"), &mut cursor)?;
    let _ = root.remove_file(Path::new(".synchi/.access_check"));
    Ok(())
}

enum PendingView<'a> {
    Copy(&'a [state::Entry]),
    Delete(&'a [plan::DeleteOp]),
}

impl<'a> PendingView<'a> {
    fn copy(entries: &'a [state::Entry]) -> Option<Self> {
        if entries.is_empty() {
            None
        } else {
            Some(Self::Copy(entries))
        }
    }

    fn delete(entries: &'a [plan::DeleteOp]) -> Option<Self> {
        if entries.is_empty() {
            None
        } else {
            Some(Self::Delete(entries))
        }
    }

    fn len(&self) -> usize {
        match self {
            PendingView::Copy(entries) => entries.len(),
            PendingView::Delete(entries) => entries.len(),
        }
    }

    fn print(&self, label: &str, console: &mut Console) -> Result<()> {
        match self {
            PendingView::Copy(entries) => {
                for entry in *entries {
                    console.out(&format!("{} {}", label, entry.path))?;
                }
            }
            PendingView::Delete(entries) => {
                for entry in *entries {
                    console.out(&format!("{} {}", label, entry.path))?;
                }
            }
        }
        Ok(())
    }
}

fn resolve_category_setting(
    choice: Option<bool>,
    label: &str,
    view: Option<PendingView<'_>>,
    dry_run: bool,
    auto_yes: bool,
    console: &mut Console,
) -> Result<bool> {
    if view.as_ref().map(|v| v.len()).unwrap_or(0) == 0 {
        return Ok(true);
    }
    if let Some(value) = choice {
        return Ok(value);
    }
    if dry_run || auto_yes {
        Ok(true)
    } else {
        prompt_category(label, view.as_ref(), console)
    }
}

fn prompt_category(
    label: &str,
    view: Option<&PendingView<'_>>,
    console: &mut Console,
) -> Result<bool> {
    loop {
        console.out_raw(&format!("Allow {label}? [y/dry/N]: "))?;
        console.flush_out()?;
        let mut input = String::new();
        io::stdin().read_line(&mut input)?;
        let decision = input.trim().to_lowercase();
        match decision.as_str() {
            "y" | "yes" => return Ok(true),
            "" | "n" | "no" => return Ok(false),
            "dry" | "d" => {
                if let Some(v) = view {
                    console.out(&format!("\nPending {label}:"))?;
                    v.print(label, console)?;
                    console.out("")?;
                } else {
                    console.out(&format!("No pending items for {label}."))?;
                }
            }
            _ => console.out("Please answer 'y', 'n', or 'dry'.")?,
        }
    }
}

fn hash_with_logging(
    label: &str,
    root: &dyn roots::Root,
    entries: &mut [state::Entry],
    state_map: &HashMap<String, state::Entry>,
    mode: config::HashMode,
    console: &Console,
) -> Result<()> {
    let start = Instant::now();
    let pb = start_hash_progress(label, console);
    let hashed = prepare_hashes_with_progress(root, entries, state_map, mode, Some(&pb))?;
    let elapsed = start.elapsed();
    pb.finish_with_message(format!("Hashed {label}: {hashed} files"));
    info!(
        "Hashing {}: {} files ({:?}) took {:.2?}",
        label, hashed, mode, elapsed
    );
    Ok(())
}

#[cfg_attr(not(test), allow(dead_code))]
fn prepare_hashes(
    root: &dyn roots::Root,
    entries: &mut [state::Entry],
    state_map: &HashMap<String, state::Entry>,
    mode: config::HashMode,
) -> Result<usize> {
    prepare_hashes_with_progress(root, entries, state_map, mode, None)
}

fn prepare_hashes_with_progress(
    root: &dyn roots::Root,
    entries: &mut [state::Entry],
    state_map: &HashMap<String, state::Entry>,
    mode: config::HashMode,
    progress: Option<&ProgressBar>,
) -> Result<usize> {
    use crate::roots::EntryKind;
    const BATCH: usize = 64;
    let mut work: Vec<(usize, std::path::PathBuf)> = Vec::new();
    for (idx, entry) in entries.iter_mut().enumerate() {
        if entry.kind != EntryKind::File {
            continue;
        }
        let path = entry.path.clone();
        let need_hash = match mode {
            config::HashMode::Always => true,
            config::HashMode::Balanced => match state_map.get(&path) {
                Some(prev) if prev.kind == EntryKind::File => {
                    prev.size != entry.size || prev.mtime != entry.mtime || prev.hash.is_none()
                }
                _ => true,
            },
        };
        if need_hash {
            work.push((idx, std::path::PathBuf::from(path)));
        } else if let Some(prev) = state_map.get(&path) {
            if let Some(prev_hash) = &prev.hash {
                entry.hash = Some(prev_hash.clone());
            }
        }
    }

    if let Some(pb) = progress {
        pb.set_length(work.len() as u64);
        pb.set_position(0);
    }

    for chunk in work.chunks(BATCH) {
        hash_batch(root, entries, chunk)?;
        if let Some(pb) = progress {
            pb.inc(chunk.len() as u64);
        }
    }
    Ok(work.len())
}

fn hash_batch(
    root: &dyn roots::Root,
    entries: &mut [state::Entry],
    batch: &[(usize, std::path::PathBuf)],
) -> Result<()> {
    if batch.is_empty() {
        return Ok(());
    }
    let paths: Vec<std::path::PathBuf> = batch.iter().map(|(_, p)| p.clone()).collect();
    let hashes = root.hash_files(&paths)?;
    for ((idx, _), hash_hex) in batch.iter().zip(hashes.into_iter()) {
        let bytes = hex::decode(hash_hex.as_bytes())?;
        entries[*idx].hash = Some(bytes);
    }
    Ok(())
}

#[cfg(test)]
mod hash_mode_tests {
    use super::*;
    use crate::roots::LocalRoot;
    use std::collections::HashMap;
    use std::os::unix::fs::PermissionsExt;
    use tempfile::tempdir;

    fn entry_from_fs(path: &std::path::Path, rel: &str) -> state::Entry {
        let meta = std::fs::symlink_metadata(path).unwrap();
        let mtime = meta
            .modified()
            .unwrap()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs() as i64;
        state::Entry {
            path: rel.to_string(),
            kind: roots::EntryKind::File,
            size: meta.len(),
            mtime,
            mode: meta.permissions().mode(),
            hash: None,
            link_target: None,
            deleted: false,
        }
    }

    #[test]
    fn balanced_hashes_when_metadata_changes() -> Result<()> {
        let dir = tempdir()?;
        let file = dir.path().join("file.txt");
        std::fs::write(&file, b"hello world")?;
        let root = LocalRoot::new(dir.path())?;
        let entry = entry_from_fs(&file, "file.txt");

        let mut prev = entry.clone();
        prev.mtime -= 10;
        prev.hash = Some(vec![0xaa]);

        let mut entries = vec![entry.clone()];
        let mut state_map = HashMap::new();
        state_map.insert(entry.path.clone(), prev);

        let hashed = prepare_hashes(&root, &mut entries, &state_map, config::HashMode::Balanced)?;
        assert_eq!(hashed, 1);
        assert!(entries[0].hash.is_some());
        Ok(())
    }

    #[test]
    fn balanced_skips_hash_when_metadata_identical() -> Result<()> {
        let dir = tempdir()?;
        let file = dir.path().join("file.txt");
        std::fs::write(&file, b"hello world")?;
        let root = LocalRoot::new(dir.path())?;
        let entry = entry_from_fs(&file, "file.txt");

        let mut entries = vec![entry.clone()];
        let mut state_map = HashMap::new();
        let mut prev = entry.clone();
        prev.hash = Some(vec![0xca, 0xfe]);
        state_map.insert(entry.path.clone(), prev.clone());

        let hashed = prepare_hashes(&root, &mut entries, &state_map, config::HashMode::Balanced)?;
        assert_eq!(hashed, 0);
        assert_eq!(entries[0].hash, prev.hash);
        Ok(())
    }

    #[test]
    fn balanced_hashes_new_files() -> Result<()> {
        let dir = tempdir()?;
        let file = dir.path().join("new.txt");
        std::fs::write(&file, b"fresh")?;
        let root = LocalRoot::new(dir.path())?;
        let entry = entry_from_fs(&file, "new.txt");

        let mut entries = vec![entry];
        let state_map = HashMap::new();

        let hashed = prepare_hashes(&root, &mut entries, &state_map, config::HashMode::Balanced)?;
        assert_eq!(hashed, 1);
        assert!(entries[0].hash.is_some());
        Ok(())
    }

    #[test]
    fn always_hashes_every_file() -> Result<()> {
        let dir = tempdir()?;
        let file = dir.path().join("file.txt");
        std::fs::write(&file, b"hello world")?;
        let root = LocalRoot::new(dir.path())?;
        let entry = entry_from_fs(&file, "file.txt");
        let mut entries = vec![entry];
        let state_map = HashMap::new();

        let hashed = prepare_hashes(&root, &mut entries, &state_map, config::HashMode::Always)?;
        assert_eq!(hashed, 1);
        assert!(entries[0].hash.is_some());
        Ok(())
    }
}
