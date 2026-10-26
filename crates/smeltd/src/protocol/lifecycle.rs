//! 守护进程级 op：version / shutdown / hook 状态。

use super::super::*;

pub(super) fn handle_version<C: ClientSocket>(
    conn: C,
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
    let _ = smelt_runtime_client::protocol::write_json_line(
        &mut c,
        &serde_json::json!({
            "version": env!("CARGO_PKG_VERSION"),
            "exe_mtime": exe_mtime,
            "exe": exe_path,
            "daemon_fingerprint": daemon_fingerprint,
            "pid": std::process::id(),
            "started_at": started_at(),
            "session_count": session_count,
            // 供其它事件客户端协商；hook helper 只发 `agent_event`。
            "agent_event_version": AGENT_EVENT_VERSION,
        }),
    );
}

pub(super) fn handle_shutdown<C: ClientSocket>(conn: C) {
    graceful_shutdown(Some(conn.into_client_conn()), None);
}

/// 停机只做一次。信号和 shutdown op 撞在一起时，后来的调用停在这里，
/// 等先到的那次把进程换掉或退出。
static SHUTDOWN_STARTED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// 停机是否已经开始。一旦为真，这个进程只会 exec 或退出，不会再回到正常服务。
/// 新会话请求据此立刻回「守护正在停机」，不去 SPAWN_GATE 上排队。
pub(crate) fn shutdown_started() -> bool {
    SHUTDOWN_STARTED.load(std::sync::atomic::Ordering::Acquire)
}

/// 五步停机。`exec_exe` 有值时，子进程和文件句柄都放开之后再 `exec` 到那份新映像。
/// 没有则在回执写完后退出。回执一定在收割和 WAL checkpoint 之后，调用方才能去换
/// `.app` / `.exe`。
pub(crate) fn graceful_shutdown(
    ack: Option<crate::ClientConn>,
    exec_exe: Option<std::path::PathBuf>,
) -> ! {
    use std::sync::atomic::Ordering;

    if SHUTDOWN_STARTED.swap(true, Ordering::AcqRel) {
        // 先到的那次会 exec 或退出，这条线程不会再被用到。没有人 unpark 它；
        // park 可能假醒，所以套在循环里。
        loop {
            std::thread::park();
        }
    }

    // 1. 禁新会话、禁新子进程，并挡住身份目录的迟到提交。上面的标记先于写锁置上：
    // 新的终端请求看到标记或拿不到读锁，都立刻回「守护正在停机」（见 `spawn_permit`）。
    let _spawn_gate = SPAWN_GATE
        .write()
        .unwrap_or_else(|error| error.into_inner());
    let _commit_gate = STATE_DIRECTORY_COMMIT_GATE
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    dlog("shutdown: 已拦住新会话和新的状态提交");

    // 2. 把没写完的回合收成中断，并留下进行中的自动化对话。只有那几段要存全文，
    // 才向宿主要回裁掉的前缀；普通会话的全文在 agent 自己的会话文件里，停机后用不到，
    // 不为它们走一趟宿主。这一步因此不随会话数变长，调用方的读超时盖得住。
    if let Some(server) = super::SERVER.get() {
        let automation_sessions = active_automation_sessions(&server.automations);
        for (id, slot) in server.acp_sessions.snapshot() {
            if automation_sessions.contains(&id) {
                dlog(&format!("shutdown: 拉回自动化会话 {id} 的全文"));
                crate::acp_host::reassemble_hosted_transcript(&slot.value);
            }
            crate::acp_host::settle_acp_session_for_shutdown(&slot.value);
        }
        save_active_automation_transcripts(&server.automations, &server.acp_sessions);
    }

    // 3. 并发收割 ACP 进程组和 PTY。回执之前必须证明它们已经退出。
    if let Some(server) = super::SERVER.get() {
        let acp = server.acp_sessions.snapshot();
        let sessions = server.sessions.snapshot();
        let mut joins = Vec::with_capacity(acp.len() + sessions.len());
        for (id, slot) in acp {
            joins.push(std::thread::spawn(move || {
                if !crate::acp_host::retire_acp_runtime(&slot.value) {
                    dlog(&format!("shutdown: 会话 {id} 的进程组没有收干净"));
                }
            }));
        }
        for (id, session) in sessions {
            joins.push(std::thread::spawn(move || {
                if !session.child.terminate_and_wait(ACP_SHUTDOWN_GRACE) {
                    dlog(&format!("shutdown: 终端 {id} 没有在宽限内退出"));
                }
            }));
        }
        for join in joins {
            let _ = join.join();
        }
    }

    // 4. sidecar、身份目录、SQLite WAL。目录清空握着上面那把提交锁。
    crate::remote::cleanup_sidecar_services();
    with_session_directory(crate::session_directory::SessionDirectory::clear_for_clean_shutdown);
    match smelt_core::sqlite_state::default_sqlite_store() {
        Ok(store) => {
            if let Err(error) = store.checkpoint_wal_truncate() {
                dlog(&format!("shutdown: WAL checkpoint 失败：{error}"));
            }
        }
        Err(error) => dlog(&format!("shutdown: 打开状态库失败：{error}")),
    }

    // 5. 先摘掉监听路径，再回执。调用方拿到 ok 就会立刻 bind 新守护；
    // 旧进程这时还没退出，路径还在的话新进程会连上这个即将消失的监听。
    let _ = std::fs::remove_file(crate::sock_path());
    if let Some(mut conn) = ack {
        let _ = smelt_runtime_client::protocol::write_json_line(
            &mut conn,
            &serde_json::json!({ "ok": true }),
        );
        let _ = conn.flush();
        let _ = ClientSocket::shutdown(&conn, std::net::Shutdown::Both);
    }
    if let Some(exe) = exec_exe {
        #[cfg(unix)]
        {
            use std::os::unix::process::CommandExt;
            dlog(&format!("shutdown: exec {}", exe.display()));
            let mut command = std::process::Command::new(&exe);
            command
                .env_remove("SMELTD_HANDOFF")
                .env_remove("SMELTD_HANDOFF_SOCK")
                .env_remove("SMELTD_DAEMON_FINGERPRINT");
            let error = command.exec();
            dlog(&format!("shutdown: exec {} 失败：{error}", exe.display()));
            std::process::exit(1);
        }
        // 没有 exec：拉起接班进程，它等本进程退出、放掉监听再绑定。
        #[cfg(windows)]
        {
            dlog(&format!("shutdown: 拉起接班守护 {}", exe.display()));
            let mut successor = std::process::Command::new(&exe);
            successor.env(crate::PREDECESSOR_PID_ENV, std::process::id().to_string());
            if let Err(error) = smelt_sys::process::spawn_detached(&mut successor) {
                dlog(&format!("shutdown: 拉起接班守护 {} 失败：{error}", exe.display()));
                std::process::exit(1);
            }
        }
    }
    std::process::exit(0);
}

