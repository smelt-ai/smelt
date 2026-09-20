//! 终端会话：open / watch / kill / list / 输入 / resize。

use super::super::session_catalog::{published_session_states, session_connection_counts};
use super::super::*;
use std::io::{BufReader, Write};
use std::os::unix::net::UnixStream;

pub(super) fn handle_list(conn: UnixStream, sessions: &Sessions, acp_sessions: &AcpSessions) {
    // 附带每个会话是否「有客户端连接」（connected），供 GUI 每次启动时
    // 自动清理：死会话和长期无人认领的游离会话都没有连接，一个字段覆盖
    // 两种场景。正常使用中的会话（GUI/远程/移动端正 attach 或旁观）都有
    // 连接，不会被误清。
    let mut ids: Vec<String> = Vec::new();
    let mut states: Vec<serde_json::Value> = Vec::new();
    for state in published_session_states(sessions, acp_sessions) {
        let mut v = serde_json::to_value(&state).unwrap_or(serde_json::Value::Null);
        let (interactive_connections, watcher_connections) =
            session_connection_counts(&state.id, sessions, acp_sessions);
        let connected = interactive_connections > 0 || watcher_connections > 0;
        if let Some(obj) = v.as_object_mut() {
            obj.insert("connected".to_string(), serde_json::json!(connected));
            obj.insert(
                "interactive_connections".to_string(),
                serde_json::json!(interactive_connections),
            );
            obj.insert(
                "watcher_connections".to_string(),
                serde_json::json!(watcher_connections),
            );
        }
        ids.push(state.id);
        states.push(v);
    }
    let mut c = conn;
    let _ = writeln!(
        c,
        "{}",
        serde_json::json!({ "sessions": ids, "states": states })
    );
}
pub(super) fn handle_kill(
    conn: UnixStream,
    v: &serde_json::Value,
    sessions: &Sessions,
    remote_sessions: &RemoteSessions,
    event_hub: &EventHubHandle,
) {
    let id = v["id"].as_str().unwrap_or_default();
    let slot = sessions.get(id);
    let mut removed = None;
    let mut removed_instance = None;
    let mut cleanup_error = None;
    if let Some(slot) = slot {
        // 与 Starting/commit 共用同一 slot 锁。kill 要么看到完整 Live runtime，
        // 要么等创建回滚，绝不在“目录已 Active、map 尚未插入”窗口里穿过。
        let _lifecycle = slot.lifecycle.lock().unwrap();
        if sessions.is_current(id, &slot) {
            let remote_owned =
                is_known_remote_session(remote_sessions, RemoteSessionKind::Terminal, id)
                    == Some(true);
            if remote_owned {
                match set_remote_lifecycle(
                    remote_sessions,
                    event_hub,
                    RemoteSessionKind::Terminal,
                    id,
                    RemoteSessionLifecycle::Closing,
                ) {
                    Ok(true) => {}
                    Ok(false) => {
                        let mut c = conn;
                        let _ = writeln!(
                            c,
                            "{}",
                            serde_json::json!({ "ok": false, "error": "remote session disappeared" })
                        );
                        return;
                    }
                    Err(error) => {
                        let mut c = conn;
                        let _ =
                            writeln!(c, "{}", serde_json::json!({ "ok": false, "error": error }));
                        return;
                    }
                }
            }
            if let Some(s) = sessions.live_in_slot(&slot) {
                removed_instance = Some(s.instance);
                let _output_gate = s.output_gate.lock().unwrap();
                let _input_gate = s.input_gate.lock().unwrap();
                if !s.child.terminate_and_wait(TERMINAL_CHILD_REAP_TIMEOUT) {
                    dlog(&format!(
                        "terminal: kill 后未能确认 pid={} 已由唯一 owner 回收",
                        s.child.pid()
                    ));
                }
                let mut out = s.out.lock().unwrap();
                for c in out.clients.drain(..) {
                    c.close();
                }
                for w in out.watchers.drain(..) {
                    w.close();
                }
            }
            let remote_cleanup = if remote_owned {
                match removed_instance {
                    Some(instance) => remove_remote_session_for_instance(
                        remote_sessions,
                        event_hub,
                        RemoteSessionKind::Terminal,
                        id,
                        instance,
                    ),
                    None => remove_remote_session_if_unbound(
                        remote_sessions,
                        event_hub,
                        RemoteSessionKind::Terminal,
                        id,
                    ),
                }
            } else {
                Ok(false)
            };
            if let Err(error) = remote_cleanup {
                // 进程已经被确认终止，不能因为目录持久化失败把死 runtime
                // 留在注册表里。返回错误后，下一次 kill 可以只重试目录清理。
                cleanup_error = Some(error);
            }
            removed = sessions.remove_if_same(id, &slot);
        }
    } else {
        // 没有 runtime 的旧目录仍可幂等清理；这条分支不允许把未知目录当成本地。
        let remote_owned =
            is_known_remote_session(remote_sessions, RemoteSessionKind::Terminal, id) == Some(true);
        if remote_owned {
            match set_remote_lifecycle(
                remote_sessions,
                event_hub,
                RemoteSessionKind::Terminal,
                id,
                RemoteSessionLifecycle::Closing,
            ) {
                Ok(true) => {}
                Ok(false) => {}
                Err(error) => {
                    let mut c = conn;
                    let _ = writeln!(c, "{}", serde_json::json!({ "ok": false, "error": error }));
                    return;
                }
            }
            if let Err(error) = remove_remote_session_if_unbound(
                remote_sessions,
                event_hub,
                RemoteSessionKind::Terminal,
                id,
            ) {
                let mut c = conn;
                let _ = writeln!(c, "{}", serde_json::json!({ "ok": false, "error": error }));
                return;
            }
        }
    }
    if let Some(session) = removed {
        forget_session(event_hub, id, session.instance);
    } else {
        forget_session(event_hub, id, 0);
    }
    let mut c = conn;
    let response = match cleanup_error {
        Some(error) => serde_json::json!({ "ok": false, "error": error }),
        None => serde_json::json!({ "ok": true }),
    };
    let _ = writeln!(c, "{response}");
}

