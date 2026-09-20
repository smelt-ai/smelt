//! 交接 v2 successor（新进程）：经 socketpair 收 manifest + fd + grid，
//! 恢复会话，READY/COMMIT 两阶段接管。
//!
//! 与恢复核心的接法（有意为之，不重写）：typed manifest 经 [`build_legacy_value`]
//! 桥接成恢复核心吃的 legacy JSON 形状，再调 [`crate::resume_from_value`]。
//! 恢复核心 400 行全是实战修出来的锁/fd 语义——事务要换的是传输与提交点，
//! 不是它。bridge 的正确性由单测 + 端到端回环测试锁死；legacy 文件读端（一代
//! 兼容）走同一恢复核心，旧单测原样证明无回归。
//!
//! 收养差异经 [`ResumeOptions`] 穿给恢复核心，只有两处：终端子进程 owner
//! （waitpid→监控器）与 grid 字节侧通道（免 hex 膨胀）。ACP/menubar 对收养
//! 天然免疫（kill+ECHILD 容忍 / ECHILD 自清），零分支。

use super::child_monitor::ChildMonitor;
use super::manifest::{AcpHandoff, FdRole, HandoffManifest, TerminalChildHandoff, TerminalHandoff};
use super::transport::{self, ReadyInfo};
use std::collections::HashMap;
use std::os::fd::{AsRawFd, OwnedFd, RawFd};
use std::os::unix::net::{UnixListener, UnixStream};
use std::sync::Arc;

/// 恢复核心的 v2 选项。legacy 文件入口传 [`ResumeOptions::legacy`]，
/// 行为与旧版逐行一致。
pub(crate) struct ResumeOptions {
    adopted: Option<Arc<ChildMonitor>>,
    grids: HashMap<String, Vec<u8>>,
}

impl ResumeOptions {
    pub(crate) fn legacy() -> Self {
        Self {
            adopted: None,
            grids: HashMap::new(),
        }
    }

    pub(crate) fn adopted(monitor: Arc<ChildMonitor>, grids: HashMap<String, Vec<u8>>) -> Self {
        Self {
            adopted: Some(monitor),
            grids,
        }
    }

    pub(crate) fn adopted_monitor(&self) -> Option<&Arc<ChildMonitor>> {
        self.adopted.as_ref()
    }

    pub(crate) fn grid_blob(&self, session_id: &str) -> Option<&Vec<u8>> {
        self.grids.get(session_id)
    }
}

#[derive(Debug)]
pub enum BridgeError {
    MissingFdRole(String),
    Serialize(String),
}

impl std::fmt::Display for BridgeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            BridgeError::MissingFdRole(role) => write!(f, "bridge 缺 adopt fd：{role}"),
            BridgeError::Serialize(what) => write!(f, "bridge 序列化失败：{what}"),
        }
    }
}

/// manifest(fd 角色声明) + 收到的 fd → 恢复核心吃的 legacy JSON。
/// 调用方保证 manifest 已 validate、fd 数量与声明一致；这里再防一层，
/// 缺角色就干净 ABORT（不 panic：交接路径 panic 省不下任何东西）。
pub(crate) fn build_legacy_value(
    manifest: &HandoffManifest,
    fds: &HashMap<FdRole, OwnedFd>,
) -> Result<serde_json::Value, BridgeError> {
    let mut fd_number = |role: &FdRole| -> Result<RawFd, BridgeError> {
        fds.get(role)
            .map(|fd| fd.as_raw_fd())
            .ok_or_else(|| BridgeError::MissingFdRole(role.to_string()))
    };

    let mut sessions = Vec::with_capacity(manifest.sessions.len());
    for session in &manifest.sessions {
        let fd = fd_number(&FdRole::TerminalMaster {
            session_id: session.id.clone(),
        })?;
        let (pid, child_needs_reaper) = match &session.child {
            // Live：恢复核心走 adopt 分支建监控 owner。
            TerminalChildHandoff::Live { pid } => (*pid, true),
            // 已退出：finished(AlreadyReaped) 记录，stale pid 永不 wait/kill。
            TerminalChildHandoff::ExitedDuringHandoff { pid } => (*pid, false),
        };
        sessions.push(terminal_item(session, fd, pid, child_needs_reaper));
    }

    let mut acp_items = Vec::with_capacity(manifest.acp.len());
    for item in &manifest.acp {
        acp_items.push(acp_item(item, &mut fd_number)?);
    }

    Ok(serde_json::json!({
        "listen_fd": fd_number(&FdRole::Listen)?,
        "sessions": sessions,
        "acp_sessions": acp_items,
        "menu_gui_pids": manifest.menu_gui_pids,
        "snapshot_wall_ms": manifest.snapshot_wall_ms,
    }))
}

