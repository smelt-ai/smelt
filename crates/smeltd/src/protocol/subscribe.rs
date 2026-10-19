//! 事件订阅 op。

use super::super::session_catalog::published_session_states;
use super::super::*;
use super::{
    AuthenticatedEventClient, SystemPeerIdentityProvider, ViewerLease,
    authenticate_event_connection,
};
use smelt_event_bus::Delivery;
use smelt_event_protocol::{
    EVENT_BUS_PROTOCOL_VERSION, FIRST_PARTY_DESKTOP_PLUGIN_ID, SubscribeErrorCode,
    SubscribeMessage, SubscribeRequest,
};
use std::collections::BTreeSet;
use std::os::unix::net::UnixStream;

pub(super) fn handle_event_subscribe_op(
    conn: UnixStream,
    v: &serde_json::Value,
    principal: &crate::devices::Principal,
    event_hub: &EventHubHandle,
) {
    // 已配对的远程设备就是另一台桌面（D3：配对即全部能力）。它的流经 socketpair
    // 接进来，对端进程是 smeltd 自己，查进程身份没有意义。
    if principal.is_remote() {
        let identity = Ok(super::auth::first_party_event_client(
            smelt_event_protocol::FirstPartyClientKind::Desktop,
        ));
        handle_event_subscribe(conn, v.get("request"), identity, event_hub);
        return;
    }
    let identity = daemon_executable_path()
        .map_err(|error| format!("cannot determine smeltd executable: {error}"))
        .and_then(|daemon_executable| {
            authenticate_event_connection(
                v,
                &conn,
                &SystemPeerIdentityProvider,
                std::process::id(),
                &daemon_executable,
            )
        });
    handle_event_subscribe(conn, v.get("request"), identity, event_hub);
}
pub(crate) fn collect_subscription_snapshot(
    sessions: &Sessions,
    acp_sessions: &AcpSessions,
    remote_sessions: &RemoteSessions,
    workspace_menu: &WorkspaceMenuStore,
    automations: AutomationFile,
) -> DaemonProjectionSeed {
    DaemonProjectionSeed {
        sessions: published_session_states(sessions, acp_sessions),
        // A catalog load failure remains fail-closed at the command boundary. Omitting it from a
        // state snapshot prevents clients from mistaking an unreadable durable catalog for empty.
        remote_sessions: remote_session_snapshot(remote_sessions).ok(),
        workspace_menu: Some(workspace_menu_snapshot(workspace_menu)),
        automations: Some(automations),
    }
}

pub(super) fn write_event_protocol_value(
    conn: &mut UnixStream,
    value: impl serde::Serialize,
) -> Result<(), String> {
    let value = serde_json::to_value(value).map_err(|error| error.to_string())?;
    smelt_runtime_client::protocol::write_json_line(conn, &value).map_err(|error| error.to_string())
}

pub(super) fn write_event_protocol_error(conn: &mut UnixStream, error: impl std::fmt::Display) {
    let _ = write_event_protocol_value(
        conn,
        SubscribeMessage::<serde_json::Value>::Error {
            code: SubscribeErrorCode::Rejected,
            message: error.to_string(),
        },
    );
}

const CONNECTION_MAILBOX_LIMIT: usize = 256;

enum ConnectionMail {
    /// 订阅建立后客户端不该再发任何东西；发了就是协议错误，读出错也算。
    ClientError(String),
    Delivery(Delivery),
}

struct ConnectionMailbox {
    state: Mutex<ConnectionMailboxState>,
    wake: Condvar,
}

struct ConnectionMailboxState {
    items: VecDeque<ConnectionMail>,
    client_closed: bool,
    hub_closed: bool,
    stopped: bool,
}

impl ConnectionMailbox {
    fn new() -> Self {
        Self {
            state: Mutex::new(ConnectionMailboxState {
                items: VecDeque::new(),
                client_closed: false,
                hub_closed: false,
                stopped: false,
            }),
            wake: Condvar::new(),
        }
    }

