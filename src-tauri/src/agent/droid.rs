use super::{AgentDetector, AgentProcess};
use crate::session::{get_github_url, AgentType, Session, SessionStatus};
use crate::terminal::get_tty_for_pid;
use sysinfo::System;
use std::collections::HashMap;
use std::fs::{self, File};
use std::io::{BufRead, BufReader, Seek, SeekFrom};
use std::path::PathBuf;

pub struct DroidDetector;

impl AgentDetector for DroidDetector {
    fn name(&self) -> &'static str {
        "Droid (Factory)"
    }

    fn agent_type(&self) -> AgentType {
        AgentType::Droid
    }

    fn find_processes(&self, system: &System) -> Vec<AgentProcess> {
        let mut processes = Vec::new();
        for (pid, process) in system.processes() {
            let cmd = process.cmd();
            let is_droid = if let Some(first_arg) = cmd.first() {
                let s = first_arg.to_string_lossy().to_lowercase();
                s == "droid" || s.ends_with("/droid")
                    || s == "factory" || s.ends_with("/factory")
            } else {
                false
            };
            if is_droid {
                processes.push(AgentProcess {
                    pid: pid.as_u32(),
                    cpu_usage: process.cpu_usage(),
                    cwd: process.cwd().map(|p| p.to_path_buf()),
                });
            }
        }
        processes
    }

    fn find_sessions(&self, processes: &[AgentProcess]) -> Vec<Session> {
        if processes.is_empty() {
            return Vec::new();
        }
        get_droid_sessions(processes)
    }
}

/// Droid stores sessions at ~/.factory/sessions/**/*.jsonl
/// and exported project logs at ~/.factory/projects/**/*.jsonl
fn get_droid_sessions(processes: &[AgentProcess]) -> Vec<Session> {
    let mut sessions = Vec::new();

    let home = match dirs::home_dir() {
        Some(h) => h,
        None => return sessions,
    };

    let factory_root = home.join(".factory");
    if !factory_root.exists() {
        return sessions;
    }

    // Build cwd -> process map
    let mut cwd_to_process: HashMap<String, &AgentProcess> = HashMap::new();
    for process in processes {
        if let Some(cwd) = &process.cwd {
            cwd_to_process.insert(cwd.to_string_lossy().to_string(), process);
        }
    }

    let mut matched_pids = std::collections::HashSet::new();

    // Scan interactive sessions dir first
    let sessions_dir = factory_root.join("sessions");
    if sessions_dir.exists() {
        scan_droid_dir(&sessions_dir, &cwd_to_process, &mut matched_pids, &mut sessions);
    }

    // Then scan exported project logs
    let projects_dir = factory_root.join("projects");
    if projects_dir.exists() {
        scan_droid_dir(&projects_dir, &cwd_to_process, &mut matched_pids, &mut sessions);
    }

    sessions
}

fn scan_droid_dir(
    dir: &PathBuf,
    cwd_to_process: &HashMap<String, &AgentProcess>,
    matched_pids: &mut std::collections::HashSet<u32>,
    sessions: &mut Vec<Session>,
) {
    // Recursively find JSONL files
    let jsonl_files = find_jsonl_recursive(dir, 3);

    for (file_path, mtime) in jsonl_files {
        // Try to extract cwd from the file
        let file_cwd = extract_cwd_from_droid_jsonl(&file_path);

        let matched_process = file_cwd.as_ref().and_then(|cwd| {
            let proc = cwd_to_process.get(cwd.as_str())?;
            if matched_pids.contains(&proc.pid) {
                return None;
            }
            Some(*proc)
        });

        let process = match matched_process {
            Some(p) => p,
            None => continue,
        };

        matched_pids.insert(process.pid);

        if let Some(session) = parse_droid_session(
            &file_path,
            mtime,
            file_cwd.as_deref().unwrap_or(""),
            process,
        ) {
            sessions.push(session);
        }
    }
}

fn find_jsonl_recursive(
    dir: &PathBuf,
    max_depth: u32,
) -> Vec<(PathBuf, std::time::SystemTime)> {
    let mut files = Vec::new();
    if max_depth == 0 {
        return files;
    }
    if let Ok(entries) = fs::read_dir(dir) {
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                files.extend(find_jsonl_recursive(&path, max_depth - 1));
            } else if path.extension().map(|e| e == "jsonl").unwrap_or(false) {
                let mtime = entry
                    .metadata()
                    .and_then(|m| m.modified())
                    .unwrap_or(std::time::UNIX_EPOCH);
                files.push((path, mtime));
            }
        }
    }
    // Newest first
    files.sort_by(|a, b| b.1.cmp(&a.1));
    files
}

