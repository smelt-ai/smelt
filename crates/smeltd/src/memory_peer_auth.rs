//! 记忆控制方法的进程身份：会话 token 只能由该会话的 Pi 进程本身使用，
//! 不能由它的 bash / 后台任务子孙冒用；采纳/删除不能由任何智能体会话进程调用。

use std::collections::HashSet;

use smelt_runtime_client::protocol::control::{ControlError, ControlErrorCode};

use super::{AcpSessions, acp_runtime_alive};

fn denied(message: &str) -> ControlError {
    ControlError::new(ControlErrorCode::PermissionDenied, message)
}

/// 当前控制连接对端的 pid。AF_UNIX 与 Windows 命名管道都走 [`crate::protocol::PeerSource`]。
/// 取不到就拒绝（生产路径必须能取证）。
pub(crate) fn peer_pid(conn: &impl crate::protocol::PeerSource) -> Result<u32, ControlError> {
    conn.peer_identity()
        .map(|identity| identity.pid)
        .map_err(|error| denied(&format!("cannot authenticate memory control peer: {error}")))
}

/// 一条会话里和记忆鉴权有关的进程：独立宿主、它拉起的 pi-agent，以及同进程模式的 stdio。
#[derive(Clone, Debug, Default, PartialEq, Eq)]
struct SessionRoots {
    host_pid: Option<u32>,
    provider_pid: Option<u32>,
    handle_pid: Option<u32>,
}

fn session_roots(session: &super::AcpSession) -> SessionRoots {
    let hosted = session.hosted_handle.lock().unwrap();
    let host_pid = hosted
        .as_ref()
        .map(|host| host.pid())
        .filter(|pid| *pid > 1)
        .and_then(|pid| u32::try_from(pid).ok());
    let provider_pid = hosted
        .as_ref()
        .and_then(|host| host.provider_pid())
        .filter(|pid| *pid > 1)
        .and_then(|pid| u32::try_from(pid).ok());
    drop(hosted);
    let handle_pid = session
        .handle
        .lock()
        .unwrap()
        .as_ref()
        .and_then(|handle| handle.stdio.lock().unwrap().as_ref().map(|stdio| stdio.pid))
        .filter(|pid| *pid > 1)
        .and_then(|pid| u32::try_from(pid).ok());
    SessionRoots {
        host_pid,
        provider_pid,
        handle_pid,
    }
}

const SHELL_NAMES: &[&str] = &[
    "ash",
    "bash",
    "csh",
    "dash",
    "fish",
    "ksh",
    "pwsh",
    "powershell",
    "sh",
    "tcsh",
    "zsh",
];

fn is_shell_name(name: &str) -> bool {
    let base = name.rsplit(['/', '\\']).next().unwrap_or(name);
    let base = strip_one_exe(base);
    SHELL_NAMES
        .iter()
        .any(|shell| base.eq_ignore_ascii_case(shell))
}

fn strip_one_exe(name: &str) -> &str {
    let bytes = name.as_bytes();
    if bytes.len() > 4 && bytes[bytes.len() - 4..].eq_ignore_ascii_case(b".exe") {
        &name[..name.len() - 4]
    } else {
        name
    }
}

/// `peer` 能不能代表这条会话调 `agent.memory.*`。
///
/// 只认 pi-agent 自己：hosted 模式的 `provider_pid`，或同进程模式 `session.handle` 的
/// stdio pid。宿主的直接子进程仅在 provider 还没报上来、且能确认不是 shell 时放行。
/// pi-agent 再往下的 bash / sh / zsh 一律拒绝。
fn allows_session_peer(
    peer: u32,
    roots: &SessionRoots,
    parent_of: impl Fn(u32) -> Option<u32>,
    comm_of: impl Fn(u32) -> Option<String>,
) -> bool {
    if comm_of(peer).is_some_and(|name| is_shell_name(&name)) {
        return false;
    }
    if roots.provider_pid == Some(peer) || roots.handle_pid == Some(peer) {
        return true;
    }
    let Some(host) = roots.host_pid else {
        return false;
    };
    if parent_of(peer) != Some(host) {
        return false;
    }
    match roots.provider_pid {
        Some(provider) => peer == provider,
        // 还不知道 pi-agent 的 pid：直接子进程必须能读到进程名，并且不是 shell。
        None => comm_of(peer).is_some_and(|name| !is_shell_name(&name)),
    }
}

