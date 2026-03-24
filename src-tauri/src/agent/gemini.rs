use super::{AgentDetector, AgentProcess};
use crate::session::{get_github_url, AgentType, Session, SessionStatus};
use crate::terminal::get_tty_for_pid;
use sysinfo::System;
use std::fs;
use std::path::PathBuf;

pub struct GeminiDetector;

impl AgentDetector for GeminiDetector {
    fn name(&self) -> &'static str {
        "Gemini CLI"
    }

    fn agent_type(&self) -> AgentType {
        AgentType::Gemini
    }

    fn find_processes(&self, system: &System) -> Vec<AgentProcess> {
        let mut processes = Vec::new();
        for (pid, process) in system.processes() {
            let cmd = process.cmd();
            let is_gemini = if let Some(first_arg) = cmd.first() {
                let s = first_arg.to_string_lossy().to_lowercase();
                s == "gemini" || s.ends_with("/gemini")
            } else {
                false
            };
            if is_gemini {
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
        get_gemini_sessions(processes)
    }
}

/// Gemini CLI stores sessions at ~/.gemini/tmp/<project>/chats/session-*.json
fn get_gemini_sessions(processes: &[AgentProcess]) -> Vec<Session> {
    let mut sessions = Vec::new();

    let gemini_root = match dirs::home_dir() {
        Some(h) => h.join(".gemini").join("tmp"),
        None => return sessions,
    };

    if !gemini_root.exists() {
        return sessions;
    }

    // Build cwd -> process map
    let mut cwd_to_process = std::collections::HashMap::new();
    for process in processes {
        if let Some(cwd) = &process.cwd {
            cwd_to_process.insert(cwd.to_string_lossy().to_string(), process);
        }
    }

    // Scan project directories
    let project_dirs = match fs::read_dir(&gemini_root) {
        Ok(entries) => entries,
        Err(_) => return sessions,
    };

    for entry in project_dirs.flatten() {
        let project_dir = entry.path();
        if !project_dir.is_dir() {
            continue;
        }

        // Skip non-project entries
        let dir_name = project_dir
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("");
        if dir_name.starts_with('.') || dir_name == "bin" {
            continue;
        }

        let chats_dir = project_dir.join("chats");
        let search_dir = if chats_dir.exists() {
            chats_dir
        } else {
            project_dir.clone()
        };

        // Find the most recently modified session file
        let mut newest_session: Option<(PathBuf, std::time::SystemTime)> = None;
        if let Ok(entries) = fs::read_dir(&search_dir) {
            for entry in entries.flatten() {
                let path = entry.path();
                let name = path
                    .file_name()
                    .and_then(|n| n.to_str())
                    .unwrap_or("");
                if name.starts_with("session-") && name.ends_with(".json") {
                    if let Ok(meta) = entry.metadata() {
                        let mtime = meta.modified().unwrap_or(std::time::UNIX_EPOCH);
                        if newest_session
                            .as_ref()
                            .map(|(_, t)| mtime > *t)
                            .unwrap_or(true)
                        {
                            newest_session = Some((path, mtime));
                        }
                    }
                }
            }
        }

        let (session_path, mtime) = match newest_session {
            Some(s) => s,
            None => continue,
        };

        // Try to match this project to a running process
        // Gemini uses the project directory name as context
        let matched_process = cwd_to_process
            .iter()
            .find(|(cwd, _)| {
                // The project dir name in ~/.gemini/tmp/ often matches the last path component
                let cwd_last = cwd.split('/').last().unwrap_or("");
                dir_name == cwd_last
            })
            .map(|(_, p)| *p);

        let process = match matched_process {
            Some(p) => p,
            None => continue,
        };

        // Parse session JSON for last message
        if let Some(session) =
            parse_gemini_session(&session_path, mtime, dir_name, process)
        {
            sessions.push(session);
        }
    }

    sessions
}

fn parse_gemini_session(
    path: &PathBuf,
    mtime: std::time::SystemTime,
    project_name: &str,
    process: &AgentProcess,
) -> Option<Session> {
    let content = fs::read_to_string(path).ok()?;
    let json: serde_json::Value = serde_json::from_str(&content).ok()?;

    // Extract session ID from filename
    let session_id = path
        .file_stem()
        .and_then(|n| n.to_str())
        .unwrap_or("unknown")
        .to_string();

    // Try to get last message from conversation array
    let mut last_message = None;
    let mut last_role = None;
    if let Some(messages) = json.get("messages").and_then(|v| v.as_array()) {
        if let Some(last) = messages.last() {
            last_role = last
                .get("role")
                .and_then(|r| r.as_str())
                .map(|s| s.to_string());
            last_message = last
                .get("content")
                .and_then(|c| c.as_str())
                .map(|s| {
                    if s.chars().count() > 100 {
                        format!("{}...", s.chars().take(100).collect::<String>())
                    } else {
                        s.to_string()
                    }
                });
        }
    }

    let file_recently_modified = mtime
        .elapsed()
        .map(|d| d.as_secs() < 3)
        .unwrap_or(false);

    let status = match last_role.as_deref() {
        Some("model") | Some("assistant") => {
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

    let project_path = process
        .cwd
        .as_ref()
        .map(|p| p.to_string_lossy().to_string())
        .unwrap_or_default();

    let last_activity_at = chrono::DateTime::<chrono::Utc>::from(mtime)
        .format("%Y-%m-%dT%H:%M:%S%.3fZ")
        .to_string();

    let github_url = get_github_url(&project_path);

    Some(Session {
        id: session_id,
        agent_type: AgentType::Gemini,
        project_name: project_name.to_string(),
        project_path,
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
