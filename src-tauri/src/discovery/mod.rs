pub mod delta;
pub mod watcher;

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;
use tauri::{AppHandle, Emitter};
use tokio::sync::Notify;

use crate::agent;
use crate::session::SessionsResponse;

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum VisibilityTier {
    Foreground,
    BackgroundVisible,
    BackgroundHidden,
}

impl VisibilityTier {
    fn base_interval(&self) -> Duration {
        match self {
            VisibilityTier::Foreground => Duration::from_secs(2),
            VisibilityTier::BackgroundVisible => Duration::from_secs(6),
            VisibilityTier::BackgroundHidden => Duration::from_secs(30),
        }
    }

    /// When FSEvents watcher is active, polling serves as a safety net
    /// rather than the primary detection mechanism, so intervals are longer.
    fn watcher_active_interval(&self) -> Duration {
        match self {
            VisibilityTier::Foreground => Duration::from_secs(5),
            VisibilityTier::BackgroundVisible => Duration::from_secs(15),
            VisibilityTier::BackgroundHidden => Duration::from_secs(60),
        }
    }
}

/// Central coordinator for session discovery.
/// Runs a background loop that:
/// 1. Detects running agent processes (sysinfo)
/// 2. Matches them to session files
/// 3. Emits results to the frontend via Tauri events
/// 4. Adapts poll frequency based on window visibility
/// 5. Wakes immediately on FSEvents file changes
pub struct DiscoveryManager {
    cached_response: std::sync::Mutex<SessionsResponse>,
    visibility: std::sync::Mutex<VisibilityTier>,
    wake_signal: Arc<Notify>,
    watcher_active: AtomicBool,
}

impl DiscoveryManager {
    pub fn new() -> Arc<Self> {
        Arc::new(Self {
            cached_response: std::sync::Mutex::new(SessionsResponse {
                sessions: Vec::new(),
                total_count: 0,
                waiting_count: 0,
            }),
            visibility: std::sync::Mutex::new(VisibilityTier::Foreground),
            wake_signal: Arc::new(Notify::new()),
            watcher_active: AtomicBool::new(false),
        })
    }

    pub fn cached_sessions(&self) -> SessionsResponse {
        self.cached_response.lock().unwrap().clone()
    }

    pub fn set_visibility(&self, tier: VisibilityTier) {
        let mut vis = self.visibility.lock().unwrap();
        if *vis != tier {
            log::info!("Visibility tier changed to {:?}", tier);
            *vis = tier;
            self.wake_signal.notify_one();
        }
    }

    /// Signal the discovery loop to run immediately (used by FSEvents watcher).
    pub fn wake(&self) {
        self.wake_signal.notify_one();
    }

    fn current_interval(&self) -> Duration {
        let tier = *self.visibility.lock().unwrap();
        if self.watcher_active.load(Ordering::Relaxed) {
            tier.watcher_active_interval()
        } else {
            tier.base_interval()
        }
    }

    /// Start the background discovery loop and FSEvents watcher.
    /// Must be called once during app setup.
    pub fn start(self: Arc<Self>, app: AppHandle) {
        let wake = self.wake_signal.clone();

        // Start FSEvents watcher
        match watcher::SessionWatcher::new(wake.clone()) {
            Ok(mut w) => {
                w.watch_agent_directories();
                self.watcher_active.store(true, Ordering::Relaxed);
                log::info!("FSEvents watcher started");
                // Keep watcher alive for app lifetime
                tauri::async_runtime::spawn(async move {
                    let _watcher = w;
                    std::future::pending::<()>().await;
                });
            }
            Err(e) => {
                log::warn!("FSEvents watcher failed to start: {}. Falling back to polling only.", e);
            }
        }

        // Start discovery loop
        let manager = self.clone();
        tauri::async_runtime::spawn(async move {
            // Let the app finish initializing
            tokio::time::sleep(Duration::from_millis(300)).await;

            loop {
                // Run discovery on a blocking thread to avoid starving the async runtime
                let response = tokio::task::spawn_blocking(|| agent::get_all_sessions())
                    .await
                    .unwrap_or_else(|_| SessionsResponse {
                        sessions: Vec::new(),
                        total_count: 0,
                        waiting_count: 0,
                    });

                // Update cache
                *manager.cached_response.lock().unwrap() = response.clone();

                // Push to frontend
                let _ = app.emit("sessions-updated", &response);

                // Wait for interval or wake signal (whichever comes first)
                let interval = manager.current_interval();
                tokio::select! {
                    _ = tokio::time::sleep(interval) => {}
                    _ = wake.notified() => {
                        // Debounce: FSEvents can fire many events in rapid succession
                        tokio::time::sleep(Duration::from_millis(150)).await;
                    }
                }
            }
        });
    }
}
