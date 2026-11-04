//! `resource.usage`：这台运行时此刻的资源占用。
//!
//! 守护进程只算它自己：会话都是它的子孙，算整棵树就把会话重复算一遍。每个会话从根进程往下
//! 算整棵树。终端的根是 PTY 里的 shell；对话的根是 agent 子进程（Pi、Claude、
//! Codex……）和它们起的工具进程都在下面，真正吃资源的多半是这些子孙。
//!
//! 采样器整个守护共用一份。CPU 是跟上一次调用比出来的：几个客户端同时在看时，间隔就是
//! 它们交错调用的间隔，得到的仍是那一小段时间的平均占用。

use std::sync::{LazyLock, Mutex};

use smelt_core::control_api::{
    LocalModelUsage, ProcessEntry, ProcessUsage, ResourceUsageResult, SESSION_PROCESS_LIMIT, SessionResourceUsage,
};
use smelt_sys::process_usage::{ProcessSampler, Usage};

use super::acp_host::AcpSession;
use super::{AcpSessions, Sessions};

/// 命令行最多带这么多个字符。长命令（一长串路径、参数）截掉尾巴，界面上本来也放不下。
const COMMAND_LIMIT: usize = 300;

static SAMPLER: LazyLock<Mutex<ProcessSampler>> =
    LazyLock::new(|| Mutex::new(ProcessSampler::new()));

pub(crate) fn resource_usage(
    sessions: &Sessions,
    acp_sessions: &AcpSessions,
) -> ResourceUsageResult {
    // 先取完根进程再锁采样器：取根进程要碰会话自己的锁，不和扫进程表叠在一起。
    let roots = session_roots(sessions, acp_sessions);
    let model_pid = super::local_model_process::current();
    let mut sampler = SAMPLER.lock().unwrap_or_else(|error| error.into_inner());
    let total_memory_bytes = sampler.total_memory();
    let table = sampler.refresh_all();
    let daemon = table
        .process(std::process::id())
        .map(wire)
        .unwrap_or_default();
    // 模型进程按整棵树算。采样期间它可能起停，所以采不到时再看一次登记：
    // 前后都没登记才是没在跑；还登记着（或刚起来）就是在跑但没采到，不编数字。
    let local_model = Some(match model_pid.and_then(|pid| table.tree(pid)) {
        Some(usage) => LocalModelUsage::Running { usage: wire(usage) },
        None if super::local_model_process::current().is_some() => LocalModelUsage::Unsampled,
        None => LocalModelUsage::NotRunning,
    });
    let sessions = roots
        .into_iter()
        .filter_map(|(session_id, pid)| {
            let processes = table.tree_processes(pid)?;
            let mut usage = Usage::default();
            for process in &processes {
                usage.add(process.usage);
            }
            Some(SessionResourceUsage {
                session_id,
                usage: wire(usage),
                processes: processes
                    .into_iter()
                    .take(SESSION_PROCESS_LIMIT)
                    .map(|process| ProcessEntry {
                        pid: process.pid,
                        name: process.name,
                        command: process.command.chars().take(COMMAND_LIMIT).collect(),
                        usage: wire(process.usage),
                    })
                    .collect(),
            })
        })
        .collect();
    ResourceUsageResult {
        daemon,
        sessions,
        total_memory_bytes,
        local_model,
        // 内存里的累计数，不做 IO。
        local_model_inference: Some(super::local_model_process::inference_stats()),
    }
}

/// 活会话的根进程，按会话 id 排序。根进程已经退出的会话不列。
fn session_roots(sessions: &Sessions, acp_sessions: &AcpSessions) -> Vec<(String, u32)> {
    let mut roots = Vec::new();
    for (id, session) in sessions.snapshot() {
        if let Some(pid) = session.child.running_pid() {
            roots.push((id, pid));
        }
    }
    for (id, slot) in acp_sessions.snapshot() {
        if let Some(pid) = acp_root_pid(&slot.value) {
            roots.push((id, pid));
        }
    }
    let mut roots: Vec<(String, u32)> = roots
        .into_iter()
        .filter_map(|(id, pid)| {
            let pid = u32::try_from(pid).ok().filter(|pid| *pid > 1)?;
            Some((id, pid))
        })
        .collect();
    roots.sort();
    roots
}

/// 对话的根进程是 agent 子进程。句柄还在，pid 就还是它的。
fn acp_root_pid(session: &AcpSession) -> Option<i32> {
    let handle = session.handle.lock();
    let stdio = *handle
        .as_ref()?
        .stdio
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    stdio.map(|stdio| stdio.pid)
}

fn wire(usage: Usage) -> ProcessUsage {
    ProcessUsage {
        cpu_percent: usage.cpu_percent,
        memory_bytes: usage.memory_bytes,
        process_count: usage.processes,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn with_no_sessions_only_the_daemon_itself_is_reported() {
        let sessions = super::super::new_sessions();
        let acp_sessions = super::super::new_test_acp_sessions();
        let result = resource_usage(&sessions, &acp_sessions);
        assert!(result.sessions.is_empty());
        assert_eq!(result.daemon.process_count, 1);
        assert!(result.daemon.memory_bytes > 0);
        assert!(result.total_memory_bytes > 0);
        // 新运行时总是带上推理统计（可能还是空的），好和不报它的旧运行时区分开。
        assert!(result.local_model_inference.is_some());
    }

    #[test]
    fn the_local_model_process_is_reported_only_while_registered() {
        let _guard = crate::local_model_process::TEST_LOCK
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let sessions = super::super::new_sessions();
        let acp_sessions = super::super::new_test_acp_sessions();
        assert_eq!(
            resource_usage(&sessions, &acp_sessions).local_model,
            Some(LocalModelUsage::NotRunning)
        );

        let mut child = std::process::Command::new("sleep")
            .arg("30")
            .spawn()
            .expect("sleep");
        crate::local_model_process::set_running(child.id());
        let result = resource_usage(&sessions, &acp_sessions);
        crate::local_model_process::clear(child.id());
        let _ = child.kill();
        let _ = child.wait();
        match result.local_model {
            Some(LocalModelUsage::Running { usage }) => {
                assert_eq!(usage.process_count, 1);
                assert!(usage.memory_bytes > 0);
            }
            other => panic!("应报模型进程：{other:?}"),
        }
        // 登记着但进程表里采不到：报在跑但没采到，不当成未运行。
        crate::local_model_process::set_running(child.id());
        assert_eq!(
            resource_usage(&sessions, &acp_sessions).local_model,
            Some(LocalModelUsage::Unsampled)
        );
        crate::local_model_process::clear(child.id());
        // 守护自己那一行不含模型进程。
        assert_eq!(result.daemon.process_count, 1);
        assert_eq!(
            resource_usage(&sessions, &acp_sessions).local_model,
            Some(LocalModelUsage::NotRunning)
        );
    }
}