pub(super) fn handle_action(conn: UnixStream, v: &serde_json::Value, sessions: &Sessions) {
    let id = v["id"].as_str().unwrap_or_default();
    let mut c = conn;
    let payload = match action_payload(v["kind"].as_str(), v["text"].as_str()) {
        Ok(p) => p,
        Err(err) => {
            let _ = writeln!(c, "{}", serde_json::json!({ "ok": false, "err": err }));
            return;
        }
    };
    let write_result = sessions
        .with_live(id, |sess| {
            // Phase 检查和 PTY 写入必须属于同一个实例租约。
            let phase = sess.state.lock().unwrap().phase;
            if !matches!(phase, Phase::AwaitingApproval | Phase::WaitingForUser) {
                return Err("agent 现在不是在等你，稍后再试".to_string());
            }
            write_session_input(sess, &payload).map_err(|error| error.to_string())
        })
        .unwrap_or_else(|| Err("会话不存在".to_string()));
    match write_result {
        Ok(()) => {
            let _ = writeln!(c, "{}", serde_json::json!({ "ok": true }));
        }
        Err(error) => {
            let _ = writeln!(c, "{}", serde_json::json!({ "ok": false, "err": error }));
        }
    }
}

pub(super) fn handle_raw_input(conn: UnixStream, v: &serde_json::Value, sessions: &Sessions) {
    // 原始输入：工作延续，无 phase 门闩。权限在网关 write_enabled，这里只做
    // 「会话在不在 + 载荷非空 + 写进 master」。
    let id = v["id"].as_str().unwrap_or_default();
    let mut c = conn;
    let Some(payload) = input_payload(v) else {
        let _ = writeln!(
            c,
            "{}",
            serde_json::json!({ "ok": false, "err": "需要非空 data" })
        );
        return;
    };
    let write_result = sessions
        .with_live(id, |sess| {
            write_session_input(sess, &payload).map_err(|error| error.to_string())
        })
        .unwrap_or_else(|| Err("会话不存在".to_string()));
    match write_result {
        Ok(()) => {
            let _ = writeln!(c, "{}", serde_json::json!({ "ok": true }));
        }
        Err(error) => {
            let _ = writeln!(c, "{}", serde_json::json!({ "ok": false, "err": error }));
        }
    }
}