    fn push(&self, item: ConnectionMail) -> bool {
        let mut state = self.state.lock().unwrap();
        loop {
            if state.stopped {
                return false;
            }
            if state.items.len() < CONNECTION_MAILBOX_LIMIT {
                state.items.push_back(item);
                self.wake.notify_all();
                return true;
            }
            state = self.wake.wait(state).unwrap();
        }
    }

    fn mark_client_closed(&self) {
        let mut state = self.state.lock().unwrap();
        state.client_closed = true;
        self.wake.notify_all();
    }

    fn mark_hub_closed(&self) {
        let mut state = self.state.lock().unwrap();
        state.hub_closed = true;
        self.wake.notify_all();
    }

    fn stop(&self) {
        let mut state = self.state.lock().unwrap();
        state.stopped = true;
        self.wake.notify_all();
    }

    /// 客户端出错先交，再交出至多一条投递。客户端关掉或订阅通道关掉之后，剩下的投递
    /// 不再交。
    fn next(&self) -> Option<ConnectionMail> {
        let mut state = self.state.lock().unwrap();
        loop {
            if let Some(index) = state
                .items
                .iter()
                .position(|item| matches!(item, ConnectionMail::ClientError(_)))
            {
                let item = state.items.remove(index).expect("刚找到的客户端错误");
                self.wake.notify_all();
                return Some(item);
            }
            if state.client_closed || state.hub_closed || state.stopped {
                return None;
            }
            if let Some(item) = state.items.pop_front() {
                self.wake.notify_all();
                return Some(item);
            }
            state = self.wake.wait(state).unwrap();
        }
    }
}

