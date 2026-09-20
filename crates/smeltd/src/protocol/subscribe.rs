//! 事件订阅与桌面插件 UI op。

use super::super::session_catalog::published_session_states;
use super::super::*;
use super::{
    AuthenticatedEventClient, SystemPeerIdentityProvider, authenticate_event_connection,
    require_desktop_identity,
};
use smelt_event_bus::Delivery;
use smelt_plugin_api::{
    DeliveryClass, EVENT_BUS_PROTOCOL_VERSION, InvocationRequest, SubscribeControlMessage,
    SubscribeErrorCode, SubscribeMessage, SubscribeRequest,
};
use std::collections::BTreeSet;
use std::io::Write;
use std::os::unix::net::UnixStream;

pub(super) fn handle_event_subscribe_op(
    conn: UnixStream,
    v: &serde_json::Value,
    event_hub: &EventHubHandle,
) {
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

pub(super) fn handle_plugin_contributions(
    mut conn: UnixStream,
    identity: Result<AuthenticatedEventClient, String>,
) {
    let response = match identity.and_then(require_desktop_identity) {
        Ok(_) => serde_json::json!({
            "ok": true,
            "plugins": crate::plugin_runtime::contributions(),
        }),
        Err(error) => serde_json::json!({ "ok": false, "error": error }),
    };
    let _ = writeln!(conn, "{response}");
}

pub(super) fn handle_plugin_statuses(
    mut conn: UnixStream,
    identity: Result<AuthenticatedEventClient, String>,
) {
    let response = match identity.and_then(require_desktop_identity) {
        Ok(_) => serde_json::json!({
            "ok": true,
            "statuses": crate::plugin_runtime::statuses(),
        }),
        Err(error) => serde_json::json!({ "ok": false, "error": error }),
    };
    let _ = writeln!(conn, "{response}");
}

/// 单次 invoke 上限。浏览器 SSO 在插件后台线程跑，不再占用这条路径。
pub(super) const MAX_PLUGIN_INVOKE_MS: u64 = 30_000;

pub(super) fn plugin_invoke_timeout(remaining_ms: u64) -> std::time::Duration {
    std::time::Duration::from_millis(remaining_ms.min(MAX_PLUGIN_INVOKE_MS))
}

pub(super) fn handle_plugin_invoke(
    mut conn: UnixStream,
    value: &serde_json::Value,
    identity: Result<AuthenticatedEventClient, String>,
) {
    let result = (|| -> Result<smelt_plugin_api::InvocationResponse, String> {
        require_desktop_identity(identity?)?;
        let plugin_id = value
            .get("plugin_id")
            .cloned()
            .ok_or_else(|| "missing plugin_id".to_string())
            .and_then(|value| serde_json::from_value(value).map_err(|error| error.to_string()))?;
        let request: InvocationRequest = value
            .get("request")
            .cloned()
            .ok_or_else(|| "missing invocation request".to_string())
            .and_then(|value| serde_json::from_value(value).map_err(|error| error.to_string()))?;
        let remaining_ms = request
            .deadline_ms
            .saturating_sub(crate::event_hub::now_ms());
        if remaining_ms == 0 {
            return Err("plugin invocation deadline has expired".to_string());
        }
        crate::plugin_runtime::invoke(plugin_id, request, plugin_invoke_timeout(remaining_ms))
    })();
    let response = match result {
        Ok(response) => serde_json::json!({ "ok": true, "response": response }),
        Err(error) => serde_json::json!({ "ok": false, "error": error }),
    };
    let _ = writeln!(conn, "{response}");
}

pub(super) fn handle_plugin_set_enabled(
    mut conn: UnixStream,
    value: &serde_json::Value,
    identity: Result<AuthenticatedEventClient, String>,
) {
    let result = (|| -> Result<(), String> {
        require_desktop_identity(identity?)?;
        let plugin_id = value
            .get("plugin_id")
            .cloned()
            .ok_or_else(|| "missing plugin_id".to_string())
            .and_then(|value| serde_json::from_value(value).map_err(|error| error.to_string()))?;
        let enabled = value
            .get("enabled")
            .and_then(serde_json::Value::as_bool)
            .ok_or_else(|| "missing enabled".to_string())?;
        crate::plugin_runtime::set_enabled(plugin_id, enabled)
    })();
    let response = match result {
        Ok(()) => serde_json::json!({ "ok": true }),
        Err(error) => serde_json::json!({ "ok": false, "error": error }),
    };
    let _ = writeln!(conn, "{response}");
}

pub(super) fn handle_plugin_reload(
    mut conn: UnixStream,
    identity: Result<AuthenticatedEventClient, String>,
) {
    let response = match identity.and_then(require_desktop_identity) {
        Ok(_) => {
            super::plugin_runtime::reload_for_package_change();
            serde_json::json!({ "ok": true })
        }
        Err(error) => serde_json::json!({ "ok": false, "error": error }),
    };
    let _ = writeln!(conn, "{response}");
}

pub(super) fn write_event_protocol_value(
    conn: &mut UnixStream,
    value: impl serde::Serialize,
) -> Result<(), String> {
    let value = serde_json::to_value(value).map_err(|error| error.to_string())?;
    writeln!(conn, "{value}").map_err(|error| error.to_string())
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
                request.subscription.delivery,
                &identity.capabilities,
            )
            .map_err(|error| error.to_string())?;
        let persisted_cursor = event_hub
            .runtime()
            .subscription_cursor(&identity.plugin_id, &request.subscription.id)
            .map_err(|error| error.to_string())?;
        if request
            .cursor
            .is_some_and(|cursor| cursor != persisted_cursor)
        {
            let _ = event_hub
                .runtime()
                .unsubscribe(&identity.plugin_id, &request.subscription.id);
            return Err(format!(
                "requested cursor does not match authenticated persisted cursor {persisted_cursor}"
            ));
        }
        Ok((identity, request, subscription, persisted_cursor))
    })();
    let (identity, request, subscription, persisted_cursor) = match setup {
        Ok(setup) => setup,
        Err(error) => {
            write_event_protocol_error(&mut conn, error);
            return;
        }
    };

    if conn.set_write_timeout(Some(CLIENT_WRITE_TIMEOUT)).is_err() {
        let _ = event_hub
            .runtime()
            .unsubscribe(&identity.plugin_id, &request.subscription.id);
        return;
    }
    if request.subscription.delivery == DeliveryClass::Durable
        && write_event_protocol_value(
            &mut conn,
            SubscribeMessage::<serde_json::Value>::Cursor {
                last_acked_sequence: persisted_cursor,
            },
        )
        .is_err()
    {
        let _ = event_hub
            .runtime()
            .unsubscribe(&identity.plugin_id, &request.subscription.id);
        return;
    }

    let (control_tx, control_rx) = std::sync::mpsc::channel();
    let Ok(reader) = conn.try_clone() else {
        let _ = event_hub
            .runtime()
            .unsubscribe(&identity.plugin_id, &request.subscription.id);
        return;
    };
    let reader_worker = thread::spawn(move || {
        let reader = BufReader::new(reader);
        for line in reader.lines() {
            let message = line.map_err(|error| error.to_string()).and_then(|line| {
                serde_json::from_str::<SubscribeControlMessage>(&line)
                    .map_err(|error| format!("invalid ack/nack: {error}"))
            });
            if control_tx.send(message).is_err() {
                return;
            }
        }
    });

    if request.subscription.delivery == DeliveryClass::Durable
        && let Err(error) = event_hub.runtime().dispatch_durable(
            &identity.plugin_id,
            &request.subscription.id,
            smelt_event_bus::DEFAULT_DURABLE_LOAD_BATCH,
        )
    {
        write_event_protocol_error(&mut conn, error);
    }

    'connection: loop {
        loop {
            match control_rx.try_recv() {
                Ok(control) => {
                    let result = match control {
                        Ok(SubscribeControlMessage::Ack { ack })
                            if ack.subscription_id != request.subscription.id =>
                        {
                            Err(smelt_event_bus::EventBusError::new(
                                "ack subscription_id does not match this connection",
                            ))
                        }
                        Ok(SubscribeControlMessage::Ack { ack }) => {
                            event_hub.runtime().ack(&identity.plugin_id, &ack)
                        }
                        Ok(SubscribeControlMessage::Nack { nack })
                            if nack.subscription_id != request.subscription.id =>
                        {
                            Err(smelt_event_bus::EventBusError::new(
                                "nack subscription_id does not match this connection",
                            ))
                        }
                        Ok(SubscribeControlMessage::Nack { nack }) => event_hub.runtime().nack(
                            &identity.plugin_id,
                            &nack,
                            crate::event_hub::now_ms(),
                        ),
                        Err(error) => {
                            write_event_protocol_error(&mut conn, error);
                            break 'connection;
                        }
                    };
                    if let Err(error) = result {
                        write_event_protocol_error(&mut conn, error);
                        break 'connection;
                    }
                    if request.subscription.delivery == DeliveryClass::Durable
                        && let Err(error) = event_hub.runtime().dispatch_durable(
                            &identity.plugin_id,
                            &request.subscription.id,
                            smelt_event_bus::DEFAULT_DURABLE_LOAD_BATCH,
                        )
                    {
                        write_event_protocol_error(&mut conn, error);
                        break 'connection;
                    }
                }
                Err(std::sync::mpsc::TryRecvError::Empty) => break,
                Err(std::sync::mpsc::TryRecvError::Disconnected) => break 'connection,
            }
        }

        match subscription.try_recv() {
            Ok(Delivery::Lag {
                dropped,
                resume_after,
            }) => {
                subscription.discard_pending();
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
            }
            Ok(delivery) => {
                if write_event_protocol_value(
                    &mut conn,
                    event_hub::delivery_to_subscribe_message(delivery),
                )
                .is_err()
                {
                    break;
                }
            }
            Err(std::sync::mpsc::TryRecvError::Empty) => {
                thread::sleep(Duration::from_millis(10));
            }
            Err(std::sync::mpsc::TryRecvError::Disconnected) => break,
        }
    }

    let _ = conn.shutdown(Shutdown::Both);
    let _ = reader_worker.join();
    let _ = event_hub
        .runtime()
        .unsubscribe(&identity.plugin_id, &request.subscription.id);
    if let Err(error) = event_hub.runtime().flush_cursors() {
        eprintln!("[event-hub] disconnect cursor flush failed: {error}");
    }
}
