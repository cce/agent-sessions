use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::time::SystemTime;

#[derive(Clone, Debug, PartialEq)]
pub struct FileStat {
    pub mtime: SystemTime,
    pub size: u64,
}

impl FileStat {
    pub fn from_path(path: &PathBuf) -> Option<Self> {
        let metadata = std::fs::metadata(path).ok()?;
        Some(Self {
            mtime: metadata.modified().ok()?,
            size: metadata.len(),
        })
    }
}

pub struct DeltaResult {
    pub changed: Vec<PathBuf>,
    pub removed: Vec<PathBuf>,
    pub unchanged: Vec<PathBuf>,
}

/// Tracks file modification state across poll cycles to avoid re-parsing unchanged files.
pub struct DeltaTracker {
    stats: HashMap<PathBuf, FileStat>,
}

impl DeltaTracker {
    pub fn new() -> Self {
        Self {
            stats: HashMap::new(),
        }
    }

    /// Compare current file stats against previous scan.
    /// Returns which files changed, were removed, or stayed the same.
    pub fn compute_delta(&mut self, current_files: Vec<(PathBuf, FileStat)>) -> DeltaResult {
        let mut changed = Vec::new();
        let mut unchanged = Vec::new();
        let current_paths: HashSet<PathBuf> =
            current_files.iter().map(|(p, _)| p.clone()).collect();

        for (path, stat) in &current_files {
            match self.stats.get(path) {
                Some(old) if *old == *stat => {
                    unchanged.push(path.clone());
                }
                _ => {
                    changed.push(path.clone());
                }
            }
        }

        let removed: Vec<PathBuf> = self
            .stats
            .keys()
            .filter(|p| !current_paths.contains(*p))
            .cloned()
            .collect();

        // Update cached stats
        self.stats.clear();
        for (path, stat) in current_files {
            self.stats.insert(path, stat);
        }

        DeltaResult {
            changed,
            removed,
            unchanged,
        }
    }

    pub fn is_empty(&self) -> bool {
        self.stats.is_empty()
    }
}
