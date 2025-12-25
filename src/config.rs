use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::path::Path;

#[derive(Debug, Deserialize, Serialize, Clone)]
#[serde(deny_unknown_fields)]
pub struct Config {
    pub root_a: Option<String>,
    pub root_b: Option<String>,
    pub include: Option<Vec<String>>,
    pub ignore: Option<Vec<String>>,
    #[serde(default)]
    pub force: Option<String>,
    /// Hardlink handling mode
    #[serde(default)]
    pub hardlinks: HardlinkMode,
    /// Hashing strategy: `Balanced` hashes when metadata changed, `Always` hashes every file
    #[serde(default)]
    pub hash_mode: HashMode,
    #[serde(default = "Config::default_preserve")]
    pub preserve_owner: bool,
    #[serde(default = "Config::default_preserve")]
    pub preserve_permissions: bool,
    #[serde(default)]
    pub state_db_name: Option<String>,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            root_a: None,
            root_b: None,
            include: None,
            ignore: None,
            force: None,
            hardlinks: HardlinkMode::Copy,
            hash_mode: HashMode::Balanced,
            preserve_owner: Self::default_preserve(),
            preserve_permissions: Self::default_preserve(),
            state_db_name: None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum HashMode {
    #[default]
    Balanced,
    Always,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum HardlinkMode {
    #[default]
    Copy,
    Skip,
    Preserve,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ForceSide {
    RootA,
    RootB,
}

impl ForceSide {
    pub fn parse(value: &str) -> Option<Self> {
        let normalized = value.trim().to_ascii_lowercase();
        match normalized.as_str() {
            "root_a" => Some(ForceSide::RootA),
            "root_b" => Some(ForceSide::RootB),
            _ => None,
        }
    }
}

impl Config {
    pub fn load_from_file(path: &Path) -> Result<Self> {
        if !path.exists() {
            return Ok(Config::default());
        }
        let content = std::fs::read_to_string(path)
            .with_context(|| format!("Failed to read config file {:?}", path))?;
        toml::from_str(&content).with_context(|| format!("Failed to parse config file {:?}", path))
    }

    pub fn force_side(&self) -> Result<Option<ForceSide>> {
        match self.force.as_deref() {
            None => Ok(None),
            Some(value) => {
                let trimmed = value.trim();
                if trimmed.is_empty() || trimmed.eq_ignore_ascii_case("none") {
                    return Ok(None);
                }
                ForceSide::parse(trimmed)
                    .ok_or_else(|| {
                        anyhow::anyhow!("Force option must be 'root_a', 'root_b', or 'none'")
                    })
                    .map(Some)
            }
        }
    }

    fn default_preserve() -> bool {
        true
    }

    pub fn state_db_filename(&self) -> String {
        let name = self
            .state_db_name
            .as_deref()
            .map(|s| s.trim())
            .filter(|s| !s.is_empty())
            .unwrap_or("state");
        let sanitized = sanitize_state_db_name(name);
        if sanitized.ends_with(".db") {
            sanitized
        } else {
            format!("{sanitized}.db")
        }
    }
}

fn sanitize_state_db_name(input: &str) -> String {
    let mut sanitized = String::with_capacity(input.len());
    for ch in input.chars() {
        if ch.is_ascii_alphanumeric() || matches!(ch, '.' | '-' | '_') {
            sanitized.push(ch);
        } else {
            sanitized.push('_');
        }
    }
    if sanitized.is_empty() {
        "state.db".to_string()
    } else {
        sanitized
    }
}
