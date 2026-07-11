pub mod claude;
pub mod codex;
pub mod copilot;
pub mod droid;
pub mod gemini;
pub mod opencode;
pub mod openclaw;

use once_cell::sync::Lazy;
use std::sync::Mutex;
use sysinfo::{ProcessRefreshKind, ProcessesToUpdate, RefreshKind, System, UpdateKind};

use crate::session::{cleanup_stale_status_entries, status_sort_priority, AgentType, Session, SessionsResponse};

/// Common process info shared across agent types
#[derive(Debug, Clone)]
pub struct AgentProcess {
    pub pid: u32,
    pub cpu_usage: f32,
    pub cwd: Option<std::path::PathBuf>,
}

/// Trait for detecting and parsing agent sessions.
/// The system parameter is a pre-refreshed sysinfo::System shared across all detectors.
pub trait AgentDetector: Send + Sync {
    fn name(&self) -> &'static str;
    fn agent_type(&self) -> AgentType;

    /// Extract this agent's processes from the shared, already-refreshed System.
    fn find_processes(&self, system: &System) -> Vec<AgentProcess>;

    /// Parse sessions from data files, matched to running processes.
    fn find_sessions(&self, processes: &[AgentProcess]) -> Vec<Session>;
}

// Single shared System instance, refreshed once per poll cycle
static SHARED_SYSTEM: Lazy<Mutex<System>> = Lazy::new(|| {
    Mutex::new(System::new_with_specifics(
        RefreshKind::new().with_processes(
            ProcessRefreshKind::new()
                .with_cmd(UpdateKind::Always)
                .with_cwd(UpdateKind::Always)
                .with_cpu()
                .with_memory(),
        ),
    ))
});

/// Refresh the shared system's process list once, then run all detectors.
pub fn get_all_sessions() -> SessionsResponse {
    use std::collections::HashSet;

    let mut system = SHARED_SYSTEM.lock().unwrap();
    system.refresh_processes_specifics(
        ProcessesToUpdate::All,
        ProcessRefreshKind::new()
            .with_cmd(UpdateKind::Always)
            .with_cwd(UpdateKind::Always)
            .with_cpu()
            .with_memory(),
    );

    let detectors: Vec<Box<dyn AgentDetector>> = vec![
        Box::new(claude::ClaudeDetector),
        Box::new(opencode::OpenCodeDetector),
        Box::new(codex::CodexDetector),
        Box::new(gemini::GeminiDetector),
        Box::new(copilot::CopilotDetector),
        Box::new(droid::DroidDetector),
        Box::new(openclaw::OpenClawDetector),
    ];

    let mut all_sessions = Vec::new();

    for detector in &detectors {
        let processes = detector.find_processes(&system);
        let sessions = detector.find_sessions(&processes);
        log::info!(
            "{}: found {} processes, {} sessions",
            detector.name(),
            processes.len(),
            sessions.len()
        );
        all_sessions.extend(sessions);
    }

    drop(system);

    // Clean up stale status tracking entries
    let active_ids: HashSet<String> = all_sessions.iter().map(|s| s.id.clone()).collect();
    cleanup_stale_status_entries(&active_ids);

    // Sort by status priority, then by most recent activity
    all_sessions.sort_by(|a, b| {
        let priority_a = status_sort_priority(&a.status);
        let priority_b = status_sort_priority(&b.status);
        if priority_a != priority_b {
            priority_a.cmp(&priority_b)
        } else {
            b.last_activity_at.cmp(&a.last_activity_at)
        }
    });

    let waiting_count = all_sessions
        .iter()
        .filter(|s| matches!(s.status, crate::session::SessionStatus::Waiting))
        .count();
    let total_count = all_sessions.len();

    SessionsResponse {
        sessions: all_sessions,
        total_count,
        waiting_count,
    }
}