/// `agent.memory.meta` / `record_*`：对端必须是持有该 token 的 pi-agent 本身。
pub(crate) fn require_session_process(
    acp_sessions: &AcpSessions,
    session_id: &str,
    peer_pid: u32,
) -> Result<(), ControlError> {
    let slot = acp_sessions
        .get(session_id)
        .ok_or_else(|| denied("session capability is invalid or expired"))?;
    let roots = acp_sessions.with_current(session_id, &slot, |session| {
        if !acp_runtime_alive(session) {
            return None;
        }
        Some(session_roots(session))
    });
    match roots {
        Some(Some(roots)) if allows_session_peer(peer_pid, &roots, parent_pid, process_comm) => {
            Ok(())
        }
        _ => Err(denied(
            "memory control methods may only be called by the session process itself",
        )),
    }
}

fn agent_ancestor_pids(acp_sessions: &AcpSessions) -> HashSet<u32> {
    let mut agent_pids = HashSet::new();
    for (id, slot) in acp_sessions.snapshot() {
        let Some(Some(roots)) = acp_sessions.with_current(&id, &slot, |session| {
            acp_runtime_alive(session).then(|| session_roots(session))
        }) else {
            continue;
        };
        for pid in [roots.host_pid, roots.provider_pid, roots.handle_pid]
            .into_iter()
            .flatten()
        {
            agent_pids.insert(pid);
        }
    }
    agent_pids
}