fn terminal_item(
    session: &TerminalHandoff,
    fd: RawFd,
    pid: i32,
    child_needs_reaper: bool,
) -> serde_json::Value {
    serde_json::json!({
        "id": session.id,
        "fd": fd,
        "pid": pid,
        "child_needs_reaper": child_needs_reaper,
        "cols": session.cols,
        "rows": session.rows,
        "cwd": session.cwd,
        "launch": session.launch,
        "agent_mcp": session.agent_mcp,
        "agent_token": session.agent_token,
        "alt_screen": session.alt_screen,
        // 无 "grid" 键：v2 grid 走 ResumeOptions 字节侧通道；缺 blob 的会话
        // 落到恢复核心的"无 grid 空 Term + jolt"，正是 BEST-EFFORT 语义。
    })
}

fn acp_item(
    item: &AcpHandoff,
    fd_number: &mut impl FnMut(&FdRole) -> Result<RawFd, BridgeError>,
) -> Result<serde_json::Value, BridgeError> {
    fn to_value<T: serde::Serialize>(
        what: &'static str,
        value: &T,
    ) -> Result<serde_json::Value, BridgeError> {
        serde_json::to_value(value).map_err(|_| BridgeError::Serialize(what.to_string()))
    }
    match item {
        AcpHandoff::Hosted {
            id,
            host_pid,
            provider_pid,
            host_snapshot_revision,
            cwd,
            launch,
            agent_mcp,
            agent_token,
            agent_needs_transcript_check,
            runtime_spec_fingerprint,
            conversation_binding,
            snapshot,
        } => {
            let host_fd = fd_number(&FdRole::AcpHost {
                session_id: id.clone(),
            })?;
            Ok(serde_json::json!({
                "runtime": "hosted",
                "id": id,
                "host_fd": host_fd,
                "host_pid": host_pid,
                "provider_pid": provider_pid,
                "host_snapshot_revision": host_snapshot_revision,
                "cwd": cwd,
                "launch": to_value("acp.launch", launch)?,
                "agent_mcp": agent_mcp,
                "agent_token": agent_token,
                "agent_needs_transcript_check": agent_needs_transcript_check,
                "runtime_spec_fingerprint": runtime_spec_fingerprint,
                "conversation_binding": to_value("acp.conversation_binding", conversation_binding)?,
                "snapshot": to_value("acp.snapshot", snapshot)?,
            }))
        }
        AcpHandoff::Direct {
            id,
            pid,
            cwd,
            launch,
            agent_mcp,
            agent_token,
            agent_needs_transcript_check,
            runtime_spec_fingerprint,
            conversation_binding,
            snapshot,
            pending_raw_line,
        } => {
            let stdin_fd = fd_number(&FdRole::AcpStdin {
                session_id: id.clone(),
            })?;
            let stdout_fd = fd_number(&FdRole::AcpStdout {
                session_id: id.clone(),
            })?;
            Ok(serde_json::json!({
                "id": id,
                "stdin_fd": stdin_fd,
                "stdout_fd": stdout_fd,
                "pid": pid,
                "cwd": cwd,
                "launch": to_value("acp.launch", launch)?,
                "agent_mcp": agent_mcp,
                "agent_token": agent_token,
                "agent_needs_transcript_check": agent_needs_transcript_check,
                "runtime_spec_fingerprint": runtime_spec_fingerprint,
                "conversation_binding": to_value("acp.conversation_binding", conversation_binding)?,
                "snapshot": to_value("acp.snapshot", snapshot)?,
                "pending_raw_line": pending_raw_line,
            }))
        }
    }
}

