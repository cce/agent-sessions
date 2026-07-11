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

/// Compute SHA-256 hex digest, matching how Gemini CLI hashes project paths
/// to create the directory names under ~/.gemini/tmp/.
fn sha256_hex(input: &str) -> String {
    // Try the OS shasum command first to avoid reimplementing crypto.
    // Only runs once per process per poll cycle.
    let output = std::process::Command::new("shasum")
        .args(["-a", "256"])
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .spawn()
        .and_then(|mut child| {
            use std::io::Write as IoWrite;
            if let Some(stdin) = child.stdin.as_mut() {
                let _ = stdin.write_all(input.as_bytes());
            }
            child.wait_with_output()
        });
    match output {
        Ok(out) if out.status.success() => {
            let s = String::from_utf8_lossy(&out.stdout);
            // shasum output: "<hex>  -\n"
            s.split_whitespace().next().unwrap_or("").to_string()
        }
        _ => {
            // Fallback: pure-Rust SHA-256 (K/H constants inlined)
            sha256_pure(input.as_bytes())
        }
    }
}

/// Pure-Rust SHA-256 so we don't depend on an external command or crate.
fn sha256_pure(data: &[u8]) -> String {
    use std::fmt::Write;

    const K: [u32; 64] = [
        0x428a2f98, 0x71374491, 0xb5c0fbcf, 0xe9b5dba5,
        0x3956c25b, 0x59f111f1, 0x923f82a4, 0xab1c5ed5,
        0xd807aa98, 0x12835b01, 0x243185be, 0x550c7dc3,
        0x72be5d74, 0x80deb1fe, 0x9bdc06a7, 0xc19bf174,
        0xe49b69c1, 0xefbe4786, 0x0fc19dc6, 0x240ca1cc,
        0x2de92c6f, 0x4a7484aa, 0x5cb0a9dc, 0x76f988da,
        0x983e5152, 0xa831c66d, 0xb00327c8, 0xbf597fc7,
        0xc6e00bf3, 0xd5a79147, 0x06ca6351, 0x14292967,
        0x27b70a85, 0x2e1b2138, 0x4d2c6dfc, 0x53380d13,
        0x650a7354, 0x766a0abb, 0x81c2c92e, 0x92722c85,
        0xa2bfe8a1, 0xa81a664b, 0xc24b8b70, 0xc76c51a3,
        0xd192e819, 0xd6990624, 0xf40e3585, 0x106aa070,
        0x19a4c116, 0x1e376c08, 0x2748774c, 0x34b0bcb5,
        0x391c0cb3, 0x4ed8aa4a, 0x5b9cca4f, 0x682e6ff3,
        0x748f82ee, 0x78a5636f, 0x84c87814, 0x8cc70208,
        0x90befffa, 0xa4506ceb, 0xbef9a3f7, 0xc67178f2,
    ];

    let mut h: [u32; 8] = [
        0x6a09e667, 0xbb67ae85, 0x3c6ef372, 0xa54ff53a,
        0x510e527f, 0x9b05688c, 0x1f83d9ab, 0x5be0cd19,
    ];

    // Pre-processing: padding
    let bit_len = (data.len() as u64) * 8;
    let mut msg = data.to_vec();
    msg.push(0x80);
    while (msg.len() % 64) != 56 {
        msg.push(0);
    }
    msg.extend_from_slice(&bit_len.to_be_bytes());

    // Process each 512-bit (64-byte) block
    for chunk in msg.chunks(64) {
        let mut w = [0u32; 64];
        for i in 0..16 {
            w[i] = u32::from_be_bytes([
                chunk[4 * i],
                chunk[4 * i + 1],
                chunk[4 * i + 2],
                chunk[4 * i + 3],
            ]);
        }
        for i in 16..64 {
            let s0 = w[i - 15].rotate_right(7) ^ w[i - 15].rotate_right(18) ^ (w[i - 15] >> 3);
            let s1 = w[i - 2].rotate_right(17) ^ w[i - 2].rotate_right(19) ^ (w[i - 2] >> 10);
            w[i] = w[i - 16]
                .wrapping_add(s0)
                .wrapping_add(w[i - 7])
                .wrapping_add(s1);
        }

        let [mut a, mut b, mut c, mut d, mut e, mut f, mut g, mut hh] = h;
        for i in 0..64 {
            let s1 = e.rotate_right(6) ^ e.rotate_right(11) ^ e.rotate_right(25);
            let ch = (e & f) ^ ((!e) & g);
            let temp1 = hh
                .wrapping_add(s1)
                .wrapping_add(ch)
                .wrapping_add(K[i])
                .wrapping_add(w[i]);
            let s0 = a.rotate_right(2) ^ a.rotate_right(13) ^ a.rotate_right(22);
            let maj = (a & b) ^ (a & c) ^ (b & c);
            let temp2 = s0.wrapping_add(maj);

            hh = g;
            g = f;
            f = e;
            e = d.wrapping_add(temp1);
            d = c;
            c = b;
            b = a;
            a = temp1.wrapping_add(temp2);
        }

        h[0] = h[0].wrapping_add(a);
        h[1] = h[1].wrapping_add(b);
        h[2] = h[2].wrapping_add(c);
        h[3] = h[3].wrapping_add(d);
        h[4] = h[4].wrapping_add(e);
        h[5] = h[5].wrapping_add(f);
        h[6] = h[6].wrapping_add(g);
        h[7] = h[7].wrapping_add(hh);
    }

    let mut result = String::with_capacity(64);
    for word in &h {
        let _ = write!(result, "{:08x}", word);
    }
    result
}

