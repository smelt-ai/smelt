//! 远程会话目录、网关与 iroh 隧道 op。

use super::super::*;
use std::io::Write;
use std::os::unix::net::UnixStream;
use std::sync::Arc;

pub(super) fn handle_remote_sessions_op(conn: UnixStream, remote_sessions: &RemoteSessions) {
    let mut c = conn;
    let response = match remote_session_snapshot(remote_sessions) {
        Ok(snapshot) => serde_json::json!({ "ok": true, "remote_sessions": snapshot }),
        Err(error) => serde_json::json!({ "ok": false, "error": error }),
    };
    let _ = writeln!(c, "{response}");
}

pub(super) fn handle_remote_rename_resume(
    conn: UnixStream,
    v: &serde_json::Value,
    remote_sessions: &RemoteSessions,
    event_hub: &EventHubHandle,
) {
    let agent_option_id = v["agent_option_id"].as_str().unwrap_or_default();
    let resume_id = v["resume_id"].as_str().unwrap_or_default();
    let title = v["title"].as_str().unwrap_or_default();
    let result = if agent_option_id.is_empty() || resume_id.is_empty() {
        Err("missing agent_option_id or resume_id".to_string())
    } else {
        mutate_remote_catalog(remote_sessions, |catalog| {
            catalog.rename_acp_by_resume_id(agent_option_id, resume_id, title)
        })
        .map(|(changed, snapshot)| {
            if changed {
                broadcast_remote_sessions(event_hub, &snapshot);
            }
        })
    };
    let mut c = conn;
    let response = match result {
        Ok(()) => serde_json::json!({ "ok": true }),
        Err(error) => serde_json::json!({ "ok": false, "error": error }),
    };
    let _ = writeln!(c, "{response}");
}

pub(super) fn handle_remote_start(
    conn: UnixStream,
    v: &serde_json::Value,
    remote_state: &RemoteState,
) {
    let bind = v["bind"].as_str().unwrap_or("127.0.0.1").to_string();
    let port = v["port"].as_u64().unwrap_or(0) as u16;
    let write = v["write"].as_bool().unwrap_or(false);
    let mut c = conn;
    match start_remote_gateway(remote_state, &bind, port, write) {
        Ok((token, addr, write)) => {
            let _ = writeln!(
                c,
                "{}",
                serde_json::json!({
                    "ok": true, "token": token, "addr": addr.to_string(), "write": write
                })
            );
        }
        Err(e) => {
            let _ = writeln!(c, "{}", serde_json::json!({ "ok": false, "err": e }));
        }
    }
}

pub(super) fn handle_remote_stop(conn: UnixStream, remote_state: &RemoteState) {
    stop_remote_gateway(remote_state);
    let mut c = conn;
    let _ = writeln!(c, "{}", serde_json::json!({ "ok": true }));
}

pub(super) fn handle_remote_set_write(
    conn: UnixStream,
    v: &serde_json::Value,
    remote_state: &RemoteState,
) {
    let write = v["write"].as_bool().unwrap_or(false);
    let mut c = conn;
    match set_remote_gateway_write(remote_state, write) {
        Ok(()) => {
            let _ = writeln!(c, "{}", serde_json::json!({ "ok": true, "write": write }));
        }
        Err(error) => {
            let _ = writeln!(c, "{}", serde_json::json!({ "ok": false, "err": error }));
        }
    }
}

pub(super) fn handle_remote_rotate_token(
    conn: UnixStream,
    remote_state: &RemoteState,
    iroh_state: &IrohState,
) {
    // Token 是设备凭证；只有这条显式操作会轮换。先停隧道和网关，确保
    // 旧连接立即失效，调用方随后按原配置重新拉起服务。
    stop_iroh(iroh_state, remote_state);
    let mut c = conn;
    match rotate_remote_token(remote_state) {
        Ok(_) => {
            let _ = writeln!(c, "{}", serde_json::json!({ "ok": true }));
        }
        Err(error) => {
            let _ = writeln!(c, "{}", serde_json::json!({ "ok": false, "err": error }));
        }
    }
}

pub(super) fn handle_remote_status(conn: UnixStream, remote_state: &RemoteState) {
    let mut c = conn;
    let guard = remote_state.lock().unwrap();
    let body = match guard.gateway.as_ref() {
        Some(g) => serde_json::json!({
            "running": true, "token": g.token, "addr": g.addr.to_string(), "write": g.write
        }),
        None => serde_json::json!({ "running": false }),
    };
    let _ = writeln!(c, "{}", body);
}

pub(super) fn handle_iroh_start(
    conn: UnixStream,
    v: &serde_json::Value,
    iroh_state: &IrohState,
    remote_state: &RemoteState,
    iroh_connections: &IrohConnections,
) {
    let write = v["write"].as_bool().unwrap_or(false);
    let relay = v["relay"].as_str().unwrap_or_default();
    let mut c = conn;
    match start_iroh(
        iroh_state,
        remote_state,
        write,
        relay,
        Arc::clone(iroh_connections),
    ) {
        Ok((endpoint_id, token, addr, write, relay)) => {
            // token 一并回：配对码 = endpoint_id + token，缺一不可
            // （隧道只负责把字节送到，鉴权仍归网关）。
            let _ = writeln!(
                c,
                "{}",
                serde_json::json!({
                    "ok": true, "endpoint_id": endpoint_id, "token": token,
                    "addr": addr.to_string(), "write": write,
                    "relay": relay
                })
            );
        }
        Err(e) => {
            let _ = writeln!(c, "{}", serde_json::json!({ "ok": false, "err": e }));
        }
    }
}

pub(super) fn handle_iroh_stop(
    conn: UnixStream,
    iroh_state: &IrohState,
    remote_state: &RemoteState,
) {
    stop_iroh(iroh_state, remote_state);
    let mut c = conn;
    let _ = writeln!(c, "{}", serde_json::json!({ "ok": true }));
}

pub(super) fn handle_iroh_status(
    conn: UnixStream,
    iroh_state: &IrohState,
    remote_state: &RemoteState,
) {
    let mut c = conn;
    let body = match iroh_status(iroh_state) {
        Some((endpoint_id, relay)) => {
            let (token, write) = remote_state
                .lock()
                .unwrap()
                .gateway
                .as_ref()
                .map(|g| (g.token.clone(), g.write))
                .unwrap_or_default();
            serde_json::json!({
                "running": true, "endpoint_id": endpoint_id,
                "token": token, "write": write,
                "relay": relay.url.to_string()
            })
        }
        None => serde_json::json!({ "running": false }),
    };
    let _ = writeln!(c, "{}", body);
}

pub(super) fn handle_iroh_connections(conn: UnixStream, iroh_state: &IrohState) {
    let mut c = conn;
    let connections = get_iroh_connections(iroh_state);
    let _ = writeln!(c, "{}", serde_json::json!({ "connections": connections }));
}