pub(super) fn handle_resize(conn: UnixStream, v: &serde_json::Value, sessions: &Sessions) {
    // 手机端按视口改 PTY 尺寸，让 Claude 等 TUI SIGWINCH 重排，
    // 避免「镜像桌面大窗口 → 底部空一大截」。
    let id = v["id"].as_str().unwrap_or_default();
    let cols = v["cols"].as_u64().unwrap_or(0) as u16;
    let rows = v["rows"].as_u64().unwrap_or(0) as u16;
    let cell_w = v["cell_w"].as_u64().unwrap_or(0) as u16;
    let cell_h = v["cell_h"].as_u64().unwrap_or(0) as u16;
    let mut c = conn;
    if cols == 0 || rows == 0 {
        let _ = writeln!(
            c,
            "{}",
            serde_json::json!({ "ok": false, "err": "cols/rows 必须 > 0" })
        );
        return;
    }
    let resized = sessions.with_live(id, |sess| {
        // 尺寸变了才 ioctl/SIGWINCH；同尺寸不再人为 jolt。
        resize_session(sess, cols, rows, cell_w, cell_h);
    });
    if resized.is_none() {
        let _ = writeln!(
            c,
            "{}",
            serde_json::json!({ "ok": false, "err": "会话不存在" })
        );
        return;
    }
    let _ = writeln!(
        c,
        "{}",
        serde_json::json!({ "ok": true, "cols": cols, "rows": rows })
    );
}
pub(crate) fn terminal_error_reply(reason: &str, rows: u16, cols: u16) -> Vec<u8> {
    let reason: String = reason
        .chars()
        .map(|ch| if ch.is_control() { ' ' } else { ch })
        .collect();
    let header = serde_json::json!({
        "ok": false,
        "err": reason,
        "rows": rows,
        "cols": cols,
        "replay_len": 0,
    });
    format!("{header}\n\r\n\x1b[31m{reason}\x1b[0m\r\n").into_bytes()
}

pub(crate) fn write_terminal_error(mut conn: &UnixStream, reason: &str, rows: u16, cols: u16) {
    let _ = conn.write_all(&terminal_error_reply(reason, rows, cols));
}

pub(crate) fn runtime_id_taken_by_other_kind(
    id: &str,
    sessions: &Sessions,
    acp_sessions: &AcpSessions,
    want: RemoteSessionKind,
) -> Option<String> {
    match want {
        RemoteSessionKind::Terminal if acp_sessions.get(id).is_some() => {
            Some("session id already belongs to an ACP runtime".into())
        }
        RemoteSessionKind::Acp if sessions.get(id).is_some() => {
            Some("session id already belongs to a terminal runtime".into())
        }
        _ => None,
    }
}