/// Gemini CLI stores sessions at ~/.gemini/tmp/<sha256(cwd)>/chats/session-*.json.
/// The directory names are SHA-256 hashes of the project's absolute path.
/// We hash each running process's cwd and look for a matching directory.
fn get_gemini_sessions(processes: &[AgentProcess]) -> Vec<Session> {
    let mut sessions = Vec::new();

    let gemini_root = match dirs::home_dir() {
        Some(h) => h.join(".gemini").join("tmp"),
        None => return sessions,
    };

    if !gemini_root.exists() {
        return sessions;
    }

    // Build hash -> process map by hashing each process's cwd
    let mut hash_to_process: HashMap<String, &AgentProcess> = HashMap::new();
    for process in processes {
        if let Some(cwd) = &process.cwd {
            let hash = sha256_hex(&cwd.to_string_lossy());
            hash_to_process.insert(hash, process);
        }
    }

    // Scan hash directories and match against our computed hashes
    let entries = match fs::read_dir(&gemini_root) {
        Ok(e) => e,
        Err(_) => return sessions,
    };

    for entry in entries.flatten() {
        let project_dir = entry.path();
        if !project_dir.is_dir() {
            continue;
        }

        let dir_name = match project_dir.file_name().and_then(|n| n.to_str()) {
            Some(n) => n.to_string(),
            None => continue,
        };

        // Skip non-hash entries
        if dir_name.starts_with('.') || dir_name == "bin" {
            continue;
        }

        let process = match hash_to_process.get(&dir_name) {
            Some(p) => *p,
            None => continue,
        };

        let chats_dir = project_dir.join("chats");
        let search_dir = if chats_dir.exists() {
            chats_dir
        } else {
            project_dir.clone()
        };

        // Find the most recently modified session file
        let mut newest: Option<(PathBuf, std::time::SystemTime)> = None;
        if let Ok(files) = fs::read_dir(&search_dir) {
            for file_entry in files.flatten() {
                let path = file_entry.path();
                let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("");
                if name.starts_with("session-") && name.ends_with(".json") {
                    if let Ok(meta) = file_entry.metadata() {
                        let mtime = meta.modified().unwrap_or(std::time::UNIX_EPOCH);
                        if newest.as_ref().map(|(_, t)| mtime > *t).unwrap_or(true) {
                            newest = Some((path, mtime));
                        }
                    }
                }
            }
        }

        let (session_path, mtime) = match newest {
            Some(s) => s,
            None => continue,
        };

        if let Some(session) = parse_gemini_session(&session_path, mtime, process) {
            sessions.push(session);
        }
    }

    sessions
}

fn parse_gemini_session(
    path: &PathBuf,
    mtime: std::time::SystemTime,
    process: &AgentProcess,
) -> Option<Session> {
    let content = fs::read_to_string(path).ok()?;
    let json: serde_json::Value = serde_json::from_str(&content).ok()?;

    let session_id = json
        .get("sessionId")
        .and_then(|v| v.as_str())
        .unwrap_or_else(|| {
            path.file_stem()
                .and_then(|n| n.to_str())
                .unwrap_or("unknown")
        })
        .to_string();

    // Gemini CLI uses "type" with values "user" and "gemini" (not "role")
    let mut last_message = None;
    let mut last_role = None;
    if let Some(messages) = json.get("messages").and_then(|v| v.as_array()) {
        if let Some(last) = messages.last() {
            let raw_type = last.get("type").and_then(|v| v.as_str());
            // Normalize "gemini" -> "assistant" for consistent status logic
            last_role = raw_type.map(|t| match t {
                "gemini" | "model" => "assistant".to_string(),
                other => other.to_string(),
            });

            // Content is a direct string field
            if let Some(text) = last.get("content").and_then(|v| v.as_str()) {
                last_message = Some(if text.chars().count() > 100 {
                    format!("{}...", text.chars().take(100).collect::<String>())
                } else {
                    text.to_string()
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

    let project_path = process
        .cwd
        .as_ref()
        .map(|p| p.to_string_lossy().to_string())
        .unwrap_or_default();

    let project_name = project_path
        .split('/')
        .filter(|s| !s.is_empty())
        .last()
        .unwrap_or("Unknown")
        .to_string();

    // Use lastUpdated from session JSON if available, fall back to file mtime
    let last_activity_at = json
        .get("lastUpdated")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string())
        .unwrap_or_else(|| {
            chrono::DateTime::<chrono::Utc>::from(mtime)
                .format("%Y-%m-%dT%H:%M:%S%.3fZ")
                .to_string()
        });

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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_sha256_pure_known_vectors() {
        // Verify our SHA-256 implementation against known values
        assert_eq!(
            sha256_pure(b""),
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
        );
        assert_eq!(
            sha256_pure(b"abc"),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
    }

    #[test]
    fn test_sha256_matches_gemini_project_hash() {
        // Verify the hash matches what Gemini CLI produces for a known path
        let hash = sha256_pure(b"/Users/cce/ga/go-algorand");
        assert_eq!(
            hash,
            "081b8d3797a3c81315259b67960a5e687d1fb8cb8746d4ee19e199b959e56aa1"
        );
    }
}
