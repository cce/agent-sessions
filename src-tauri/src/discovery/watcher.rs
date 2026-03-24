use std::sync::Arc;
use notify::{Config, RecommendedWatcher, RecursiveMode, Watcher};
use tokio::sync::Notify;

/// Watches agent session directories via FSEvents (macOS) for real-time change detection.
/// When a file changes, it signals the DiscoveryManager to wake up and re-scan.
pub struct SessionWatcher {
    watcher: RecommendedWatcher,
}

impl SessionWatcher {
    pub fn new(wake_signal: Arc<Notify>) -> Result<Self, notify::Error> {
        let watcher = RecommendedWatcher::new(
            move |res: Result<notify::Event, notify::Error>| {
                match res {
                    Ok(event) => {
                        use notify::EventKind;
                        match event.kind {
                            EventKind::Create(_)
                            | EventKind::Modify(_)
                            | EventKind::Remove(_) => {
                                wake_signal.notify_one();
                            }
                            _ => {}
                        }
                    }
                    Err(e) => {
                        log::warn!("File watcher error: {}", e);
                    }
                }
            },
            Config::default(),
        )?;

        Ok(Self { watcher })
    }

    /// Start watching all known agent session directories.
    /// Directories that don't exist yet are silently skipped.
    pub fn watch_agent_directories(&mut self) {
        let home = match dirs::home_dir() {
            Some(h) => h,
            None => return,
        };

        let dirs = [
            home.join(".claude").join("projects"),
            home.join(".codex").join("sessions"),
            home.join(".local")
                .join("share")
                .join("opencode")
                .join("storage"),
            home.join(".gemini").join("tmp"),
            home.join(".copilot").join("session-state"),
            home.join(".factory").join("sessions"),
            home.join(".openclaw").join("agents"),
        ];

        for dir in &dirs {
            if dir.exists() {
                match self.watcher.watch(dir, RecursiveMode::Recursive) {
                    Ok(_) => log::info!("Watching directory: {:?}", dir),
                    Err(e) => log::warn!("Failed to watch {:?}: {}", dir, e),
                }
            } else {
                log::debug!("Skipping non-existent watch dir: {:?}", dir);
            }
        }
    }
}
