use super::{AgentDetector, AgentProcess};
use crate::process::claude::find_claude_processes_in;
use crate::session::parser::get_sessions_internal;
use crate::session::{AgentType, Session};
use sysinfo::System;

pub struct ClaudeDetector;

impl AgentDetector for ClaudeDetector {
    fn name(&self) -> &'static str {
        "Claude Code"
    }

    fn agent_type(&self) -> AgentType {
        AgentType::Claude
    }

    fn find_processes(&self, system: &System) -> Vec<AgentProcess> {
        find_claude_processes_in(system)
            .into_iter()
            .map(|p| AgentProcess {
                pid: p.pid,
                cpu_usage: p.cpu_usage,
                cwd: p.cwd,
            })
            .collect()
    }

    fn find_sessions(&self, processes: &[AgentProcess]) -> Vec<Session> {
        get_sessions_internal(processes, AgentType::Claude)
    }
}
