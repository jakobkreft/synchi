use anyhow::{Context, Result};
use hostname::get;
use indicatif::ProgressBar;
use std::collections::{HashMap, HashSet};
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

use crate::cli::Commands;
use crate::scan::Entry as ScanEntry;
pub use cli::Cli;
use output::Console;
use roots::{Root, RootSpec};
use transport::CopyBehavior;

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
        .with_writer(std::io::stderr)
        .finish();
    tracing::subscriber::set_global_default(subscriber)
        .context("setting default subscriber failed")?;

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
    if let Some(mode) = cli.hardlinks {
        config.hardlinks = mode.into();
    }
    if let Some(name) = cli.state_db_name {
        config.state_db_name = Some(name);
    }

    let root_a_spec = config.root_a.as_deref().map(RootSpec::parse).transpose()?;
    let root_b_spec = config.root_b.as_deref().map(RootSpec::parse).transpose()?;

    print_effective_config(
        &config,
        root_a_spec.as_ref(),
        root_b_spec.as_ref(),
        &mut console,
    )?;

    let root_a_spec = root_a_spec.context("Root A not defined in config or CLI")?;
    let root_b_spec = root_b_spec.context("Root B not defined in config or CLI")?;
    let db_filename = config.state_db_filename();
    let root_a = match &root_a_spec {
        RootSpec::Local { path } => roots::LocalRoot::new(path)?,
        _ => anyhow::bail!("Root A must be a local path"),
    };

    match &cli.command {
        Commands::Init => {
            info!("Initializing synchi...");

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

            let state_db_path = root_a.path().join(".synchi").join(&db_filename);
            if !state_db_path.exists() {
                console.out("Not initialized. Run 'synchi init' first.")?;
                return Ok(());
            }
            let state_entries = state::StateDb::open_readonly(&state_db_path)?.list_entries()?;

            let prepared =
                prepare_pipeline(&config, &root_a, &root_b_spec, state_entries, &mut console)?;
            let summary = summarize_diffs(&prepared.diffs);
            print_status_summary(
                prepared.scan_a_count,
                prepared.scan_b_count,
                prepared.state_count,
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

            info!(
                "Syncing from {} to {}",
                root_a_spec.display(),
                root_b_spec.display()
            );

            let state_db_path = root_a.path().join(".synchi").join(&db_filename);
            let state_entries = if state_db_path.exists() {
                state::StateDb::open_readonly(&state_db_path)?.list_entries()?
            } else {
                Vec::new()
            };

            let mut prepared =
                prepare_pipeline(&config, &root_a, &root_b_spec, state_entries, &mut console)?;

            let summary = summarize_diffs(&prepared.diffs);
            print_status_summary(
                prepared.scan_a_count,
                prepared.scan_b_count,
                prepared.state_count,
                &summary,
                false,
                &mut console,
            )?;

            let conflict_count = prepared
                .diffs
                .iter()
                .filter(|diff| matches!(diff.action, diff::SyncAction::Conflict(_)))
                .count();
            if conflict_count > 0 {
                console.out(&format!("Found {} conflicts.", conflict_count))?;
                if *dry_run {
                    console.out("Dry run: Skipping conflict resolution.")?;
                } else {
                    let conflicts: Vec<diff::DiffResult> = prepared
                        .diffs
                        .iter()
                        .filter(|diff| matches!(diff.action, diff::SyncAction::Conflict(_)))
                        .cloned()
                        .collect();
                    let decisions = ui::Ui::resolve_conflicts(conflicts)?;
                    apply_conflict_decisions(&mut prepared.diffs, &decisions);
                }
            }

            let pending = PendingSets::from_diffs(&prepared.diffs);
            let policy = collect_policy(
                &pending,
                PolicyPrompt {
                    copy_a_override: *copy_a_to_b,
                    copy_b_override: *copy_b_to_a,
                    delete_a_override: *delete_on_a,
                    delete_b_override: *delete_on_b,
                    dry_run: *dry_run,
                    auto_yes: *auto_yes,
                },
                &mut console,
            )?;
            apply_policy_to_diffs(&mut prepared.diffs, &policy)?;

            let diffs_for_links = if prepared.preserve_mode {
                Some(prepared.diffs.clone())
            } else {
                None
            };
            let mut plan = plan::PlanBuilder::build(prepared.diffs);

            if prepared.preserve_mode {
                if let (Some(groups_a), Some(groups_b)) = (
                    prepared.hardlink_groups_a.as_ref(),
                    prepared.hardlink_groups_b.as_ref(),
                ) {
                    let allow_copy_a_to_b = policy.copy_a_to_b == CopyPolicy::Allow;
                    let allow_copy_b_to_a = policy.copy_b_to_a == CopyPolicy::Allow;
                    plan::apply_hardlink_preserve(plan::HardlinkPreserveInputs {
                        plan: &mut plan,
                        diffs: diffs_for_links.as_ref().unwrap(),
                        groups_a,
                        groups_b,
                        scan_a: prepared.scan_a_for_links.as_ref().unwrap(),
                        scan_b: prepared.scan_b_for_links.as_ref().unwrap(),
                        allow_copy_a_to_b,
                        allow_copy_b_to_a,
                    });
                }
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
                console.out(&format!(
                    "Overall Duration: {:.2?}",
                    overall_start.elapsed()
                ))?;
            } else {
                ensure_root_a_ready(&root_a)?;
                let lock_info = lock_info_string();
                let lock_name = format!("{}.lock", db_filename);
                let _lock_a = state::Lock::acquire(&root_a, &lock_name, &lock_info)?;
                ensure_root_b_marker(prepared.root_b.as_ref())?;

                let state_db_path = root_a.path().join(".synchi").join(&db_filename);
                std::fs::create_dir_all(state_db_path.parent().unwrap())?;
                let db = state::StateDb::open(&state_db_path)?;
                db.refresh_metadata(&prepared.scan_a_state)?;
                db.queue_plan(&plan)?;
                let mut journal = journal::Journal::new();
                let copy_behavior = CopyBehavior {
                    preserve_owner: config.preserve_owner,
                    preserve_permissions: config.preserve_permissions,
                };
                let executor = apply::Executor::new(
                    &root_a,
                    prepared.root_b.as_ref(),
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

struct PreparedSync {
    root_b: Box<dyn roots::Root>,
    diffs: Vec<diff::DiffResult>,
    scan_a_count: usize,
    scan_b_count: usize,
    state_count: usize,
    scan_a_state: Vec<state::Entry>,
    hardlink_groups_a: Option<scan::HardlinkGroups>,
    hardlink_groups_b: Option<scan::HardlinkGroups>,
    scan_a_for_links: Option<Vec<ScanEntry>>,
    scan_b_for_links: Option<Vec<ScanEntry>>,
    preserve_mode: bool,
}

fn prepare_pipeline(
    config: &config::Config,
    root_a: &roots::LocalRoot,
    root_b_spec: &RootSpec,
    state_entries: Vec<state::Entry>,
    console: &mut Console,
) -> Result<PreparedSync> {
    let root_b = root_b_spec.root()?;
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
    if include_patterns.is_empty() {
        tracing::warn!("Include patterns are empty; nothing will be scanned or synced.");
    }
    let filter = scan::Filter::new(include_patterns, ignore_patterns)?;
    let hardlink_mode = config.hardlinks;

    let label_a = format!("Root A ({})", root_a.path().display());
    info!("Scanning {}", label_a);
    let mut scan_a = run_scan_with_progress(
        &label_a,
        Some(state_hint),
        |pb| scan::LocalScanner::new(root_a, &filter).scan_with_progress(Some(pb)),
        console,
    )?;

    let mut scan_b = scan_root_b(root_b.as_ref(), &filter, hardlink_mode, state_hint, console)?;

    if !matches!(hardlink_mode, config::HardlinkMode::Copy)
        && (scan::has_missing_inode(&scan_a) || scan::has_missing_inode(&scan_b))
    {
        anyhow::bail!("Hardlink modes require inode/device IDs");
    }

    let preserve_mode = matches!(hardlink_mode, config::HardlinkMode::Preserve);
    let hardlink_groups_a = if preserve_mode {
        Some(scan::hardlink_groups(&scan_a))
    } else {
        None
    };
    let hardlink_groups_b = if preserve_mode {
        Some(scan::hardlink_groups(&scan_b))
    } else {
        None
    };

    let hardlink_skip = if matches!(hardlink_mode, config::HardlinkMode::Skip) {
        hardlink_skip_paths(&scan_a, &scan_b)
    } else {
        HashSet::new()
    };
    if !hardlink_skip.is_empty() {
        tracing::warn!(
            "Skipping {} hard-linked paths (hardlinks=skip).",
            hardlink_skip.len()
        );
        filter_scan_entries(&mut scan_a, &hardlink_skip);
        filter_scan_entries(&mut scan_b, &hardlink_skip);
    }

    let scan_a_count = scan_a.len();
    let scan_b_count = scan_b.len();

    hash_with_logging(
        "Root A",
        root_a,
        &mut scan_a,
        &state_map,
        config.hash_mode,
        console,
    )?;
    let scan_a_state: Vec<state::Entry> = scan_a.iter().map(ScanEntry::to_state).collect();
    hash_with_logging(
        "Root B",
        root_b.as_ref(),
        &mut scan_b,
        &state_map,
        config.hash_mode,
        console,
    )?;

    let scan_a_for_links = if preserve_mode {
        Some(scan_a.clone())
    } else {
        None
    };
    let scan_b_for_links = if preserve_mode {
        Some(scan_b.clone())
    } else {
        None
    };

    let mut state_a = state_entries.clone();
    let mut state_b = state_entries;
    if !hardlink_skip.is_empty() {
        filter_state_entries(&mut state_a, &hardlink_skip);
        filter_state_entries(&mut state_b, &hardlink_skip);
    }

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

    Ok(PreparedSync {
        root_b,
        diffs,
        scan_a_count,
        scan_b_count,
        state_count,
        scan_a_state,
        hardlink_groups_a,
        hardlink_groups_b,
        scan_a_for_links,
        scan_b_for_links,
        preserve_mode,
    })
}

fn scan_root_b(
    root_b: &dyn roots::Root,
    filter: &scan::Filter,
    hardlink_mode: config::HardlinkMode,
    state_hint: u64,
    console: &Console,
) -> Result<Vec<ScanEntry>> {
    match root_b.kind() {
        roots::RootType::Ssh => {
            let ssh_root = root_b
                .as_any()
                .downcast_ref::<roots::SshRoot>()
                .context("Root B is not SSH")?;
            let caps = ssh_root.probe_caps()?;
            let label_b = format!("Root B ({})", ssh_root.path().display());
            info!("Scanning {}", label_b);
            if !matches!(hardlink_mode, config::HardlinkMode::Copy) && !caps.has_find_inode {
                anyhow::bail!("Hardlink modes require remote `find` with %D/%i support");
            }
            run_scan_with_progress(
                &label_b,
                Some(state_hint),
                |pb| scan::RemoteScanner::new(ssh_root, filter, caps).scan_with_progress(Some(pb)),
                console,
            )
        }
        roots::RootType::Local => {
            let local_b = root_b
                .as_any()
                .downcast_ref::<roots::LocalRoot>()
                .context("Unsupported root type for B")?;
            let label_b = format!("Root B ({})", local_b.path().display());
            info!("Scanning {}", label_b);
            run_scan_with_progress(
                &label_b,
                Some(state_hint),
                |pb| scan::LocalScanner::new(local_b, filter).scan_with_progress(Some(pb)),
                console,
            )
        }
    }
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
    console.out(&format!("Hardlink mode: {:?}", config.hardlinks))?;
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
) -> Result<Vec<ScanEntry>>
where
    F: FnOnce(&ProgressBar) -> Result<Vec<ScanEntry>>,
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

fn apply_conflict_decisions(diffs: &mut [diff::DiffResult], decisions: &[ui::ConflictDecision]) {
    let decision_map: HashMap<&str, diff::SyncAction> = decisions
        .iter()
        .map(|d| (d.path.as_str(), d.action.clone()))
        .collect();
    for diff in diffs {
        if let Some(action) = decision_map.get(diff.path.as_str()) {
            diff.action = action.clone();
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

fn ensure_root_a_ready(root: &dyn roots::Root) -> Result<()> {
    root.mkdirs(Path::new(".synchi"))?;
    let mut cursor = Cursor::new(Vec::from("synchi access check"));
    root.write_file(Path::new(".synchi/.access_check"), &mut cursor)?;
    let _ = root.remove_file(Path::new(".synchi/.access_check"));
    Ok(())
}

fn ensure_root_b_marker(root: &dyn roots::Root) -> Result<()> {
    root.mkdirs(Path::new(".synchi"))?;
    Ok(())
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum CopyPolicy {
    Allow,
    Skip,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum DeletePolicy {
    Delete,
    Restore,
    Skip,
}

struct Policy {
    copy_a_to_b: CopyPolicy,
    copy_b_to_a: CopyPolicy,
    delete_on_a: DeletePolicy,
    delete_on_b: DeletePolicy,
}

struct PendingSets {
    copy_a_to_b: Vec<String>,
    copy_b_to_a: Vec<String>,
    delete_a: Vec<String>,
    delete_b: Vec<String>,
}

impl PendingSets {
    fn from_diffs(diffs: &[diff::DiffResult]) -> Self {
        let mut pending = Self {
            copy_a_to_b: Vec::new(),
            copy_b_to_a: Vec::new(),
            delete_a: Vec::new(),
            delete_b: Vec::new(),
        };
        for diff in diffs {
            match diff.action {
                diff::SyncAction::CopyAtoB => pending.copy_a_to_b.push(diff.path.clone()),
                diff::SyncAction::CopyBtoA => pending.copy_b_to_a.push(diff.path.clone()),
                diff::SyncAction::DeleteA => pending.delete_a.push(diff.path.clone()),
                diff::SyncAction::DeleteB => pending.delete_b.push(diff.path.clone()),
                _ => {}
            }
        }
        pending
    }
}

struct PolicyPrompt {
    copy_a_override: Option<cli::CopyPolicyArg>,
    copy_b_override: Option<cli::CopyPolicyArg>,
    delete_a_override: Option<cli::DeletePolicyArg>,
    delete_b_override: Option<cli::DeletePolicyArg>,
    dry_run: bool,
    auto_yes: bool,
}

fn collect_policy(
    pending: &PendingSets,
    prompt: PolicyPrompt,
    console: &mut Console,
) -> Result<Policy> {
    Ok(Policy {
        copy_a_to_b: resolve_copy_policy(
            "Copy A → B",
            &pending.copy_a_to_b,
            prompt.copy_a_override,
            prompt.dry_run,
            prompt.auto_yes,
            console,
        )?,
        copy_b_to_a: resolve_copy_policy(
            "Copy B → A",
            &pending.copy_b_to_a,
            prompt.copy_b_override,
            prompt.dry_run,
            prompt.auto_yes,
            console,
        )?,
        delete_on_a: resolve_delete_policy(
            "Delete on A",
            &pending.delete_a,
            prompt.delete_a_override,
            prompt.dry_run,
            prompt.auto_yes,
            console,
        )?,
        delete_on_b: resolve_delete_policy(
            "Delete on B",
            &pending.delete_b,
            prompt.delete_b_override,
            prompt.dry_run,
            prompt.auto_yes,
            console,
        )?,
    })
}

fn resolve_copy_policy(
    label: &str,
    paths: &[String],
    override_choice: Option<cli::CopyPolicyArg>,
    dry_run: bool,
    auto_yes: bool,
    console: &mut Console,
) -> Result<CopyPolicy> {
    if paths.is_empty() {
        return Ok(CopyPolicy::Allow);
    }
    if let Some(value) = override_choice {
        return Ok(match value {
            cli::CopyPolicyArg::Allow => CopyPolicy::Allow,
            cli::CopyPolicyArg::Skip => CopyPolicy::Skip,
        });
    }
    if dry_run || auto_yes {
        return Ok(CopyPolicy::Allow);
    }
    prompt_copy_policy(label, paths, console)
}

fn resolve_delete_policy(
    label: &str,
    paths: &[String],
    override_choice: Option<cli::DeletePolicyArg>,
    dry_run: bool,
    auto_yes: bool,
    console: &mut Console,
) -> Result<DeletePolicy> {
    if paths.is_empty() {
        return Ok(DeletePolicy::Delete);
    }
    if let Some(value) = override_choice {
        return Ok(match value {
            cli::DeletePolicyArg::Delete => DeletePolicy::Delete,
            cli::DeletePolicyArg::Restore => DeletePolicy::Restore,
            cli::DeletePolicyArg::Skip => DeletePolicy::Skip,
        });
    }
    if dry_run || auto_yes {
        return Ok(DeletePolicy::Delete);
    }
    prompt_delete_policy(label, paths, console)
}

fn prompt_copy_policy(label: &str, paths: &[String], console: &mut Console) -> Result<CopyPolicy> {
    loop {
        console.out_raw(&format!(
            "Allow {label}? [y]es [n]o [l]ist [h]elp (default: n): "
        ))?;
        console.flush_out()?;
        let mut input = String::new();
        io::stdin().read_line(&mut input)?;
        let decision = input.trim().to_ascii_lowercase();
        match decision.as_str() {
            "y" | "yes" => return Ok(CopyPolicy::Allow),
            "" | "n" | "no" => return Ok(CopyPolicy::Skip),
            "l" | "list" => {
                print_pending_list(label, paths, console)?;
            }
            "h" | "help" | "?" => {
                console.out("y = allow, n = skip, l = list pending paths")?;
            }
            _ => console.out("Please answer y, n, l, or h.")?,
        }
    }
}

fn prompt_delete_policy(
    label: &str,
    paths: &[String],
    console: &mut Console,
) -> Result<DeletePolicy> {
    loop {
        console.out_raw(&format!(
            "Action for {label}? [y]es [n]o [l]ist [h]elp (default: n): "
        ))?;
        console.flush_out()?;
        let mut input = String::new();
        io::stdin().read_line(&mut input)?;
        let decision = input.trim().to_ascii_lowercase();
        match decision.as_str() {
            "y" | "yes" => return Ok(DeletePolicy::Delete),
            "" | "n" | "no" => return Ok(DeletePolicy::Skip),
            "l" | "list" => {
                print_pending_list(label, paths, console)?;
            }
            "h" | "help" | "?" => {
                console.out(
                    "y = allow delete, n = skip delete, l = list pending paths (restore is CLI-only: --delete-on-a/--delete-on-b restore)",
                )?;
            }
            _ => console.out("Please answer y, n, l, or h.")?,
        }
    }
}

fn print_pending_list(label: &str, paths: &[String], console: &mut Console) -> Result<()> {
    console.out(&format!("\nPending {label}:"))?;
    for path in paths {
        console.out(&format!("{label} {path}"))?;
    }
    console.out("")?;
    Ok(())
}

fn apply_policy_to_diffs(diffs: &mut [diff::DiffResult], policy: &Policy) -> Result<()> {
    for diff in diffs {
        match diff.action {
            diff::SyncAction::CopyAtoB => {
                if policy.copy_a_to_b == CopyPolicy::Skip {
                    diff.action = diff::SyncAction::NoOp;
                }
            }
            diff::SyncAction::CopyBtoA => {
                if policy.copy_b_to_a == CopyPolicy::Skip {
                    diff.action = diff::SyncAction::NoOp;
                }
            }
            diff::SyncAction::DeleteA => match policy.delete_on_a {
                DeletePolicy::Delete => {}
                DeletePolicy::Skip => diff.action = diff::SyncAction::NoOp,
                DeletePolicy::Restore => {
                    if diff.change_a.entry_now.is_none() {
                        anyhow::bail!("Cannot restore {}: missing source on A", diff.path);
                    }
                    diff.action = diff::SyncAction::CopyAtoB;
                }
            },
            diff::SyncAction::DeleteB => match policy.delete_on_b {
                DeletePolicy::Delete => {}
                DeletePolicy::Skip => diff.action = diff::SyncAction::NoOp,
                DeletePolicy::Restore => {
                    if diff.change_b.entry_now.is_none() {
                        anyhow::bail!("Cannot restore {}: missing source on B", diff.path);
                    }
                    diff.action = diff::SyncAction::CopyBtoA;
                }
            },
            _ => {}
        }
    }
    Ok(())
}

#[cfg(test)]
mod policy_tests {
    use super::*;
    use crate::diff::{ChangeType, SideChange};
    use crate::roots::EntryKind;

    fn make_scan_entry(path: &str) -> ScanEntry {
        ScanEntry {
            path: path.to_string(),
            kind: EntryKind::File,
            size: 1,
            mtime: 0,
            mode: 0o644,
            nlink: 1,
            dev: 1,
            inode: 1,
            hash: None,
            link_target: None,
        }
    }

    fn make_delete_a_diff(path: &str, has_source: bool) -> diff::DiffResult {
        let entry_a = if has_source {
            Some(make_scan_entry(path))
        } else {
            None
        };
        diff::DiffResult {
            path: path.to_string(),
            action: diff::SyncAction::DeleteA,
            change_a: SideChange {
                change: ChangeType::Unchanged,
                entry_now: entry_a,
                entry_prev: None,
            },
            change_b: SideChange {
                change: ChangeType::Deleted,
                entry_now: None,
                entry_prev: None,
            },
        }
    }

    #[test]
    fn restore_turns_delete_into_copy() -> Result<()> {
        let mut diffs = vec![make_delete_a_diff("file.txt", true)];
        let policy = Policy {
            copy_a_to_b: CopyPolicy::Allow,
            copy_b_to_a: CopyPolicy::Allow,
            delete_on_a: DeletePolicy::Restore,
            delete_on_b: DeletePolicy::Delete,
        };
        apply_policy_to_diffs(&mut diffs, &policy)?;
        assert!(matches!(diffs[0].action, diff::SyncAction::CopyAtoB));
        Ok(())
    }

    #[test]
    fn restore_requires_source() {
        let mut diffs = vec![make_delete_a_diff("missing.txt", false)];
        let policy = Policy {
            copy_a_to_b: CopyPolicy::Allow,
            copy_b_to_a: CopyPolicy::Allow,
            delete_on_a: DeletePolicy::Restore,
            delete_on_b: DeletePolicy::Delete,
        };
        let result = apply_policy_to_diffs(&mut diffs, &policy);
        assert!(result.is_err());
    }

    #[test]
    fn skip_copy_turns_into_noop() -> Result<()> {
        let mut diffs = vec![diff::DiffResult {
            path: "copy.txt".to_string(),
            action: diff::SyncAction::CopyAtoB,
            change_a: SideChange {
                change: ChangeType::Created,
                entry_now: Some(make_scan_entry("copy.txt")),
                entry_prev: None,
            },
            change_b: SideChange {
                change: ChangeType::Unchanged,
                entry_now: None,
                entry_prev: None,
            },
        }];
        let policy = Policy {
            copy_a_to_b: CopyPolicy::Skip,
            copy_b_to_a: CopyPolicy::Allow,
            delete_on_a: DeletePolicy::Delete,
            delete_on_b: DeletePolicy::Delete,
        };
        apply_policy_to_diffs(&mut diffs, &policy)?;
        assert!(matches!(diffs[0].action, diff::SyncAction::NoOp));
        Ok(())
    }
}

fn hash_with_logging(
    label: &str,
    root: &dyn roots::Root,
    entries: &mut [ScanEntry],
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

#[cfg(test)]
fn prepare_hashes(
    root: &dyn roots::Root,
    entries: &mut [ScanEntry],
    state_map: &HashMap<String, state::Entry>,
    mode: config::HashMode,
) -> Result<usize> {
    prepare_hashes_with_progress(root, entries, state_map, mode, None)
}

fn prepare_hashes_with_progress(
    root: &dyn roots::Root,
    entries: &mut [ScanEntry],
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
    entries: &mut [ScanEntry],
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

fn hardlink_skip_paths(scan_a: &[ScanEntry], scan_b: &[ScanEntry]) -> HashSet<String> {
    let mut skip = hardlink_group_paths(scan_a);
    skip.extend(hardlink_group_paths(scan_b));
    skip
}

fn hardlink_group_paths(entries: &[ScanEntry]) -> HashSet<String> {
    let mut skip = HashSet::new();
    for paths in scan::hardlink_groups(entries).values() {
        for path in paths {
            skip.insert(path.clone());
        }
    }
    skip
}

fn filter_scan_entries(entries: &mut Vec<ScanEntry>, skip: &HashSet<String>) {
    if skip.is_empty() {
        return;
    }
    entries.retain(|entry| !skip.contains(&entry.path));
}

fn filter_state_entries(entries: &mut Vec<state::Entry>, skip: &HashSet<String>) {
    if skip.is_empty() {
        return;
    }
    entries.retain(|entry| !skip.contains(&entry.path));
}

#[cfg(test)]
mod hardlink_skip_tests {
    use super::*;
    use crate::roots::EntryKind;

    fn make_entry(path: &str, kind: EntryKind, nlink: u64, dev: u64, inode: u64) -> ScanEntry {
        ScanEntry {
            path: path.to_string(),
            kind,
            size: 0,
            mtime: 0,
            mode: 0o644,
            nlink,
            dev,
            inode,
            hash: None,
            link_target: None,
        }
    }

    #[test]
    fn hardlink_skip_unions_both_sides() {
        let scan_a = vec![
            make_entry("file_a1.txt", EntryKind::File, 2, 1, 10),
            make_entry("file_a2.txt", EntryKind::File, 2, 1, 10),
            make_entry("dir", EntryKind::Dir, 2, 2, 20),
        ];
        let scan_b = vec![
            make_entry("file_b1.txt", EntryKind::File, 3, 3, 30),
            make_entry("file_b2.txt", EntryKind::File, 3, 3, 30),
        ];
        let skip = hardlink_skip_paths(&scan_a, &scan_b);
        assert!(skip.contains("file_a1.txt"));
        assert!(skip.contains("file_a2.txt"));
        assert!(skip.contains("file_b1.txt"));
        assert!(skip.contains("file_b2.txt"));
        assert!(!skip.contains("dir"));
    }
}

#[cfg(test)]
mod hash_mode_tests {
    use super::*;
    use crate::roots::LocalRoot;
    use std::collections::HashMap;
    use std::os::unix::fs::{MetadataExt, PermissionsExt};
    use tempfile::tempdir;

    fn entry_from_fs(path: &std::path::Path, rel: &str) -> ScanEntry {
        let meta = std::fs::symlink_metadata(path).unwrap();
        let mtime = meta
            .modified()
            .unwrap()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs() as i64;
        ScanEntry {
            path: rel.to_string(),
            kind: roots::EntryKind::File,
            size: meta.len(),
            mtime,
            mode: meta.permissions().mode(),
            nlink: meta.nlink(),
            dev: meta.dev(),
            inode: meta.ino(),
            hash: None,
            link_target: None,
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
        state_map.insert(entry.path.clone(), prev.to_state());

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
        state_map.insert(entry.path.clone(), prev.to_state());

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