pub(crate) fn handle_open(
    conn: UnixStream,
    mut reader: BufReader<UnixStream>,
    v: &serde_json::Value,
    sessions: Sessions,
    acp_sessions: AcpSessions,
    event_hub: EventHubHandle,
    remote_sessions: RemoteSessions,
) {
    // 有 viewer 在看：headless 自升级 5 分钟内让路（GUI 有空闲门控）。
    super::auth::record_viewer_seen();
    let id = v["id"].as_str().unwrap_or_default().to_string();
    if id.is_empty() {
        return;
    }
    let cols = v["cols"].as_u64().unwrap_or(80) as u16;
    let rows = v["rows"].as_u64().unwrap_or(24) as u16;
    let cwd = v["cwd"].as_str().map(String::from);
    let launch = v["initial_launch"].as_str().map(String::from);
    let create_if_missing = v["create_if_missing"].as_bool().unwrap_or(true);
    let mut remote_session = match v.get("remote_session") {
        Some(value) => match serde_json::from_value::<RemoteTerminalSession>(value.clone()) {
            Ok(session) if session.id == id => Some(session),
            Ok(_) => {
                write_terminal_error(&conn, "远程目录会话 id 不匹配", rows, cols);
                return;
            }
            Err(error) => {
                write_terminal_error(&conn, &format!("远程目录会话无效：{error}"), rows, cols);
                return;
            }
        },
        None => None,
    };

    // 一个 slot 覆盖 Starting -> Live -> Removed 的全部转换。创建和 kill 都先取得这把
    // 同 ID 生命周期锁，因此目录、runtime 与状态发布不会再出现半提交窗口。
    let (slot, created) = sessions.reserve(&id);
    if let Some(error) =
        runtime_id_taken_by_other_kind(&id, &sessions, &acp_sessions, RemoteSessionKind::Terminal)
    {
        if created {
            let _ = sessions.remove_if_same(&id, &slot);
        }
        write_terminal_error(&conn, &error, rows, cols);
        return;
    }
    let lifecycle = slot.lifecycle.lock().unwrap();
    if !sessions.is_current(&id, &slot) {
        drop(lifecycle);
        write_terminal_error(&conn, "终端会话正在收尾，请重试", rows, cols);
        return;
    }

    let existing = sessions.live_in_slot(&slot);
    if !created && existing.is_none() {
        drop(lifecycle);
        write_terminal_error(&conn, "终端会话正在收尾，请重试", rows, cols);
        return;
    }
    let mut remote_snapshots = Vec::new();
    let mut pty_pump = None;

    let sess = match existing {
        Some(s) => {
            // 不允许远程请求借同 ID 把本地会话变成远程所有；只有目录中已有同类记录的
            // 重试/reattach 才能继续激活。
            if remote_session.is_some() {
                if is_known_remote_session(&remote_sessions, RemoteSessionKind::Terminal, &id)
                    != Some(true)
                {
                    drop(lifecycle);
                    write_terminal_error(
                        &conn,
                        "terminal id already belongs to a local session",
                        rows,
                        cols,
                    );
                    return;
                }
                match activate_remote_session_instance(
                    &remote_sessions,
                    RemoteSessionKind::Terminal,
                    &id,
                    s.instance,
                ) {
                    Ok(snapshot) => remote_snapshots.push(snapshot),
                    Err(error) => {
                        drop(lifecycle);
                        write_terminal_error(
                            &conn,
                            &format!("无法激活远程会话：{error}"),
                            rows,
                            cols,
                        );
                        return;
                    }
                }
            }
            // reattach：只挂 jolt 旗，等客户端首帧 type-1 resize（含真实 cell
            // 像素）再消费。attach 当下不 SIGWINCH：GUI 仍是旧几何，错尺寸重画
            // 才是「显示不全」的根因。
            s.ctl.lock().unwrap().jolt = true;
            s
        }
        None if !create_if_missing => {
            let _ = sessions.remove_if_same(&id, &slot);
            drop(lifecycle);
            write_terminal_error(&conn, "终端会话不存在", rows, cols);
            return;
        }
        None => {
            if let Some(session) = &mut remote_session {
                session.lifecycle = RemoteSessionLifecycle::Creating;
                let snapshot = match mutate_remote_catalog(&remote_sessions, |catalog| {
                    catalog.upsert_terminal(session.clone())
                }) {
                    Ok(((), snapshot)) => snapshot,
                    Err(error) => {
                        let _ = sessions.remove_if_same(&id, &slot);
                        drop(lifecycle);
                        write_terminal_error(
                            &conn,
                            &format!("无法记录远程会话：{error}"),
                            rows,
                            cols,
                        );
                        return;
                    }
                };
                remote_snapshots.push(snapshot);
            }

            let instance = next_session_instance();
            let result = spawn_session(
                &id,
                instance,
                rows,
                cols,
                cwd.as_deref(),
                launch.as_deref(),
                &event_hub,
            );
            let (sess, pty_reader) = match result {
                Ok(result) => result,
                Err(error) => {
                    if remote_session.is_some() {
                        let _ = remove_remote_session_if_unbound(
                            &remote_sessions,
                            &event_hub,
                            RemoteSessionKind::Terminal,
                            &id,
                        );
                    }
                    let _ = sessions.remove_if_same(&id, &slot);
                    drop(lifecycle);
                    smelt_core::app_log::error(
                        "session",
                        &format!("会话 {id} 启动失败：{error:#}"),
                    );
                    write_terminal_error(&conn, &format!("终端启动失败：{error:#}"), rows, cols);
                    return;
                }
            };
            smelt_core::app_log::info("session", &format!("会话 {id} 已创建"));
            let sess = Arc::new(sess);
            // 先让 runtime 在 registry 中可见，再把远程目录改为 Active。lifecycle 锁
            // 同时挡住 kill；这样同步读取目录的调用方也不会观察到 Active 但找不到
            // runtime 的半提交状态。
            if !sessions.commit_if_current(&id, &slot, Arc::clone(&sess)) {
                if !sess.child.terminate_and_wait(TERMINAL_CHILD_REAP_TIMEOUT) {
                    dlog(&format!(
                        "terminal: 创建回滚未能确认 pid={} 已回收",
                        sess.child.pid()
                    ));
                }
                if remote_session.is_some() {
                    let _ = remove_remote_session_if_unbound(
                        &remote_sessions,
                        &event_hub,
                        RemoteSessionKind::Terminal,
                        &id,
                    );
                }
                drop(lifecycle);
                write_terminal_error(&conn, "终端会话创建已取消", rows, cols);
                return;
            }
            if remote_session.is_some() {
                match activate_remote_session_instance(
                    &remote_sessions,
                    RemoteSessionKind::Terminal,
                    &id,
                    sess.instance,
                ) {
                    Ok(snapshot) => remote_snapshots.push(snapshot),
                    Err(error) => {
                        if !sess.child.terminate_and_wait(TERMINAL_CHILD_REAP_TIMEOUT) {
                            dlog(&format!(
                                "terminal: 远程激活回滚未能确认 pid={} 已回收",
                                sess.child.pid()
                            ));
                        }
                        let _ = remove_remote_session_if_unbound(
                            &remote_sessions,
                            &event_hub,
                            RemoteSessionKind::Terminal,
                            &id,
                        );
                        let _ = sessions.remove_if_same(&id, &slot);
                        drop(lifecycle);
                        write_terminal_error(
                            &conn,
                            &format!("无法激活远程会话目录：{error}"),
                            rows,
                            cols,
                        );
                        return;
                    }
                }
            }
            let opened_state = {
                let mut state = sess.state.lock().unwrap();
                bump_state_revision(&mut state);
                state.clone()
            };
            broadcast_state(&event_hub, &opened_state);
            pty_pump = Some((Arc::clone(&sess), pty_reader));
            sess
        }
    };

    for snapshot in remote_snapshots {
        broadcast_remote_sessions(&event_hub, &snapshot);
    }
    if let Some((pump_session, pty_reader)) = pty_pump {
        start_pty_pump(
            pump_session,
            pty_reader,
            id.clone(),
            Arc::clone(&sessions),
            Arc::clone(&event_hub),
            Some(Arc::clone(&remote_sessions)),
            Arc::clone(&slot),
        );
    }

    // attach：回报 PTY 当前尺寸 → 网格 ANSI 快照 → 接管转发。
    //
    // 输出闸门保证 snapshot 与挂载之间不被泵插入。header + keyframe 会先进入该
    // attachment 的专属写队列；若先释放闸门再注册，泵可能 advance(D) 后发现还没
    // client 而丢弃 D，新客户端的网格就永久缺字节（正是「吐快照」要避免的错位）。
    let launch_for_snap = sess.state.lock().unwrap().launch.clone();
    let attached_fd = {
        let _output_gate = sess.output_gate.lock().unwrap();
        let Ok(c) = conn.try_clone() else { return };

        let (cur_cols, cur_rows, snapshot) = {
            let ctl = sess.ctl.lock().unwrap();
            let term = sess.term.lock().unwrap();
            let mut snapshot = terminal_geometry_osc(
                &sess.geometry_token,
                TerminalGeometryOsc {
                    cols: ctl.cols,
                    rows: ctl.rows,
                    cell_width: ctl.cell_w,
                    cell_height: ctl.cell_h,
                    remote_controlled: ctl.remote_viewports > 0,
                },
            );
            // 快照分流只看真实 ALT_SCREEN；启动命令不能把主屏 scrollback 错当成 TUI。
            snapshot.extend(snapshot_ansi(&term, launch_for_snap.as_deref()));
            drop(term);
            let geometry = (ctl.cols, ctl.rows);
            drop(ctl);
            (geometry.0, geometry.1, snapshot)
        };

        // replay_len = 快照字节数：客户端仍用它划「历史/实时」边界，跳过快照里的
        // 历史 OSC 9（网格快照本身不含旧通知序列，但边界语义保留兼容）。
        let replay_len = snapshot.len();
        let mut initial = format!(
            "{}\n",
            serde_json::json!({
                "cols": cur_cols,
                "rows": cur_rows,
                "replay_len": replay_len,
                "geometry_token": sess.geometry_token.as_str(),
                "daemon_handles_color_requests": true,
            })
        )
        .into_bytes();
        initial.extend_from_slice(&snapshot);
        let Ok(attachment) = OutputAttachment::new(c, initial, &id, "attachment") else {
            return;
        };
        let fd = attachment.fd;
        sess.out.lock().unwrap().clients.push(attachment);
        fd
    };
    drop(lifecycle);

    // reattach jolt：只挂旗，不定时补枪。
    // 客户端首帧 type-1 resize（含真实 cell 像素）进 `resize_session` 即消费 jolt；
    // 同尺寸无 resize 即无 SIGWINCH（tmux 同款：尺寸没变就不重画，快照本身准确）。
    // 不在此处 SIGWINCH：attach 当下 GUI 仍是旧几何，错尺寸重画即“显示不全”根因。
    // 旧 350ms+200ms 双补枪已删除：时序猜+无条件第二枪正是叠补丁，白抖还会把
    // 不切备用屏 CLI 的整段对话重排重印一遍。

    // 帧循环：输入 / resize，直到客户端断开。
    loop {
        let mut hdr = [0u8; 5];
        if reader.read_exact(&mut hdr).is_err() {
            break;
        }
        let len = u32::from_be_bytes([hdr[1], hdr[2], hdr[3], hdr[4]]) as usize;
        if len > (1 << 20) {
            break; // 异常长度，掐断
        }
        let mut payload = vec![0u8; len];
        if reader.read_exact(&mut payload).is_err() {
            break;
        }
        match hdr[0] {
            0 => {
                if sessions
                    .with_current(&id, &slot, |session| {
                        // 桌面侧有人在敲键盘：人回到 PC 了，手机离开后留下的几何宽限立即作废，
                        // 不要让桌面对着一个手机尺寸的网格干等。
                        cancel_remote_viewport_grace(session);
                        let _ = write_session_input(session, &payload);
                    })
                    .is_none()
                {
                    break;
                }
            }
            1 if len == 8 || len == 16 => {
                let cols = u32::from_be_bytes(payload[0..4].try_into().unwrap()) as u16;
                let rows = u32::from_be_bytes(payload[4..8].try_into().unwrap()) as u16;
                // 可选：单元格像素（新客户端 16 字节帧）；整窗像素 = 行列 × 格像素。
                let (cell_w, cell_h) = if len == 16 {
                    let cw = u32::from_be_bytes(payload[8..12].try_into().unwrap()) as u16;
                    let ch = u32::from_be_bytes(payload[12..16].try_into().unwrap()) as u16;
                    (cw, ch)
                } else {
                    (0, 0)
                };
                if sessions
                    .with_current(&id, &slot, |session| {
                        resize_session(session, cols, rows, cell_w, cell_h);
                    })
                    .is_none()
                {
                    break;
                }
            }
            _ => break,
        }
    }

    // 断开：只摘掉本 attachment，不影响同一 PTY 的其它渲染层。
    let _output_gate = sess.output_gate.lock().unwrap();
    let mut out = sess.out.lock().unwrap();
    if let Some(pos) = out.clients.iter().position(|c| c.fd == attached_fd) {
        let client = out.clients.swap_remove(pos);
        client.close();
    }
}