pub(crate) enum ImportOutcome {
    Restored {
        listener: UnixListener,
        sessions: crate::Sessions,
        acp_sessions: crate::acp_host::AcpSessions,
        ready: ReadyInfo,
    },
    Aborted {
        reason: String,
    },
}

/// `--import-handoff` 驱动：收齐 → 建监控 → bridge → 恢复 → READY → 等 COMMIT。
/// 任何一步失败都发 ABORT（能发则发）并返回 Aborted——调用方（main）直接
/// exit，predecessor 见 EOF 回滚。abort 路径下收到的 fd 随进程退出由内核回收，
/// 无需逐个清理。
pub(crate) fn run_import(
    sock: &UnixStream,
    event_hub: &crate::event_hub::EventHubHandle,
    remote_sessions: Option<crate::RemoteSessions>,
    legacy_rehome: bool,
) -> ImportOutcome {
    let abort = |sock: &UnixStream, reason: String| {
        let _ = transport::send_abort(sock, &reason);
        crate::dlog(&format!("handoff: import ABORT：{reason}"));
        ImportOutcome::Aborted { reason }
    };

    let manifest = match transport::recv_manifest(sock) {
        Ok(manifest) => manifest,
        Err(error) => return abort(sock, format!("收 manifest 失败：{error}")),
    };
    // 监控必须在恢复前建好：Live pid 在 predecessor 存活期间不可能被复用，
    // EV_ADD 看到的是真身（见 child_monitor 的窗口封闭性注释）。
    let monitor = ChildMonitor::spawn(&live_pids(&manifest));
    let fd_count = manifest.fd_roles.len();
    let received = match transport::recv_fds(sock, fd_count) {
        Ok(fds) => fds,
        Err(error) => return abort(sock, format!("收 fd 失败：{error}")),
    };
    let fds = super::manifest::assign_fds(&manifest.fd_roles, received);
    let mut grids = HashMap::new();
    let mut dropped_grids = Vec::new();
    for grid in transport::recv_grids(sock, &manifest.grids) {
        match grid.result {
            Ok(blob) => {
                grids.insert(grid.session_id, blob);
            }
            Err(error) => {
                crate::dlog(&format!(
                    "handoff: grid {} 损坏已隔离（会话保留）：{error}",
                    grid.session_id
                ));
                dropped_grids.push(grid.session_id);
            }
        }
    }
    let value = match build_legacy_value(&manifest, &fds) {
        Ok(value) => value,
        Err(error) => return abort(sock, format!("bridge 失败：{error}")),
    };
    // 所有权移交：bridge 已把裸号编进 JSON，此处 forget 掉 OwnedFd 防止
    // 析构 close；恢复核心经 from_raw_fd 逐个接管。恢复失败则 abort→进程
    // 退出，内核回收一切——abort 路径无需逐个清理。
    std::mem::forget(fds);
    let opts = ResumeOptions::adopted(monitor, grids);
    let Some((listener, sessions, acp_sessions)) =
        crate::resume_from_value(&value, &opts, event_hub, remote_sessions, legacy_rehome)
    else {
        return abort(sock, "恢复会话失败".to_string());
    };
    let ready = ReadyInfo {
        restored_terminals: sessions.len(),
        restored_acp: acp_sessions.snapshot().len(),
        dropped_grids,
    };
    if transport::send_ready(sock, &ready).is_err() {
        return abort(sock, "回 READY 失败".to_string());
    }
    match transport::await_commit(sock) {
        transport::CommitDecision::Commit | transport::CommitDecision::PredecessorGone => {
            ImportOutcome::Restored {
                listener,
                sessions,
                acp_sessions,
                ready,
            }
        }
        transport::CommitDecision::Abandon => abort(sock, "predecessor 放弃".to_string()),
        transport::CommitDecision::Timeout => abort(sock, "等 COMMIT 超时".to_string()),
    }
}

