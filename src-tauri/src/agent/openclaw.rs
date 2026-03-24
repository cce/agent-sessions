use super::{AgentDetector, AgentProcess};
use crate::session::{get_github_url, AgentType, Session, SessionStatus};
use crate::terminal::get_tty_for_pid;
use sysinfo::System;
use std::fs::{self, File};
use std::io::{BufRead, BufReader, Seek, SeekFrom};
use std::path::PathBuf;

pub struct OpenClawDetector;

impl AgentDetector for OpenClawDetector {
    fn name(&self) -> &'static str {
        "OpenClaw"
    }

    fn agent_type(&self) -> AgentType {
        AgentType::OpenClaw
    }

    fn find_processes(&self, system: &System) -> Vec<AgentProcess> {
        let mut processes = Vec::new();
        for (pid, process) in system.processes() {
            let cmd = process.cmd();
            let is_openclaw = if let Some(first_arg) = cmd.first() {
                let s = first_arg.to_string_lossy().to_lowercase();
                s == "openclaw" || s.ends_with("/openclaw")
                    || s == "clawdbot" || s.ends_with("/clawdbot")
            } else {
                false
            };
            if is_openclaw {
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
        get_openclaw_sessions(processes)
    }
}

/// OpenClaw stores sessions at ~/.openclaw/agents/<agentId>/sessions/*.jsonl
/// Legacy location: ~/.clawdbot/...
/// Respects OPENCLAW_STATE_DIR env var.
fn get_openclaw_sessions(processes: &[AgentProcess]) -> Vec<Session> {
    let mut sessions = Vec::new();

    let state_dir = get_openclaw_state_dir();
    let agents_dir = match state_dir {
        Some(d) if d.join("agents").exists() => d.join("agents"),
        _ => return sessions,
    };

    // Build cwd -> process map
    let mut cwd_to_process = std::collections::HashMap::new();
    for process in processes {
        if let Some(cwd) = &process.cwd {
            cwd_to_process.insert(cwd.to_string_lossy().to_string(), process);
        }
    }

    let mut matched_pids = std::collections::HashSet::new();

    // Scan agent directories
    if let Ok(agent_entries) = fs::read_dir(&agents_dir) {
        for agent_entry in agent_entries.flatten() {
            let sessions_dir = agent_entry.path().join("sessions");
            if !sessions_dir.is_dir() {
                continue;
            }

            // Find JSONL files sorted by mtime
            let mut files: Vec<(PathBuf, std::time::SystemTime)> = fs::read_dir(&sessions_dir)
                .into_iter()
                .flatten()
                .flatten()
                .filter(|e| {
                    let path = e.path();
                    path.extension().map(|ext| ext == "jsonl").unwrap_or(false)
                        && !path
                            .file_name()
                            .and_then(|n| n.to_str())
                            .map(|n| n.contains(".deleted."))
                            .unwrap_or(false)
                })
                .filter_map(|e| {
                    let mtime = e.metadata().and_then(|m| m.modified()).ok()?;
                    Some((e.path(), mtime))
                })
                .collect();
            files.sort_by(|a, b| b.1.cmp(&a.1));

            for (file_path, mtime) in files {
                let file_cwd = extract_cwd(&file_path);
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

                if let Some(session) = parse_openclaw_session(
                    &file_path,
                    mtime,
                    file_cwd.as_deref().unwrap_or(""),
                    process,
                ) {
                    sessions.push(session);
                }
            }
        }
    }

    sessions
}

fn get_openclaw_state_dir() -> Option<PathBuf> {
    if let Ok(dir) = std::env::var("OPENCLAW_STATE_DIR") {
        if !dir.is_empty() {
            let p = PathBuf::from(dir);
            if p.exists() {
                return Some(p);
            }
        }
    }
    let home = dirs::home_dir()?;
    let primary = home.join(".openclaw");
    if primary.exists() {
        return Some(primary);
    }
    let legacy = home.join(".clawdbot");
    if legacy.exists() {
        return Some(legacy);
    }
    None
}

fn extract_cwd(path: &PathBuf) -> Option<String> {
    let file = File::open(path).ok()?;
    let reader = BufReader::new(file);
    for line in reader.lines().take(20).flatten() {
        if let Ok(val) = serde_json::from_str::<serde_json::Value>(&line) {
            if let Some(cwd) = val.get("cwd").and_then(|v| v.as_str()) {
                if cwd.starts_with('/') {
                    return Some(cwd.to_string());
                }
            }
        }
    }
    None
}

fn parse_openclaw_session(
    path: &PathBuf,
    mtime: std::time::SystemTime,
    project_path: &str,
    process: &AgentProcess,
) -> Option<Session> {
    let file = File::open(path).ok()?;
    let file_size = file.metadata().ok()?.len();

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
                    .and_then(|v| v.as_str())
                    .map(|s| s.to_string());
            }

            let ts = val.get("timestamp").and_then(|v| v.as_str()).map(|s| s.to_string());
            if ts.is_some() {
                last_timestamp = ts;
            }

            if let Some(msg) = val.get("message") {
                if let Some(role) = msg.get("role").and_then(|v| v.as_str()) {
                    last_role = Some(role.to_string());
                    let text = msg
                        .get("content")
                        .and_then(|v| v.as_str())
                        .map(|s| {
                            if s.chars().count() > 100 {
                                format!("{}...", s.chars().take(100).collect::<String>())
                            } else {
                                s.to_string()
                            }
                        });
                    if text.is_some() {
                        last_message = text;
                    }
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
        agent_type: AgentType::OpenClaw,
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
