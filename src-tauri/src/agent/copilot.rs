use super::{AgentDetector, AgentProcess};
use crate::session::{get_github_url, AgentType, Session, SessionStatus};
use crate::session::model::JsonlMessage;
use crate::terminal::get_tty_for_pid;
use sysinfo::System;
use std::fs::{self, File};
use std::io::{BufRead, BufReader};
use std::path::PathBuf;

pub struct CopilotDetector;

impl AgentDetector for CopilotDetector {
    fn name(&self) -> &'static str {
        "GitHub Copilot"
    }

    fn agent_type(&self) -> AgentType {
        AgentType::Copilot
    }

    fn find_processes(&self, system: &System) -> Vec<AgentProcess> {
        let mut processes = Vec::new();
        for (pid, process) in system.processes() {
            let cmd = process.cmd();
            let is_copilot = if let Some(first_arg) = cmd.first() {
                let s = first_arg.to_string_lossy().to_lowercase();
                s == "github-copilot" || s.ends_with("/github-copilot")
                    || s == "copilot" || s.ends_with("/copilot")
            } else {
                false
            };
            if is_copilot {
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
        get_copilot_sessions(processes)
    }
}

/// Copilot stores sessions at ~/.copilot/session-state/*.jsonl
fn get_copilot_sessions(processes: &[AgentProcess]) -> Vec<Session> {
    let mut sessions = Vec::new();

    let session_dir = match dirs::home_dir() {
        Some(h) => h.join(".copilot").join("session-state"),
        None => return sessions,
    };

    if !session_dir.exists() {
        return sessions;
    }

    // Get JSONL files sorted by mtime (newest first)
    let mut files: Vec<(PathBuf, std::time::SystemTime)> = fs::read_dir(&session_dir)
        .into_iter()
        .flatten()
        .flatten()
        .filter(|e| {
            e.path()
                .extension()
                .map(|ext| ext == "jsonl")
                .unwrap_or(false)
        })
        .filter_map(|e| {
            let mtime = e.metadata().and_then(|m| m.modified()).ok()?;
            Some((e.path(), mtime))
        })
        .collect();
    files.sort_by(|a, b| b.1.cmp(&a.1));

    // Build cwd -> process map
    let mut cwd_to_process = std::collections::HashMap::new();
    for process in processes {
        if let Some(cwd) = &process.cwd {
            cwd_to_process.insert(cwd.to_string_lossy().to_string(), process);
        }
    }

    // Try to match files to processes via embedded cwd
    let mut matched_pids = std::collections::HashSet::new();

    for (file_path, mtime) in &files {
        // Extract cwd from first lines of JSONL
        let file_cwd = extract_cwd_from_copilot_jsonl(file_path);

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

        if let Some(session) = parse_copilot_session(
            file_path,
            *mtime,
            file_cwd.as_deref().unwrap_or(""),
            process,
        ) {
            sessions.push(session);
        }
    }

    sessions
}

fn extract_cwd_from_copilot_jsonl(path: &PathBuf) -> Option<String> {
    let file = File::open(path).ok()?;
    let reader = BufReader::new(file);
    for line in reader.lines().take(20).flatten() {
        if let Ok(msg) = serde_json::from_str::<JsonlMessage>(&line) {
            if let Some(cwd) = msg.cwd {
                if cwd.starts_with('/') {
                    return Some(cwd);
                }
            }
        }
    }
    None
}

fn parse_copilot_session(
    path: &PathBuf,
    mtime: std::time::SystemTime,
    project_path: &str,
    process: &AgentProcess,
) -> Option<Session> {
    let file = File::open(path).ok()?;
    let reader = BufReader::new(file);

    let mut session_id = None;
    let mut last_role = None;
    let mut last_message = None;
    let mut last_timestamp = None;

    // Read all lines and look at the last ones for status
    let lines: Vec<String> = reader.lines().flatten().collect();
    for line in lines.iter().rev().take(200) {
        if let Ok(msg) = serde_json::from_str::<JsonlMessage>(line) {
            if session_id.is_none() {
                session_id = msg.session_id;
            }
            if last_timestamp.is_none() {
                last_timestamp = msg.timestamp;
            }
            if last_role.is_none() {
                if let Some(content) = &msg.message {
                    if let Some(role) = &content.role {
                        last_role = Some(role.clone());
                        if let Some(c) = &content.content {
                            let text = match c {
                                serde_json::Value::String(s) if !s.is_empty() => Some(s.clone()),
                                serde_json::Value::Array(arr) => arr.iter().find_map(|v| {
                                    v.get("text")
                                        .and_then(|t| t.as_str())
                                        .filter(|s| !s.is_empty())
                                        .map(String::from)
                                }),
                                _ => None,
                            };
                            if let Some(t) = text {
                                last_message = Some(if t.chars().count() > 100 {
                                    format!("{}...", t.chars().take(100).collect::<String>())
                                } else {
                                    t
                                });
                            }
                        }
                    }
                }
            }
            if session_id.is_some() && last_role.is_some() {
                break;
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
        agent_type: AgentType::Copilot,
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