pub(crate) struct RemoteViewportLease {
    pub(crate) sessions: Sessions,
    pub(crate) id: String,
    pub(crate) slot: Arc<TerminalSlot<Session>>,
}

/// 断开一律走宽限，不分「切后台」还是「退出会话页」。
///
/// 两者在链路上无法区分，更重要的是：即便能区分，退出页面也**不该**立刻还尺寸——
/// 退出再进来是手机上最频繁的动作，中间还一次、进来再抢一次，等于白白让不切备用屏的
/// CLI 把整段对话重印两遍。真正的归还信号是桌面侧有人敲键盘。
impl Drop for RemoteViewportLease {
    fn drop(&mut self) {
        let paused = self
            .sessions
            .with_current(&self.id, &self.slot, |session| {
                pause_remote_viewport(session)
            })
            .flatten();
        let Some(seq) = paused else {
            return;
        };
        let sessions = Arc::clone(&self.sessions);
        let slot = Arc::clone(&self.slot);
        let id = self.id.clone();
        thread::spawn(move || {
            thread::sleep(REMOTE_VIEWPORT_GRACE);
            let _ = sessions.with_current(&id, &slot, |session| {
                expire_remote_viewport(session, seq);
            });
        });
    }
}

/// 旁观/远程渲染连接。普通 watch 仍严格只读；声明
/// `controls_geometry` 的移动端连接在自己的生命周期内持有 PTY 尺寸租约，并可在
/// 同一连接上发送 type-1 resize 帧。跟 `handle_open` 的核心区别——
/// 1. 不兜底 spawn：会话必须已存在，旁观一个不存在的会话没有意义；
/// 2. 不影响 `out.clients`，也不顶替其它 watcher——`push` 进去，多个旁观者可并存；
/// 3. 移动端只取得尺寸所有权，不替换桌面 attachment；断开时自动归还。
pub(crate) fn handle_watch(
    conn: UnixStream,
    mut reader: BufReader<UnixStream>,
    v: &serde_json::Value,
    sessions: Sessions,
) {
    // 有 viewer 在看：headless 自升级 5 分钟内让路（GUI 有空闲门控）。
    super::auth::record_viewer_seen();
    let id = v["id"].as_str().unwrap_or_default().to_string();
    if id.is_empty() {
        return;
    }
    let controls_geometry = v["controls_geometry"].as_bool().unwrap_or(false);
    // 消费端声明的 scrollback 预算。不传 = 要全量（桌面、老网关），行为不变。
    let max_scrollback_lines = v["max_scrollback_lines"]
        .as_u64()
        .map(|lines| lines.min(SNAPSHOT_MAX_LINES as u64) as usize)
        .unwrap_or(SNAPSHOT_MAX_LINES);
    let remote_geometry = controls_geometry.then(|| {
        (
            v["cols"].as_u64().unwrap_or(0).min(1000) as u16,
            v["rows"].as_u64().unwrap_or(0).min(1000) as u16,
            v["cell_w"].as_u64().unwrap_or(0).min(256) as u16,
            v["cell_h"].as_u64().unwrap_or(0).min(256) as u16,
        )
    });
    if remote_geometry.is_some_and(|(cols, rows, _, _)| cols == 0 || rows == 0) {
        return;
    }
    let Some(slot) = sessions.get(&id) else {
        return;
    };
    let attached = sessions.with_current(&id, &slot, |session| {
        if let Some((cols, rows, cell_w, cell_h)) = remote_geometry {
            let _ = begin_remote_viewport(session, cols, rows, cell_w, cell_h);
        }
        let attached = (|| {
            let (cur_cols, cur_rows) = {
                let ctl = session.ctl.lock().unwrap();
                (ctl.cols, ctl.rows)
            };
            let launch_for_snap = session.state.lock().unwrap().launch.clone();
            let _output_gate = session.output_gate.lock().unwrap();
            let c = conn.try_clone().ok()?;
            let term = session.term.lock().unwrap();
            let snapshot =
                snapshot_ansi_for_watch(&term, launch_for_snap.as_deref(), max_scrollback_lines);
            // 客户端靠这个判断「还有没有更老的内容」：截断过就还有，没截断就到头了。
            let history_lines = available_snapshot_lines(&term);
            drop(term);

            let replay_len = snapshot.len();
            let mut initial = format!(
                "{}\n",
                serde_json::json!({
                    "cols": cur_cols,
                    "rows": cur_rows,
                    "replay_len": replay_len,
                    "history_lines": history_lines,
                })
            )
            .into_bytes();
            initial.extend_from_slice(&snapshot);
            let attachment = OutputAttachment::new(c, initial, &id, "watcher").ok()?;
            let fd = attachment.fd;
            session.out.lock().unwrap().watchers.push(attachment);
            Some((Arc::clone(session), fd))
        })();
        if attached.is_none() && remote_geometry.is_some() {
            end_remote_viewport(session);
        }
        attached
    });
    let Some(Some((sess, attached_fd))) = attached else {
        return;
    };
    let _remote_viewport = remote_geometry.map(|_| RemoteViewportLease {
        sessions: Arc::clone(&sessions),
        id: id.clone(),
        slot: Arc::clone(&slot),
    });

    // 几何变化的那一次 SIGWINCH 已在 `begin_remote_viewport` 内打过（仅变才打，
    // 同尺寸续租一次不抖）。watcher 挂载后不再补抖：快照即准确画面，重绘画
    // 面随实时流自然到达。

    if controls_geometry {
        loop {
            let mut header = [0u8; 5];
            if reader.read_exact(&mut header).is_err() {
                break;
            }
            let len = u32::from_be_bytes([header[1], header[2], header[3], header[4]]) as usize;
            if header[0] != 1 || (len != 8 && len != 16) {
                break;
            }
            let mut payload = vec![0u8; len];
            if reader.read_exact(&mut payload).is_err() {
                break;
            }
            let cols = u32::from_be_bytes(payload[0..4].try_into().unwrap()) as u16;
            let rows = u32::from_be_bytes(payload[4..8].try_into().unwrap()) as u16;
            let (cell_w, cell_h) = if len == 16 {
                (
                    u32::from_be_bytes(payload[8..12].try_into().unwrap()) as u16,
                    u32::from_be_bytes(payload[12..16].try_into().unwrap()) as u16,
                )
            } else {
                (0, 0)
            };
            if cols == 0 || rows == 0 {
                break;
            }
            if sessions
                .with_current(&id, &slot, |session| {
                    resize_session_remote(session, cols, rows, cell_w, cell_h);
                })
                .is_none()
            {
                break;
            }
        }
    } else {
        // Legacy watch remains read-only. Any byte (or EOF) ends the watch.
        let mut scratch = [0u8; 64];
        let _ = reader.read(&mut scratch);
    }

    let _output_gate = sess.output_gate.lock().unwrap();
    let mut out = sess.out.lock().unwrap();
    if let Some(pos) = out.watchers.iter().position(|w| w.fd == attached_fd) {
        let watcher = out.watchers.swap_remove(pos);
        watcher.close();
    }
}
