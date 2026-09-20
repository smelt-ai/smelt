//! 守护进程级 op：version / shutdown / hook 状态。

use super::super::*;
use std::io::Write;
use std::os::unix::net::UnixStream;

pub(super) fn handle_version(
    conn: UnixStream,
    sessions: &Sessions,
    exe_mtime: u64,
    daemon_fingerprint: Option<&str>,
) {
    // pid/started_at/session_count/exe/fingerprint 是后加的：旧 GUI 只读
    // version/exe_mtime，多出来的字段它直接忽略，协议向后兼容。
    // `exe`：GUI 用来判断守护是否仍住在 .app 内（装 DMG 会被 SIGKILL）。
    // `daemon_fingerprint`：启动时钉死的内容哈希，GUI 判 outdated 的主键。
    // mtime heuristic 在 StageDiskOnly 后会误判（磁盘新了进程还是老的），
    // 指纹不受磁盘替换影响。
    let session_count = sessions.len();
    let exe_path = daemon_executable_path()
        .ok()
        .map(|p| p.to_string_lossy().into_owned());
    let mut c = conn;
    let _ = writeln!(
        c,
        "{}",
        serde_json::json!({
            "version": env!("CARGO_PKG_VERSION"),
            "exe_mtime": exe_mtime,
            "exe": exe_path,
            "daemon_fingerprint": daemon_fingerprint,
            "pid": std::process::id(),
            "started_at": started_at(),
            "session_count": session_count,
            // 供其它事件客户端协商；hook helper 只发 `agent_event`。
            "agent_event_version": AGENT_EVENT_VERSION,
        })
    );
}

pub(super) fn handle_shutdown(conn: UnixStream, event_hub: &EventHubHandle) {
    let mut c = conn;
    let _peer_messaging_shutdown = match peer_messaging::begin_upgrade() {
        Ok(guard) => guard,
        Err(active_operations) => {
            let _ = writeln!(
                c,
                "{}",
                serde_json::json!({
                    "ok": false,
                    "busy": true,
                    "error": "peer messaging operations are still active",
                    "peer_messaging_operations": active_operations,
                })
            );
            return;
        }
    };
    crate::plugin_runtime::stop();
    if let Err(error) = event_hub.flush_for_shutdown() {
        crate::plugin_runtime::restart_after_failed_exec();
        let _ = writeln!(
            c,
            "{}",
            serde_json::json!({
                "ok": false,
                "error": format!("EventHub durable flush failed before shutdown: {error}")
            })
        );
        return;
    }
    let _ = writeln!(c, "{}", serde_json::json!({ "ok": true }));
    let _ = c.shutdown(Shutdown::Both);
    // 先收 iroh 隧道与远程网关，再 exit——否则手机侧的连接会继续转发到一个
    // 已死的端口。PTY 随本进程死、shell 收 SIGHUP，这是「重启守护」的代价。
    cleanup_sidecar_services();
    // 正常退出：清空身份落盘，下次启动视为干净退出。
    // 保持到 process::exit：不能让已等待的 state/remove 在删除后重新落盘。
    let _commit = STATE_DIRECTORY_COMMIT_GATE.lock().unwrap();
    with_session_directory(SessionDirectory::clear_for_clean_shutdown);
    std::process::exit(0);
}

pub(super) fn handle_agent_event_op(
    conn: UnixStream,
    v: &serde_json::Value,
    sessions: &Sessions,
    event_hub: &EventHubHandle,
) {
    let id = v["id"].as_str().unwrap_or_default();
    let event = serde_json::from_value::<AgentEvent>(v["event"].clone()).ok();
    let snapshot = event.and_then(|event| {
        sessions
            .with_live(id, |sess| {
                let mut st = sess.state.lock().unwrap();
                apply_agent_event(&mut st, &event).then(|| st.clone())
            })
            .flatten()
    });
    let accepted = snapshot.is_some();
    // ACK 表示 reducer 已提交。hook helper 收到后才退出，使 provider 的下一
    // 条同步 hook 不会越过本条；广播在 ACK 后进行，慢订阅者不阻塞事件顺序。
    let mut c = conn;
    let _ = writeln!(c, "{}", serde_json::json!({ "ok": accepted }));
    if let Some(snapshot) = snapshot {
        broadcast_state(event_hub, &snapshot);
    }
}