/// `agent.memory.approve` / `delete`：对端不能是任何活跃智能体会话进程或其子孙。
pub(crate) fn deny_agent_session_peer(
    acp_sessions: &AcpSessions,
    peer_pid: u32,
) -> Result<(), ControlError> {
    let agent_pids = agent_ancestor_pids(acp_sessions);
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

/// 采纳/删除的调用方。本机必须是签过名的桌面进程，再叠加「不是会话进程树」。
/// 已配对的远程设备走 iroh，不在这台机器的进程树上。
///
/// `allow_missing_peer` 只给没有 unix 对端的单元测试用。生产路径必须是 false。
pub(crate) fn authorize_memory_mutation(
    acp_sessions: &AcpSessions,
    principal: &crate::devices::Principal,
    peer_pid: Option<u32>,
    desktop_authenticated: bool,
    allow_missing_peer: bool,
) -> Result<(), ControlError> {
    use crate::devices::Principal;
    match principal {
        Principal::Device(_) => Ok(()),
        Principal::Unpaired(_) => Err(denied("unpaired devices cannot adopt or delete memory")),
        Principal::Local => {
            if allow_missing_peer && peer_pid.is_none() {
                return Ok(());
            }
            if !desktop_authenticated {
                return Err(denied(
                    "adopting or deleting memory requires the signed desktop app",
                ));
            }
            let Some(pid) = peer_pid else {
                return Err(denied(
                    "cannot authenticate caller for memory approve/delete",
                ));
            };
            deny_agent_session_peer(acp_sessions, pid)
        }
    }
}

fn is_same_or_descendant(peer_pid: u32, ancestors: &HashSet<u32>) -> bool {
    is_same_or_descendant_with(peer_pid, ancestors, parent_pid)
}

fn is_same_or_descendant_with(
    peer_pid: u32,
    ancestors: &HashSet<u32>,
    parent_of: impl Fn(u32) -> Option<u32>,
) -> bool {
    let mut pid = peer_pid;
    for _ in 0..64 {
        if ancestors.contains(&pid) {
            return true;
        }
        let Some(parent) = parent_of(pid) else {
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
fn process_comm(pid: u32) -> Option<String> {
    let raw = std::fs::read_to_string(format!("/proc/{pid}/comm")).ok()?;
    let name = raw.trim().trim_end_matches('\0');
    (!name.is_empty()).then(|| name.to_string())
}

#[cfg(target_os = "linux")]
fn parent_pid(pid: u32) -> Option<u32> {
    let stat = std::fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
    let after_comm = stat.rsplit_once(')')?.1;
    after_comm.split_whitespace().nth(1)?.parse().ok()
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

#[cfg(target_os = "macos")]
fn process_comm(pid: u32) -> Option<String> {
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
    let end = info
        .pbsi_comm
        .iter()
        .position(|byte| *byte == 0)
        .unwrap_or(info.pbsi_comm.len());
    let name = std::str::from_utf8(&info.pbsi_comm[..end]).ok()?.trim();
    (!name.is_empty()).then(|| name.to_string())
}

/// Windows 没有可用的父进程查询。父链走不到就拒绝「宿主直接子进程」这条宽放。
/// 子孙黑名单因此只能认出 pid 本身；采纳和删除仍由桌面白名单失败关闭。
/// 精确的 provider / handle pid 仍然有效。
#[cfg(not(any(target_os = "linux", target_os = "macos")))]
fn parent_pid(_pid: u32) -> Option<u32> {
    None
}

/// Windows 用 #83 的映像路径判断是不是 shell。读不到就当未知，宽放路径因此失败关闭。
#[cfg(windows)]
fn process_comm(pid: u32) -> Option<String> {
    let path = smelt_sys::process::process_executable(pid).ok()?;
    path.file_name()
        .and_then(|name| name.to_str())
        .map(str::to_string)
}

#[cfg(not(any(target_os = "linux", target_os = "macos", windows)))]
fn process_comm(_pid: u32) -> Option<String> {
    None
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;
    use std::sync::{Arc, Mutex};

    use super::*;

    fn parents(map: &HashMap<u32, u32>) -> impl Fn(u32) -> Option<u32> + '_ {
        move |pid| map.get(&pid).copied()
    }

    fn comms<'map>(map: &'map HashMap<u32, &'static str>) -> impl Fn(u32) -> Option<String> + 'map {
        move |pid| map.get(&pid).map(|name| (*name).to_string())
    }

    struct FixedPeer {
        pid: u32,
        fail: bool,
    }

    impl crate::protocol::PeerSource for FixedPeer {
        fn peer_identity(&self) -> Result<crate::protocol::PeerIdentity, String> {
            if self.fail {
                return Err("no peer".into());
            }
            Ok(crate::protocol::PeerIdentity {
                pid: self.pid,
                executable: None,
                #[cfg(target_os = "macos")]
                audit_token: None,
            })
        }
    }

    #[test]
    fn peer_pid_reads_the_peer_source_identity() {
        assert_eq!(
            peer_pid(&FixedPeer {
                pid: 42,
                fail: false
            })
            .unwrap(),
            42
        );
        assert!(peer_pid(&FixedPeer { pid: 1, fail: true }).is_err());
    }

    #[test]
    fn a_pid_matches_itself_but_a_reparented_orphan_does_not() {
        let mut set = HashSet::new();
        set.insert(42);
        assert!(is_same_or_descendant_with(42, &set, |_| None));
        assert!(!is_same_or_descendant_with(99, &set, |_| None));
        // double-fork 之后 PPID=1，沿父链走不到 pi-agent。黑名单因此放行，必须靠桌面白名单。
        let mut tree = HashMap::new();
        tree.insert(7, 1);
        tree.insert(1, 0);
        assert!(!is_same_or_descendant_with(7, &set, parents(&tree)));
    }

    #[test]
    fn only_the_pi_agent_itself_may_call_session_memory_methods() {
        let roots = SessionRoots {
            host_pid: Some(10),
            provider_pid: Some(20),
            handle_pid: Some(30),
        };
        let mut tree = HashMap::new();
        tree.insert(20, 10); // pi-agent 是 host 的直接子进程
        tree.insert(21, 20); // bash 是 pi-agent 的子进程
        tree.insert(22, 21); // 孙进程
        tree.insert(23, 10); // host 的另一个直接子进程
        let mut names = HashMap::new();
        names.insert(20, "node");
        names.insert(21, "bash");
        names.insert(22, "python");
        names.insert(23, "zsh");
        names.insert(25, "powershell.exe");
        names.insert(30, "pi-rpc");
        let allow = |pid| allows_session_peer(pid, &roots, parents(&tree), comms(&names));
        assert!(allow(20), "provider 自己可以");
        assert!(allow(30), "同进程模式的 handle pid 可以");
        assert!(!allow(25), "Windows 的 powershell.exe 也不行");
        assert!(!allow(21), "pi-agent 的 bash 子进程不行");
        assert!(!allow(22), "再往下的孙进程不行");
        assert!(!allow(23), "宿主的 shell 子进程不行");
        assert!(!allow(10), "session-host 自己不是 pi-agent");

        let unbound = SessionRoots {
            host_pid: Some(10),
            provider_pid: None,
            handle_pid: None,
        };
        let mut names = HashMap::new();
        names.insert(24, "bun");
        names.insert(23, "zsh");
        tree.insert(24, 10);
        let allow = |pid| allows_session_peer(pid, &unbound, parents(&tree), comms(&names));
        assert!(
            allow(24),
            "provider 还没绑定时，宿主的非 shell 直接子进程可以"
        );
        assert!(!allow(23), "这时 shell 仍然不行");
        assert!(!allow(21), "pi-agent 的子进程仍然不行");
    }

    fn session_with_handle_pid(registry: &AcpSessions, id: &str, pid: i32) {
        let (slot, _) = registry.reserve_with(id, || {
            super::super::make_acp_session(
                id,
                None,
                Some(smelt_core::conversation::ConversationBinding::Direct),
                None,
            )
        });
        {
            let mut state = slot.value.state.lock().unwrap();
            state.phase = super::super::Phase::Idle;
        }
        let (cmd_tx, _cmd_rx) = smol::channel::unbounded();
        let (_event_tx, event_rx) = smol::channel::unbounded();
        *slot.value.handle.lock().unwrap() = Some(smelt_core::acp_conn::ConversationHandle {
            cmd_tx,
            event_rx,
            stdio: Arc::new(Mutex::new(Some(smelt_core::acp_conn::AcpStdio {
                pid,
                #[cfg(unix)]
                stdin_fd: -1,
                #[cfg(unix)]
                stdout_fd: -1,
            }))),
            in_flight_rpc: Arc::new(std::sync::atomic::AtomicUsize::new(0)),
            shutdown_requested: Arc::new(std::sync::atomic::AtomicBool::new(false)),
            supports_mid_turn_input: false,
            supports_compaction: false,
            supports_native_queue: false,
            supports_rewind: false,
        });
    }

    #[test]
    fn same_process_handle_pid_is_a_session_root() {
        let sessions = super::super::new_test_acp_sessions();
        // 用不存在的 pid，避免碰巧撞上本机的 shell 而被进程名拒绝。
        let handle_pid = 2_147_000_242;
        session_with_handle_pid(&sessions, "acp-1", handle_pid);
        assert!(require_session_process(&sessions, "acp-1", handle_pid as u32).is_ok());
        assert!(require_session_process(&sessions, "acp-1", handle_pid as u32 + 1).is_err());
        assert!(deny_agent_session_peer(&sessions, handle_pid as u32).is_err());
        assert!(deny_agent_session_peer(&sessions, handle_pid as u32 + 1).is_ok());
    }

    #[test]
    fn local_approve_requires_the_desktop_and_paired_devices_are_allowed() {
        let sessions = super::super::new_test_acp_sessions();
        session_with_handle_pid(&sessions, "acp-1", 2_147_000_242);
        let local = crate::devices::Principal::Local;
        let paired = crate::devices::Principal::Device("phone".into());
        let unpaired = crate::devices::Principal::Unpaired("stranger".into());

        assert!(
            authorize_memory_mutation(&sessions, &local, None, false, true).is_ok(),
            "单测没有对端时放行"
        );
        let missing_desktop =
            authorize_memory_mutation(&sessions, &local, Some(7), false, false).unwrap_err();
        assert!(missing_desktop.message.contains("signed desktop"));
        let agent = authorize_memory_mutation(&sessions, &local, Some(2_147_000_242), true, false)
            .unwrap_err();
        assert!(agent.message.contains("outside the agent session"));
        assert!(authorize_memory_mutation(&sessions, &local, Some(7), true, false).is_ok());
        assert!(authorize_memory_mutation(&sessions, &paired, None, false, false).is_ok());
        assert!(authorize_memory_mutation(&sessions, &unpaired, None, true, false).is_err());
    }
}
