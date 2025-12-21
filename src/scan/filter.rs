use anyhow::{Context, Result};
use globset::{Glob, GlobSet, GlobSetBuilder};
use std::path::{Path, PathBuf};

#[derive(Clone, Debug)]
pub enum ScanTargets {
    All,
    Limited(Vec<PathBuf>),
    None,
}

pub struct Filter {
    include: Option<GlobSet>,
    include_prefixes: Option<Vec<PathBuf>>,
    ignore: GlobSet,
}

impl Filter {
    pub fn new(include_pats: &[String], ignore_pats: &[String]) -> Result<Self> {
        let include = if include_pats.is_empty() {
            None
        } else {
            let mut builder = GlobSetBuilder::new();
            for pat in include_pats {
                builder.add(Glob::new(pat).context("Invalid include glob")?);
            }
            Some(builder.build().context("Failed to build include globset")?)
        };

        let include_prefixes = if include.is_some() {
            compute_prefixes(include_pats)
        } else {
            None
        };

        let mut builder = GlobSetBuilder::new();
        // Always ignore internal .synchi
        builder.add(Glob::new(".synchi/**").unwrap());
        builder.add(Glob::new(".synchi").unwrap());
        for pat in ignore_pats {
            builder.add(Glob::new(pat).context("Invalid ignore glob")?);
        }
        let ignore = builder.build().context("Failed to build ignore globset")?;

        Ok(Self {
            include,
            include_prefixes,
            ignore,
        })
    }

    pub fn is_ignored(&self, path: &Path) -> bool {
        self.ignore.is_match(path)
    }

    pub fn is_included(&self, path: &Path) -> bool {
        if let Some(inc) = &self.include {
            inc.is_match(path)
        } else {
            false
        }
    }

    pub fn scan_targets(&self) -> ScanTargets {
        if self.include.is_none() {
            ScanTargets::None
        } else if let Some(prefixes) = &self.include_prefixes {
            ScanTargets::Limited(prefixes.clone())
        } else {
            ScanTargets::All
        }
    }
}

fn compute_prefixes(patterns: &[String]) -> Option<Vec<PathBuf>> {
    if patterns.is_empty() {
        return None;
    }
    let mut prefixes = Vec::new();
    for pat in patterns {
        if let Some(prefix) = literal_prefix(pat) {
            prefixes.push(prefix);
        } else {
            return None;
        }
    }
    if prefixes.is_empty() {
        return None;
    }
    prefixes.sort();
    prefixes.dedup();
    let mut pruned: Vec<PathBuf> = Vec::new();
    'outer: for prefix in prefixes {
        for existing in &pruned {
            if prefix.starts_with(existing) {
                continue 'outer;
            }
        }
        pruned.push(prefix);
    }
    Some(pruned)
}

fn literal_prefix(pattern: &str) -> Option<PathBuf> {
    let mut prefix = PathBuf::new();
    for part in pattern.split('/') {
        if part.is_empty() {
            continue;
        }
        if part == "**" || contains_glob_meta(part) {
            break;
        }
        prefix.push(part);
    }
    if prefix.as_os_str().is_empty() {
        None
    } else {
        Some(prefix)
    }
}

fn contains_glob_meta(segment: &str) -> bool {
    segment.contains('*')
        || segment.contains('?')
        || segment.contains('[')
        || segment.contains(']')
        || segment.contains('{')
        || segment.contains('}')
}