pub(crate) fn handle_event_subscribe(
    mut conn: UnixStream,
    request: Option<&serde_json::Value>,
    identity: Result<AuthenticatedEventClient, String>,
    event_hub: &EventHubHandle,
) {
    let setup = (|| {
        let identity = identity?;
        let request = request
            .cloned()
            .ok_or_else(|| "missing event subscribe request".to_string())
            .and_then(|request| {
                serde_json::from_value::<SubscribeRequest>(request)
                    .map_err(|error| format!("invalid event subscribe request: {error}"))
            })?;
        if request.protocol.min > request.protocol.max
            || request.protocol.min > EVENT_BUS_PROTOCOL_VERSION
            || request.protocol.max < EVENT_BUS_PROTOCOL_VERSION
        {
            return Err(format!(
                "unsupported event protocol range {}..={}",
                request.protocol.min, request.protocol.max
            ));
        }
        let topics = request
            .subscription
            .topics
            .iter()
            .cloned()
            .collect::<BTreeSet<_>>();
        if topics.len() != request.subscription.topics.len() {
            return Err("subscription contains duplicate topics".to_string());
        }
        let subscription = event_hub
            .subscribe(
                identity.plugin_id.clone(),
                request.subscription.id.clone(),
                topics,
                &identity.capabilities,
            )
            .map_err(|error| error.to_string())?;
        Ok((identity, request, subscription))
    })();
    let (identity, request, subscription) = match setup {
        Ok(setup) => setup,
        Err(error) => {
            write_event_protocol_error(&mut conn, error);
            return;
        }
    };
    // 桌面 subscribe 是 GUI 活着的长连接。headless 自升级必须让路，不能靠
    // 「最近鉴权过」这种会过期的时间戳。
    let _desktop_viewer =
        (identity.plugin_id.as_str() == FIRST_PARTY_DESKTOP_PLUGIN_ID).then(ViewerLease::acquire);

    if conn.set_write_timeout(Some(CLIENT_WRITE_TIMEOUT)).is_err() {
        let _ = event_hub
            .runtime()
            .unsubscribe(&identity.plugin_id, &request.subscription.id);
        return;
    }
    let Ok(reader) = conn.try_clone() else {
        let _ = event_hub
            .runtime()
            .unsubscribe(&identity.plugin_id, &request.subscription.id);
        return;
    };

    // 投递线程独占 `subscription.recv`。滞后时它自己先丢掉通道里溢出的增量，
    // 再停在 resume 上，等主循环把快照写完。两条线程同时 recv / discard 会把
    // 已经落后的旧增量又交出去。
    let mailbox = Arc::new(ConnectionMailbox::new());
    let (resume_tx, resume_rx) = std::sync::mpsc::channel();
    let forward_mailbox = Arc::clone(&mailbox);
    let forwarder = thread::Builder::new()
        .name("smelt-subscribe-fwd".to_string())
        .spawn(move || {
            loop {
                match subscription.recv() {
                    Ok(Delivery::Lag {
                        dropped,
                        resume_after,
                    }) => {
                        subscription.discard_pending();
                        if !forward_mailbox.push(ConnectionMail::Delivery(Delivery::Lag {
                            dropped,
                            resume_after,
                        })) {
                            break;
                        }
                        if resume_rx.recv().is_err() {
                            break;
                        }
                    }
                    Ok(delivery) => {
                        if !forward_mailbox.push(ConnectionMail::Delivery(delivery)) {
                            break;
                        }
                    }
                    Err(_) => {
                        forward_mailbox.mark_hub_closed();
                        break;
                    }
                }
            }
        });
    let Ok(forwarder) = forwarder else {
        let _ = event_hub
            .runtime()
            .unsubscribe(&identity.plugin_id, &request.subscription.id);
        return;
    };
    let reader_mailbox = Arc::clone(&mailbox);
    let reader_worker = thread::Builder::new()
        .name("smelt-subscribe-read".to_string())
        .spawn(move || {
            let reader = BufReader::new(reader);
            if let Some(line) = reader.lines().next() {
                let error = match line {
                    Ok(_) => "unexpected client message after subscribe".to_string(),
                    Err(error) => error.to_string(),
                };
                reader_mailbox.push(ConnectionMail::ClientError(error));
                return;
            }
            reader_mailbox.mark_client_closed();
        });
    let Ok(reader_worker) = reader_worker else {
        drop(resume_tx);
        mailbox.stop();
        let _ = event_hub
            .runtime()
            .unsubscribe(&identity.plugin_id, &request.subscription.id);
        let _ = forwarder.join();
        return;
    };

    'connection: loop {
        let Some(mail) = mailbox.next() else {
            break;
        };
        match mail {
            ConnectionMail::ClientError(error) => {
                write_event_protocol_error(&mut conn, error);
                break 'connection;
            }
            ConnectionMail::Delivery(Delivery::Lag {
                dropped,
                resume_after,
            }) => {
                if write_event_protocol_value(
                    &mut conn,
                    SubscribeMessage::<serde_json::Value>::Lag {
                        dropped,
                        resume_after,
                    },
                )
                .is_err()
                {
                    break;
                }
                let snapshots = match event_hub
                    .runtime()
                    .recover_snapshots(&identity.plugin_id, &request.subscription.id)
                {
                    Ok(snapshots) => snapshots,
                    Err(error) => {
                        write_event_protocol_error(&mut conn, error);
                        break;
                    }
                };
                for snapshot in snapshots {
                    if write_event_protocol_value(
                        &mut conn,
                        event_hub::delivery_to_subscribe_message(Delivery::Snapshot(snapshot)),
                    )
                    .is_err()
                    {
                        break 'connection;
                    }
                }
                let _ = resume_tx.send(());
            }
            ConnectionMail::Delivery(delivery) => {
                if write_event_protocol_value(
                    &mut conn,
                    event_hub::delivery_to_subscribe_message(delivery),
                )
                .is_err()
                {
                    break;
                }
            }
        }
    }

    drop(resume_tx);
    mailbox.stop();
    let _ = conn.shutdown(Shutdown::Both);
    let _ = event_hub
        .runtime()
        .unsubscribe(&identity.plugin_id, &request.subscription.id);
    let _ = reader_worker.join();
    let _ = forwarder.join();
}
