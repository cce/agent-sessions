//! Standalone tool for session discovery.
//!
//! Usage:
//!   discover                  -- list active sessions
//!   discover --watch          -- poll every 3s
//!   discover --json [FILE]    -- output sessions as JSONL

use std::collections::HashMap;
use std::io::Write;
use std::path::PathBuf;
use tauri_temp_lib::agent;
use tauri_temp_lib::discovery::delta::{DeltaTracker, FileStat};
use tauri_temp_lib::terminal;

fn main() {
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info"))
        .format_timestamp_millis()
        .init();

    let args: Vec<String> = std::env::args().collect();
    let watch_mode = args.iter().any(|a| a == "--watch");
    let json_mode = args.iter().any(|a| a == "--json");
    let json_file = args
        .iter()
        .position(|a| a == "--json")
        .and_then(|i| args.get(i + 1))
        .filter(|a| !a.starts_with("--"))
        .cloned();

    let rt = tokio::runtime::Runtime::new().expect("failed to create tokio runtime");

    if watch_mode {
        println!("=== Watch mode: polling every 3s (Ctrl-C to stop) ===\n");
        let mut delta = DeltaTracker::new();
        loop {
            run_discovery(&rt, false, None, &mut Some(&mut delta));
            std::thread::sleep(std::time::Duration::from_secs(3));
            println!("\n--- refresh ---\n");
        }
    } else {
        run_discovery(&rt, json_mode, json_file.as_deref(), &mut None);
    }
}

fn get_tty_to_window(rt: &tokio::runtime::Runtime) -> HashMap<String, String> {
    match rt.block_on(terminal::get_iterm_layout()) {
        Ok(layout) => layout.session_to_window,
        Err(e) => {
            eprintln!("iTerm2 layout unavailable: {}", e);
            HashMap::new()
        }
    }
}

fn run_discovery(
    rt: &tokio::runtime::Runtime,
    json_mode: bool,
    json_file: Option<&str>,
    delta: &mut Option<&mut DeltaTracker>,
) {
    let response = agent::get_all_sessions();
    let tty_to_window = get_tty_to_window(rt);

    if json_mode {
        let mut out: Box<dyn Write> = match json_file {
            Some(path) => {
                let f = std::fs::File::create(path)
                    .unwrap_or_else(|e| panic!("failed to create {}: {}", path, e));
                Box::new(std::io::BufWriter::new(f))
            }
            None => Box::new(std::io::stdout().lock()),
        };
        for session in &response.sessions {
            let window_id = session.tty.as_ref().and_then(|t| tty_to_window.get(t));
            let mut obj: serde_json::Value = serde_json::to_value(session).unwrap();
            if let Some(wid) = window_id {
                obj["windowId"] = serde_json::Value::String(wid.clone());
            }
            writeln!(out, "{}", serde_json::to_string(&obj).unwrap()).unwrap();
        }
        if let Some(path) = json_file {
            eprintln!("Wrote {} sessions to {}", response.sessions.len(), path);
        }
        return;
    }

    println!(
        "Found {} sessions ({} waiting)\n",
        response.total_count, response.waiting_count
    );

    for session in &response.sessions {
        println!(
            "  [{:?}] {} ({:?})",
            session.agent_type, session.project_name, session.status
        );
        println!("    id: {}", session.id);
        println!("    path: {}", session.project_path);
        println!(
            "    pid: {}, cpu: {:.1}%, tty: {:?}",
            session.pid, session.cpu_usage, session.tty
        );
        if let Some(tty) = &session.tty {
            if let Some(window_id) = tty_to_window.get(tty) {
                println!("    window: {}", window_id);
            }
        }
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
