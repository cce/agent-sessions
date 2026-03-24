use super::{AgentDetector, AgentProcess};
use crate::session::{get_github_url, AgentType, Session, SessionStatus};
use crate::terminal::get_tty_for_pid;
use std::collections::HashMap;
use std::fs;
use std::path::PathBuf;
use sysinfo::System;

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

/// Gemini CLI stores sessions at ~/.gemini/tmp/<hash>/chats/session-*.json.
/// The <hash> directories don't correspond to project names, so we extract
/// the project path from the JSON content (projectHash or cwd fields) and
/// match against running process cwds.
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
    let mut cwd_to_process: HashMap<String, &AgentProcess> = HashMap::new();
    for process in processes {
        if let Some(cwd) = &process.cwd {
            cwd_to_process.insert(cwd.to_string_lossy().to_string(), process);
        }
    }

    let mut matched_pids = std::collections::HashSet::new();

    // Scan all hash directories under ~/.gemini/tmp/
    let project_dirs = match fs::read_dir(&gemini_root) {
        Ok(entries) => entries,
        Err(_) => return sessions,
    };

    for entry in project_dirs.flatten() {
        let project_dir = entry.path();
        if !project_dir.is_dir() {
            continue;
        }

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

        // Parse the JSON to extract project path and match to a process
        let content = match fs::read_to_string(&session_path) {
            Ok(c) => c,
            Err(_) => continue,
        };
        let json: serde_json::Value = match serde_json::from_str(&content) {
            Ok(v) => v,
            Err(_) => continue,
        };

        // Try to extract the actual project path from the session JSON.
        // Gemini CLI stores "cwd" or "projectPath" in the session metadata.
        let session_cwd = json
            .get("cwd")
            .and_then(|v| v.as_str())
            .or_else(|| json.get("projectPath").and_then(|v| v.as_str()))
            .map(|s| s.to_string());

        let matched_process = session_cwd.as_ref().and_then(|cwd| {
            let proc = cwd_to_process.get(cwd.as_str())?;
            if matched_pids.contains(&proc.pid) {
                return None;
            }
            Some(*proc)
        });

        // If no cwd in JSON, fall back to matching against all unmatched processes
        // by checking if any process cwd's last component appears in the dir name
        let process = match matched_process {
            Some(p) => p,
            None => {
                let fallback = cwd_to_process.iter().find(|(_, p)| {
                    !matched_pids.contains(&p.pid)
                });
                match fallback {
                    Some((_, p)) => *p,
                    None => continue,
                }
            }
        };

        matched_pids.insert(process.pid);

        if let Some(session) = parse_gemini_session(
            &session_path,
            mtime,
            &json,
            session_cwd.as_deref(),
            process,
        ) {
            sessions.push(session);
        }
    }

    sessions
}

fn parse_gemini_session(
    path: &PathBuf,
    mtime: std::time::SystemTime,
    json: &serde_json::Value,
    session_cwd: Option<&str>,
    process: &AgentProcess,
) -> Option<Session> {
    let session_id = path
        .file_stem()
        .and_then(|n| n.to_str())
        .unwrap_or("unknown")
        .to_string();

    // Gemini CLI uses "type" (not "role") with values "user" and "gemini"
    // for message entries. Some versions may use "role" as well.
    let mut last_message = None;
    let mut last_role = None;
    if let Some(messages) = json.get("messages").and_then(|v| v.as_array()) {
        if let Some(last) = messages.last() {
            // Try "type" first (gemini native), fall back to "role"
            let raw_role = last
                .get("type")
                .and_then(|r| r.as_str())
                .or_else(|| last.get("role").and_then(|r| r.as_str()));
            // Normalize "gemini" -> "assistant" for consistent status logic
            last_role = raw_role.map(|r| match r {
                "gemini" | "model" => "assistant".to_string(),
                other => other.to_string(),
            });

            // Content can be a string or nested in parts
            let text = last
                .get("text")
                .and_then(|v| v.as_str())
                .or_else(|| last.get("content").and_then(|v| v.as_str()))
                .or_else(|| {
                    last.get("parts")
                        .and_then(|v| v.as_array())
                        .and_then(|arr| {
                            arr.iter().find_map(|p| {
                                p.get("text").and_then(|t| t.as_str())
                            })
                        })
                });
            if let Some(t) = text {
                last_message = Some(if t.chars().count() > 100 {
                    format!("{}...", t.chars().take(100).collect::<String>())
                } else {
                    t.to_string()
                });
            }
        }
    }

    let file_recently_modified = mtime
        .elapsed()
        .map(|d| d.as_secs() < 3)
        .unwrap_or(false);

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

    let project_path = session_cwd
        .map(|s| s.to_string())
        .or_else(|| {
            process
                .cwd
                .as_ref()
                .map(|p| p.to_string_lossy().to_string())
        })
        .unwrap_or_default();

    let project_name = project_path
        .split('/')
        .filter(|s| !s.is_empty())
        .last()
        .unwrap_or("Unknown")
        .to_string();

    let last_activity_at = chrono::DateTime::<chrono::Utc>::from(mtime)
        .format("%Y-%m-%dT%H:%M:%S%.3fZ")
        .to_string();

    let github_url = get_github_url(&project_path);

    Some(Session {
        id: session_id,
        agent_type: AgentType::Gemini,
        project_name,
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