/// 进行中的自动化各自跑在哪个会话里。
fn active_automation_sessions(
    automations: &crate::AutomationStore,
) -> std::collections::HashSet<String> {
    automations
        .lock()
        .unwrap_or_else(|error| error.into_inner())
        .snapshot()
        .runs
        .into_iter()
        .filter(|run| run.status.is_active())
        .filter_map(|run| run.session_id)
        .collect()
}

fn save_active_automation_transcripts(
    automations: &crate::AutomationStore,
    acp_sessions: &crate::acp_host::AcpSessions,
) {
    let runs = automations
        .lock()
        .unwrap_or_else(|error| error.into_inner())
        .snapshot()
        .runs;
    for run in runs.into_iter().filter(|run| run.status.is_active()) {
        let Some(session_id) = run.session_id.as_deref() else {
            continue;
        };
        let Some(slot) = acp_sessions.get(session_id) else {
            continue;
        };
        let entries = slot.value.reduced.lock().unwrap().entries().to_vec();
        if entries.is_empty() {
            continue;
        }
        if let Err(error) = automations
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .save_run_transcript(&run.id, &entries)
        {
            dlog(&format!(
                "shutdown: 保存自动化 {} 的对话失败：{error}",
                run.id
            ));
        }
    }
}

pub(super) fn handle_agent_event_op<C: ClientSocket>(
    conn: C,
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
    let _ = smelt_runtime_client::protocol::write_json_line(
        &mut c,
        &serde_json::json!({ "ok": accepted }),
    );
    if let Some(snapshot) = snapshot {
        broadcast_state(event_hub, &snapshot);
    }
}
