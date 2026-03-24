use crate::process::{find_claude_processes_in, is_orphaned_process, ClaudeProcess};
use std::path::PathBuf;
use sysinfo::{ProcessRefreshKind, ProcessesToUpdate, RefreshKind, System, UpdateKind};

fn make_refreshed_system() -> System {
    let mut system = System::new_with_specifics(
        RefreshKind::new().with_processes(
            ProcessRefreshKind::new()
                .with_cmd(UpdateKind::Always)
                .with_cwd(UpdateKind::Always)
                .with_cpu()
                .with_memory(),
        ),
    );
    system.refresh_processes_specifics(
        ProcessesToUpdate::All,
        ProcessRefreshKind::new()
            .with_cmd(UpdateKind::Always)
            .with_cwd(UpdateKind::Always)
            .with_cpu()
            .with_memory(),
    );
    system
}

#[test]
fn test_claude_process_creation() {
    let process = ClaudeProcess {
        pid: 12345,
        cwd: Some(PathBuf::from("/Users/test/Projects/my-project")),
        cpu_usage: 5.5,
        memory: 1024,
    };

    assert_eq!(process.pid, 12345);
    assert_eq!(
        process.cwd,
        Some(PathBuf::from("/Users/test/Projects/my-project"))
    );
    assert_eq!(process.cpu_usage, 5.5);
    assert_eq!(process.memory, 1024);
}

#[test]
fn test_claude_process_without_cwd() {
    let process = ClaudeProcess {
        pid: 99999,
        cwd: None,
        cpu_usage: 0.0,
        memory: 0,
    };

    assert_eq!(process.pid, 99999);
    assert!(process.cwd.is_none());
}

#[test]
fn test_claude_process_clone() {
    let process = ClaudeProcess {
        pid: 12345,
        cwd: Some(PathBuf::from("/test/path")),
        cpu_usage: 10.0,
        memory: 2048,
    };

    let cloned = process.clone();
    assert_eq!(process.pid, cloned.pid);
    assert_eq!(process.cwd, cloned.cwd);
    assert_eq!(process.cpu_usage, cloned.cpu_usage);
    assert_eq!(process.memory, cloned.memory);
}

#[test]
fn test_claude_process_serialization() {
    let process = ClaudeProcess {
        pid: 12345,
        cwd: Some(PathBuf::from("/test/path")),
        cpu_usage: 5.5,
        memory: 1024,
    };

    let json = serde_json::to_string(&process).unwrap();
    assert!(json.contains("12345"));
    assert!(json.contains("5.5"));
}

#[test]
fn test_find_claude_processes_returns_vec() {
    let system = make_refreshed_system();
    let processes = find_claude_processes_in(&system);
    let _ = processes.len();
}

#[test]
fn test_find_claude_processes_excludes_orphans() {
    let system = make_refreshed_system();
    let processes = find_claude_processes_in(&system);

    for cp in &processes {
        let pid = sysinfo::Pid::from_u32(cp.pid);
        if let Some(process) = system.process(pid) {
            assert!(
                !is_orphaned_process(&system, process),
                "Process pid={} should not be orphaned but was returned by find_claude_processes_in",
                cp.pid
            );
        }
    }
}

#[test]
fn test_is_orphaned_process_with_current_process() {
    let system = make_refreshed_system();

    let current_pid = sysinfo::Pid::from_u32(std::process::id());
    if let Some(process) = system.process(current_pid) {
        assert!(
            !is_orphaned_process(&system, process),
            "Current test process should not be detected as orphaned"
        );
    }
}

#[test]
fn test_is_orphaned_process_with_launchd() {
    let system = make_refreshed_system();

    let pid1 = sysinfo::Pid::from_u32(1);
    if let Some(process) = system.process(pid1) {
        let _ = is_orphaned_process(&system, process);
    }
}
