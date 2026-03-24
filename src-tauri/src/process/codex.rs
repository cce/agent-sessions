use log::debug;
use std::path::PathBuf;
use sysinfo::System;

/// Represents a running Codex CLI process
#[derive(Debug, Clone)]
pub struct CodexProcess {
    pub pid: u32,
    pub cwd: Option<PathBuf>,
    pub cpu_usage: f32,
}

/// Extract Codex processes from a pre-refreshed System instance.
pub fn find_codex_processes_in(system: &System) -> Vec<CodexProcess> {
    debug!("=== Starting Codex process discovery ===");

    let mut processes = Vec::new();

    for (pid, process) in system.processes() {
        let cmd = process.cmd();

        let is_codex = if let Some(first_arg) = cmd.first() {
            let first_arg_str = first_arg.to_string_lossy().to_lowercase();
            first_arg_str == "codex" || first_arg_str.ends_with("/codex")
        } else {
            false
        };

        if is_codex {
            let cwd = process.cwd().map(|p| p.to_path_buf());
            let cpu = process.cpu_usage();

            debug!(
                "Found Codex process: pid={}, cpu={:.1}%, cwd={:?}",
                pid.as_u32(),
                cpu,
                cwd
            );

            processes.push(CodexProcess {
                pid: pid.as_u32(),
                cwd,
                cpu_usage: cpu,
            });
        }
    }

    debug!(
        "Codex process discovery complete: found {} processes",
        processes.len()
    );
    processes
}
