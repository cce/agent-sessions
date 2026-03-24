//! Standalone test harness that runs the session discovery pipeline
//! without the Tauri UI. Useful for verifying detection on a live machine.
//!
//! Usage: cargo run --bin discover [--watch]

use tauri_temp_lib::agent;
use tauri_temp_lib::discovery::delta::{DeltaTracker, FileStat};
use std::path::PathBuf;

fn main() {
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info"))
        .format_timestamp_millis()
        .init();

    let watch_mode = std::env::args().any(|a| a == "--watch");

    if watch_mode {
        println!("=== Watch mode: polling every 3s (Ctrl-C to stop) ===\n");
        let mut delta = DeltaTracker::new();
        loop {
            run_discovery(&mut Some(&mut delta));
            std::thread::sleep(std::time::Duration::from_secs(3));
            println!("\n--- refresh ---\n");
        }
    } else {
        run_discovery(&mut None);
    }
}

fn run_discovery(delta: &mut Option<&mut DeltaTracker>) {
    let response = agent::get_all_sessions();

    println!(
        "Found {} sessions ({} waiting)\n",
        response.total_count, response.waiting_count
    );

    for session in &response.sessions {
        println!(
            "  [{:?}] {} ({:?})",
            session.agent_type, session.project_name, session.status
        );
        println!("    path: {}", session.project_path);
        println!("    pid: {}, cpu: {:.1}%, tty: {:?}", session.pid, session.cpu_usage, session.tty);
        if let Some(branch) = &session.git_branch {
            println!("    branch: {}", branch);
        }
        if session.active_subagent_count > 0 {
            println!("    subagents: {}", session.active_subagent_count);
        }
        if let Some(msg) = &session.last_message {
            let preview: String = msg.chars().take(80).collect();
            println!(
                "    last_msg ({}): {}{}",
                session.last_message_role.as_deref().unwrap_or("?"),
                preview,
                if msg.chars().count() > 80 { "..." } else { "" }
            );
        }
        println!("    activity: {}", session.last_activity_at);
        println!();
    }

    // If delta tracker is provided, show file change stats
    if let Some(tracker) = delta {
        let home = dirs::home_dir().unwrap();
        let claude_dir = home.join(".claude").join("projects");
        if claude_dir.exists() {
            let mut current_files = Vec::new();
            collect_file_stats(&claude_dir, &mut current_files);
            let result = tracker.compute_delta(current_files);
            if !result.changed.is_empty() || !result.removed.is_empty() {
                println!(
                    "  Delta: {} changed, {} removed, {} unchanged",
                    result.changed.len(),
                    result.removed.len(),
                    result.unchanged.len()
                );
                for f in &result.changed {
                    println!("    changed: {}", f.display());
                }
            } else {
                println!(
                    "  Delta: no changes ({} files tracked)",
                    result.unchanged.len()
                );
            }
        }
    }
}

fn collect_file_stats(dir: &PathBuf, out: &mut Vec<(PathBuf, FileStat)>) {
    if let Ok(entries) = std::fs::read_dir(dir) {
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                collect_file_stats(&path, out);
            } else if path
                .extension()
                .map(|e| e == "jsonl" || e == "ndjson")
                .unwrap_or(false)
            {
                if let Some(stat) = FileStat::from_path(&path) {
                    out.push((path, stat));
                }
            }
        }
    }
}
