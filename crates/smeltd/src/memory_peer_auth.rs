//! 记忆控制方法的进程身份：会话 token 只能由该会话的 Pi 进程本身使用，
//! 不能由它的 bash / 后台任务子孙冒用；采纳/删除不能由任何智能体会话进程调用。

use std::collections::HashSet;
use std::os::unix::net::UnixStream;

use smelt_runtime_client::protocol::control::{ControlError, ControlErrorCode};

use super::{AcpSessions, acp_runtime_alive};

fn denied(message: &str) -> ControlError {
    ControlError::new(ControlErrorCode::PermissionDenied, message)
}

/// 当前控制连接对端的 pid；取不到就拒绝（生产路径必须能取证）。
pub(crate) fn peer_pid(conn: &UnixStream) -> Result<u32, ControlError> {
    crate::protocol::system_peer_identity(conn)
        .map(|identity| identity.pid)
        .map_err(|error| denied(&format!("cannot authenticate memory control peer: {error}")))
}

/// 会话的 provider（Pi / ACP 子进程）pid。
pub(crate) fn session_provider_pid(session: &super::AcpSession) -> Option<i32> {
    if let Some(pid) = session
        .hosted_handle
        .lock()
        .unwrap()
        .as_ref()
        .and_then(|host| host.provider_pid())
    {
        return Some(pid);
    }
    session
        .handle
        .lock()
        .unwrap()
        .as_ref()
        .and_then(|handle| handle.stdio.lock().unwrap().map(|stdio| stdio.pid))
}

/// `agent.memory.meta` / `record_*`：对端必须就是持有该 token 的会话进程本身，
/// 不能是它的 bash 子孙（即使偷到了 token / 猜到了 sock 路径）。
pub(crate) fn require_session_process(
    acp_sessions: &AcpSessions,
    session_id: &str,
    peer_pid: u32,
) -> Result<(), ControlError> {
    let slot = acp_sessions
        .get(session_id)
        .ok_or_else(|| denied("session capability is invalid or expired"))?;
    let provider = acp_sessions
        .with_current(session_id, &slot, |session| {
            if !acp_runtime_alive(session) {
                return None;
            }
            session_provider_pid(session)
        })
        .flatten();
    match provider {
        Some(pid) if pid as u32 == peer_pid => Ok(()),
        _ => Err(denied(
            "memory control methods may only be called by the session process itself",
        )),
    }
}

/// `agent.memory.approve` / `delete`：对端不能是任何活跃智能体会话进程或其子孙。
pub(crate) fn deny_agent_session_peer(
    acp_sessions: &AcpSessions,
    peer_pid: u32,
) -> Result<(), ControlError> {
    let mut agent_pids = HashSet::new();
    for (id, slot) in acp_sessions.snapshot() {
        let pid = acp_sessions
            .with_current(&id, &slot, |session| {
                if acp_runtime_alive(session) {
                    session_provider_pid(session)
                } else {
                    None
                }
            })
            .flatten();
        if let Some(pid) = pid {
            agent_pids.insert(pid as u32);
        }
    }
    if agent_pids.is_empty() {
        return Ok(());
    }
    if is_same_or_descendant(peer_pid, &agent_pids) {
        return Err(denied(
            "adopting or deleting memory requires a user action outside the agent session",
        ));
    }
    Ok(())
}

fn is_same_or_descendant(peer_pid: u32, ancestors: &HashSet<u32>) -> bool {
    let mut pid = peer_pid;
    for _ in 0..64 {
        if ancestors.contains(&pid) {
            return true;
        }
        let Some(parent) = parent_pid(pid) else {
            return false;
        };
        if parent == 0 || parent == pid {
            return false;
        }
        pid = parent;
    }
    false
}

#[cfg(target_os = "linux")]
fn parent_pid(pid: u32) -> Option<u32> {
    let stat = std::fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
    let after_comm = stat.rsplit_once(')')?.1;
    after_comm
        .split_whitespace()
        .nth(1)?
        .parse()
        .ok()
}

#[cfg(target_os = "macos")]
fn parent_pid(pid: u32) -> Option<u32> {
    // PROC_PIDT_SHORTBSDINFO：只要 ppid，不碰 kinfo_proc 布局差异。
    #[repr(C)]
    struct ProcBsdShortInfo {
        pbsi_pid: u32,
        pbsi_ppid: u32,
        pbsi_pgid: u32,
        pbsi_status: u32,
        pbsi_comm: [u8; 16],
        pbsi_flags: u32,
        pbsi_uid: u32,
        pbsi_gid: u32,
        pbsi_ruid: u32,
        pbsi_rgid: u32,
        pbsi_svuid: u32,
        pbsi_svgid: u32,
        pbsi_rfu: u32,
    }
    unsafe extern "C" {
        fn proc_pidinfo(
            pid: libc::c_int,
            flavor: libc::c_int,
            arg: u64,
            buffer: *mut libc::c_void,
            buffersize: libc::c_int,
        ) -> libc::c_int;
    }
    const PROC_PIDT_SHORTBSDINFO: libc::c_int = 13;
    let mut info = ProcBsdShortInfo {
        pbsi_pid: 0,
        pbsi_ppid: 0,
        pbsi_pgid: 0,
        pbsi_status: 0,
        pbsi_comm: [0; 16],
        pbsi_flags: 0,
        pbsi_uid: 0,
        pbsi_gid: 0,
        pbsi_ruid: 0,
        pbsi_rgid: 0,
        pbsi_svuid: 0,
        pbsi_svgid: 0,
        pbsi_rfu: 0,
    };
    let size = std::mem::size_of_val(&info) as libc::c_int;
    let got = unsafe {
        proc_pidinfo(
            pid as libc::c_int,
            PROC_PIDT_SHORTBSDINFO,
            0,
            (&mut info as *mut ProcBsdShortInfo).cast(),
            size,
        )
    };
    if got < size {
        return None;
    }
    (info.pbsi_ppid > 0).then_some(info.pbsi_ppid)
}

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
fn parent_pid(_pid: u32) -> Option<u32> {
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_pid_matches_itself() {
        let mut set = HashSet::new();
        set.insert(42);
        assert!(is_same_or_descendant(42, &set));
        assert!(!is_same_or_descendant(99, &set));
    }
}