fn live_pids(manifest: &HandoffManifest) -> Vec<i32> {
    manifest
        .sessions
        .iter()
        .filter_map(|session| match session.child {
            TerminalChildHandoff::Live { pid } => Some(pid),
            TerminalChildHandoff::ExitedDuringHandoff { .. } => None,
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::super::manifest::{
        AcpHandoff, FdRole, HandoffManifest, ProducerInfo, TerminalChildHandoff, TerminalHandoff,
    };
    use super::*;
    use std::os::fd::{AsRawFd, FromRawFd};

    fn live_session(id: &str, pid: i32) -> TerminalHandoff {
        TerminalHandoff {
            id: id.to_string(),
            child: TerminalChildHandoff::Live { pid },
            cols: 100,
            rows: 40,
            cwd: Some("/x".to_string()),
            launch: Some("fish".to_string()),
            agent_mcp: true,
            agent_token: "tok".to_string(),
            alt_screen: true,
        }
    }

    /// 造一批"假 adopt fd"：pipe 读端，号有效、语义无意义（bridge 不碰内核）。
    fn fake_fds(roles: &[FdRole]) -> HashMap<FdRole, OwnedFd> {
        let mut map = HashMap::new();
        for role in roles {
            let mut pair = [0; 2];
            assert_eq!(unsafe { libc::pipe(pair.as_mut_ptr()) }, 0);
            unsafe { libc::close(pair[1]) };
            map.insert(role.clone(), unsafe { OwnedFd::from_raw_fd(pair[0]) });
        }
        map
    }

    #[test]
    fn bridge_emits_legacy_terminal_shape() {
        let manifest = HandoffManifest {
            producer: ProducerInfo {
                version: "0.9.0".to_string(),
                pid: 9,
            },
            snapshot_wall_ms: 0,
            fd_roles: vec![
                FdRole::Listen,
                FdRole::TerminalMaster {
                    session_id: "t1".to_string(),
                },
                FdRole::TerminalMaster {
                    session_id: "t-dead".to_string(),
                },
            ],
            sessions: vec![
                live_session("t1", 111),
                TerminalHandoff {
                    id: "t-dead".to_string(),
                    child: TerminalChildHandoff::ExitedDuringHandoff { pid: 112 },
                    ..live_session("ignored", 0)
                },
            ],
            acp: Vec::new(),
            menu_gui_pids: vec![113],
            grids: Vec::new(),
        };
        let fds = fake_fds(&manifest.fd_roles);
        let master_fd = fds[&FdRole::TerminalMaster {
            session_id: "t1".to_string(),
        }]
            .as_raw_fd();
        let value = build_legacy_value(&manifest, &fds).unwrap();

        assert_eq!(
            value["listen_fd"].as_i64().unwrap() as RawFd,
            fds[&FdRole::Listen].as_raw_fd()
        );
        let items = value["sessions"].as_array().unwrap();
        assert_eq!(items.len(), 2);
        // Live：reaper=true，走 adopt 分支。
        assert_eq!(items[0]["id"], "t1");
        assert_eq!(items[0]["fd"].as_i64().unwrap() as RawFd, master_fd);
        assert_eq!(items[0]["pid"], 111);
        assert_eq!(items[0]["child_needs_reaper"], true);
        assert_eq!(items[0]["cols"], 100);
        assert_eq!(items[0]["alt_screen"], true);
        assert!(items[0].get("grid").is_none(), "v2 不走 hex grid 键");
        // 已退出：reaper=false，finished 记录。
        assert_eq!(items[1]["id"], "t-dead");
        assert_eq!(items[1]["pid"], 112);
        assert_eq!(items[1]["child_needs_reaper"], false);
        assert_eq!(value["menu_gui_pids"][0], 113);
    }

    fn snapshot_fixture() -> smelt_core::acp_session::ConversationSnapshot {
        // 最小合法快照（与 manifest 测试同一形状；bridge 只搬运不解读）。
        serde_json::from_value(serde_json::json!({
            "entries": [],
            "phase": "Idle",
            "pending_elicitation": null,
            "status_line": null,
            "acp_session_id": "acp-sess-1",
            "supports_image": false,
            "available_commands": [],
            "usage": null,
            "plan": null,
            "model": null,
            "config_options": [],
            "completed_unread": false,
            "should_persist": false,
        }))
        .expect("最小快照应合法")
    }

    #[test]
    fn bridge_emits_legacy_acp_shapes() {
        let launch = smelt_core::agent_kind::ConversationLaunchSpec::from_command("agent --x");
        let manifest = HandoffManifest {
            producer: ProducerInfo {
                version: "0.9.0".to_string(),
                pid: 9,
            },
            snapshot_wall_ms: 0,
            fd_roles: vec![
                FdRole::Listen,
                FdRole::AcpHost {
                    session_id: "a1".to_string(),
                },
                FdRole::AcpStdin {
                    session_id: "a2".to_string(),
                },
                FdRole::AcpStdout {
                    session_id: "a2".to_string(),
                },
            ],
            sessions: Vec::new(),
            acp: vec![
                AcpHandoff::Hosted {
                    id: "a1".to_string(),
                    host_pid: 201,
                    provider_pid: Some(202),
                    host_snapshot_revision: 7,
                    cwd: Some("/r".to_string()),
                    launch: launch.clone(),
                    agent_mcp: true,
                    agent_token: "tok".to_string(),
                    agent_needs_transcript_check: false,
                    runtime_spec_fingerprint: None,
                    conversation_binding: None,
                    snapshot: snapshot_fixture(),
                },
                AcpHandoff::Direct {
                    id: "a2".to_string(),
                    pid: 301,
                    cwd: None,
                    launch,
                    agent_mcp: false,
                    agent_token: String::new(),
                    agent_needs_transcript_check: true,
                    runtime_spec_fingerprint: Some("fp".to_string()),
                    conversation_binding: None,
                    snapshot: snapshot_fixture(),
                    pending_raw_line: Some("raw".to_string()),
                },
            ],
            menu_gui_pids: Vec::new(),
            grids: Vec::new(),
        };
        let fds = fake_fds(&manifest.fd_roles);
        let value = build_legacy_value(&manifest, &fds).unwrap();
        let items = value["acp_sessions"].as_array().unwrap();
        assert_eq!(items.len(), 2);
        // hosted：恢复侧按 runtime 分流，字段名必须逐字匹配。
        assert_eq!(items[0]["runtime"], "hosted");
        assert_eq!(items[0]["id"], "a1");
        assert_eq!(items[0]["host_pid"], 201);
        assert_eq!(items[0]["provider_pid"], 202);
        assert_eq!(items[0]["host_snapshot_revision"], 7);
        assert_eq!(items[0]["launch"]["command"], "agent --x");
        assert_eq!(
            items[0]["host_fd"].as_i64().unwrap(),
            fds[&FdRole::AcpHost {
                session_id: "a1".to_string()
            }]
                .as_raw_fd() as i64
        );
        // direct：stdin/stdout/pid 三件套 + 快照内 acp_session_id。
        assert!(items[1].get("runtime").is_none());
        assert_eq!(items[1]["id"], "a2");
        assert_eq!(items[1]["pid"], 301);
        assert_eq!(items[1]["snapshot"]["acp_session_id"], "acp-sess-1");
        assert_eq!(items[1]["pending_raw_line"], "raw");
    }

    #[test]
    fn bridge_reports_missing_role_without_panic() {
        let manifest = HandoffManifest {
            producer: ProducerInfo {
                version: "0.9.0".to_string(),
                pid: 9,
            },
            snapshot_wall_ms: 0,
            fd_roles: vec![FdRole::Listen],
            sessions: vec![live_session("t1", 111)],
            acp: Vec::new(),
            menu_gui_pids: Vec::new(),
            grids: Vec::new(),
        };
        let fds = fake_fds(&manifest.fd_roles);
        assert!(matches!(
            build_legacy_value(&manifest, &fds),
            Err(BridgeError::MissingFdRole(_))
        ));
    }
}
