use crate::config;
use clap::{Parser, Subcommand, ValueEnum};
use std::path::PathBuf;

#[derive(Parser)]
#[command(author, version, about, long_about = None)]
pub struct Cli {
    #[arg(short, long, value_name = "FILE")]
    pub config: Option<PathBuf>,

    #[arg(long)]
    pub root_a: Option<String>,

    #[arg(long)]
    pub root_b: Option<String>,

    #[arg(long, value_name = "NAME")]
    pub state_db_name: Option<String>,

    /// Enable verbose output (debug logging)
    #[arg(short, long)]
    pub verbose: bool,

    /// Hash comparison mode (balanced, always)
    #[arg(long, value_enum)]
    pub hash_mode: Option<HashModeArg>,

    /// Force synchronization direction (root_a, root_b, none)
    #[arg(long, value_enum)]
    pub force: Option<ForceArg>,

    /// Hardlink handling mode (copy, skip, preserve)
    #[arg(long, value_enum)]
    pub hardlinks: Option<HardlinkModeArg>,

    #[command(subcommand)]
    pub command: Commands,
}

#[derive(Subcommand)]
pub enum Commands {
    /// Initialize creating state DBs
    Init,
    /// Run synchronization
    Sync {
        #[arg(short, long)]
        dry_run: bool,
        /// Automatically allow all categories (overridden by specific flags)
        #[arg(short = 'y', long = "yes")]
        auto_yes: bool,
        /// Copy A → B policy for this run
        #[arg(long, value_enum)]
        copy_a_to_b: Option<CopyPolicyArg>,
        /// Copy B → A policy for this run
        #[arg(long, value_enum)]
        copy_b_to_a: Option<CopyPolicyArg>,
        /// Delete on A policy for this run
        #[arg(long, value_enum)]
        delete_on_a: Option<DeletePolicyArg>,
        /// Delete on B policy for this run
        #[arg(long, value_enum)]
        delete_on_b: Option<DeletePolicyArg>,
    },
    /// Show status/diff without syncing
    Status,
}

#[derive(Clone, Copy, Debug, ValueEnum)]
pub enum HashModeArg {
    Balanced,
    Always,
}

impl From<HashModeArg> for config::HashMode {
    fn from(value: HashModeArg) -> Self {
        match value {
            HashModeArg::Balanced => config::HashMode::Balanced,
            HashModeArg::Always => config::HashMode::Always,
        }
    }
}

#[derive(Clone, Copy, Debug, ValueEnum)]
pub enum HardlinkModeArg {
    Copy,
    Skip,
    Preserve,
}

impl From<HardlinkModeArg> for config::HardlinkMode {
    fn from(value: HardlinkModeArg) -> Self {
        match value {
            HardlinkModeArg::Copy => config::HardlinkMode::Copy,
            HardlinkModeArg::Skip => config::HardlinkMode::Skip,
            HardlinkModeArg::Preserve => config::HardlinkMode::Preserve,
        }
    }
}

#[derive(Clone, Copy, Debug, ValueEnum)]
pub enum ForceArg {
    RootA,
    RootB,
    None,
}

impl ForceArg {
    pub fn as_config_value(self) -> Option<String> {
        match self {
            ForceArg::RootA => Some("root_a".to_string()),
            ForceArg::RootB => Some("root_b".to_string()),
            ForceArg::None => Some("none".to_string()),
        }
    }
}

#[derive(Clone, Copy, Debug, ValueEnum)]
pub enum CopyPolicyArg {
    Allow,
    Skip,
}

#[derive(Clone, Copy, Debug, ValueEnum)]
pub enum DeletePolicyArg {
    Delete,
    Restore,
    Skip,
}