fn extract_cwd_from_droid_jsonl(path: &PathBuf) -> Option<String> {
    let file = File::open(path).ok()?;
    let reader = BufReader::new(file);
    for line in reader.lines().take(20).flatten() {
        if let Ok(val) = serde_json::from_str::<serde_json::Value>(&line) {
            // Check for cwd field at top level or in payload
            if let Some(cwd) = val.get("cwd").and_then(|v| v.as_str()) {
                if cwd.starts_with('/') {
                    return Some(cwd.to_string());
                }
            }
            if let Some(payload) = val.get("payload") {
                if let Some(cwd) = payload.get("cwd").and_then(|v| v.as_str()) {
                    if cwd.starts_with('/') {
                        return Some(cwd.to_string());
                    }
                }
            }
        }
    }
    None
}

fn parse_droid_session(
    path: &PathBuf,
    mtime: std::time::SystemTime,
    project_path: &str,
    process: &AgentProcess,
) -> Option<Session> {
    let file = File::open(path).ok()?;
    let file_size = file.metadata().ok()?.len();

    // Read last 64KB for recent messages
    let mut reader = BufReader::new(file);
    let tail_size: u64 = 65536;
    let start_pos = if file_size > tail_size {
        file_size - tail_size
    } else {
        0
    };
    if start_pos > 0 {
        let _ = reader.seek(SeekFrom::Start(start_pos));
        let mut partial = String::new();
        let _ = reader.read_line(&mut partial);
    }

    let mut session_id = None;
    let mut last_role = None;
    let mut last_message = None;
    let mut last_timestamp = None;

    for line in reader.lines().flatten() {
        if let Ok(val) = serde_json::from_str::<serde_json::Value>(&line) {
            if session_id.is_none() {
                session_id = val
                    .get("sessionId")
                    .or_else(|| val.get("session_id"))
                    .and_then(|v| v.as_str())
                    .map(|s| s.to_string());
            }

            let ts = val.get("timestamp").and_then(|v| v.as_str()).map(|s| s.to_string());
            if ts.is_some() {
                last_timestamp = ts;
            }

            // Check for role in message or payload
            let role = val
                .get("role")
                .or_else(|| val.pointer("/message/role"))
                .or_else(|| val.pointer("/payload/role"))
                .and_then(|v| v.as_str());

            if let Some(r) = role {
                last_role = Some(r.to_string());

                let text = val
                    .get("content")
                    .and_then(|v| v.as_str())
                    .or_else(|| val.pointer("/message/content").and_then(|v| v.as_str()))
                    .or_else(|| val.pointer("/payload/content").and_then(|v| v.as_str()));
                if let Some(t) = text {
                    last_message = Some(if t.chars().count() > 100 {
                        format!("{}...", t.chars().take(100).collect::<String>())
                    } else {
                        t.to_string()
                    });
                }
            }
        }
    }

    let session_id = session_id.or_else(|| {
        path.file_stem()
            .and_then(|n| n.to_str())
            .map(|s| s.to_string())
    })?;

    let file_recently_modified = mtime.elapsed().map(|d| d.as_secs() < 3).unwrap_or(false);

    let status = match last_role.as_deref() {
        Some("assistant") => {
            if file_recently_modified {
                SessionStatus::Processing
            } else {
                SessionStatus::Waiting
            }
        }
        Some("user") => {
            if file_recently_modified {
                SessionStatus::Thinking
            } else {
                SessionStatus::Waiting
            }
        }
        _ => SessionStatus::Idle,
    };

    let project_name = project_path
        .split('/')
        .filter(|s| !s.is_empty())
        .last()
        .unwrap_or("Unknown")
        .to_string();

    let last_activity_at = last_timestamp.unwrap_or_else(|| {
        chrono::DateTime::<chrono::Utc>::from(mtime)
            .format("%Y-%m-%dT%H:%M:%S%.3fZ")
            .to_string()
    });

    let github_url = get_github_url(project_path);

    Some(Session {
        id: session_id,
        agent_type: AgentType::Droid,
        project_name,
        project_path: project_path.to_string(),
        git_branch: None,
        github_url,
        status,
        last_message,
        last_message_role: last_role,
        last_activity_at,
        pid: process.pid,
        cpu_usage: process.cpu_usage,
        active_subagent_count: 0,
        tty: get_tty_for_pid(process.pid).ok(),
    })
}
