//! 对端节点：和本节点双向配对过的其它运行时（见 `docs/cross-machine.md`）。
//!
//! 一台电脑一个身份：往外拨对端用的是接受连入的同一个 iroh endpoint（远程访问开着时），
//! 远程访问关着时才用同一把密钥单独绑一个只拨出的 endpoint。两个不会同时在：同一把
//! 密钥绑两个 endpoint，relay 上后连上的会把先连上的挤掉。
//!
//! 本机的桌面、命令行、智能体经 `op=forward` 由这里转发到对端；智能体会话进程树里的
//! 调用方只能去打开了 `agent_access` 的对端，也改不了对端设置。

use std::collections::HashMap;
use std::io::BufReader;
use std::net::Shutdown;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use smelt_core::control_api::{NodePair, NodeUnpair, PeerPair, PeerUpdate};
use smelt_core::peer_node::{
    NodeInfo, NodePairParams, PeerIdParams, PeerLink, PeerLinkResult, PeerList, PeerNode,
    PeerPairParams, PeerRoute, PeerUpdateParams,
};
use smelt_iroh::runtime::{IrohClient, RuntimeConnector};
use smelt_runtime_client::RuntimeClient;
use smelt_runtime_client::protocol::control::{
    ControlCall, ControlError, ControlErrorCode, ControlMethod, ControlReason, EmptyParams,
    decode_params,
};
use smelt_runtime_client::protocol::forward::{ForwardErrorCode, ForwardReply};
use smelt_store::PeerNodeRecord;

use crate::AcpSessions;
use crate::client_conn::ClientSocket;
use crate::devices::Principal;

/// 远程访问关着时只拨出的 endpoint。远程访问的 endpoint 绑上时丢掉（见 [`endpoint_changed`]）。
static DIAL_ONLY: Mutex<Option<Arc<IrohClient>>> = Mutex::new(None);

/// 每个对端一个连接器：同一条 QUIC 连接上开流。拨号用的 endpoint 换了就按新的重建。
static CONNECTORS: Mutex<Option<HashMap<String, Arc<RuntimeConnector>>>> = Mutex::new(None);

/// 配对要走公网拨号，比本机调用慢得多。
const PAIR_TIMEOUT: Duration = Duration::from_secs(30);
/// 远程访问正在绑定时，拨号最多等它这么久。
const BIND_WAIT: Duration = Duration::from_secs(30);
/// 删对端时顺手请它也删掉本节点，等不到就算了。
const UNPAIR_TIMEOUT: Duration = Duration::from_secs(10);

fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|elapsed| elapsed.as_millis() as i64)
        .unwrap_or(0)
}

fn failed(message: impl Into<String>) -> ControlError {
    ControlError::new(ControlErrorCode::OperationFailed, message)
}

fn unavailable(message: impl Into<String>) -> ControlError {
    ControlError::new(ControlErrorCode::TemporarilyUnavailable, message)
}

fn store() -> Result<smelt_store::Store, ControlError> {
    smelt_core::sqlite_state::default_sqlite_store().map_err(failed)
}

fn parse_relay(relay: Option<&str>) -> Result<Option<smelt_iroh::RelaySettings>, String> {
    match relay.map(str::trim).unwrap_or_default() {
        "" => Ok(None),
        relay => smelt_iroh::RelaySettings::parse(relay)
            .map(Some)
            .map_err(|error| format!("Invalid relay address: {error:#}")),
    }
}

fn wire(record: PeerNodeRecord) -> PeerNode {
    PeerNode {
        runtime_id: record.runtime_id,
        endpoint_id: record.endpoint_id,
        name: record.name,
        relay_url: record.relay_url,
        os: record.os,
        agent_access: record.agent_access,
        added_at_ms: record.added_at_ms,
    }
}

/// 本节点自报的身份。
fn self_info() -> Result<NodeInfo, ControlError> {
    let runtime_id = crate::runtime_identity::runtime_id()
        .ok_or_else(|| unavailable("This runtime has no identity yet"))?;
    let relay = smelt_core::remote_config::load().iroh_relay;
    let relay = relay.trim();
    Ok(NodeInfo {
        runtime_id: runtime_id.to_string(),
        name: smelt_sys::hostname().unwrap_or_else(|| "Smelt".to_string()),
        relay_url: (!relay.is_empty()).then(|| relay.to_string()),
        os: Some(std::env::consts::OS.to_string()),
    })
}

/// 往外拨用的客户端：远程访问在跑就借它的 endpoint，正在绑定就等它绑好；关着（或绑不上）
/// 才单独绑一个只拨出的。阻塞，不能在 tokio 运行时里调。
fn dialer() -> Result<Arc<IrohClient>, String> {
    if let Some(state) = crate::remote::lifecycle_remote() {
        use smelt_core::remote_config::RemoteAccessState;
        let mut status = crate::remote::remote_status(state);
        if status == RemoteAccessState::Starting {
            status = crate::remote::wait_remote_status_change(state, &status, BIND_WAIT);
        }
        if let RemoteAccessState::Running { .. } = status
            && let Some(dialer) = state
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .endpoint
                .as_ref()
                .and_then(|endpoint| endpoint.dialer.clone())
        {
            return Ok(dialer);
        }
    }
    let mut slot = DIAL_ONLY.lock().unwrap_or_else(|e| e.into_inner());
    if let Some(client) = slot.as_ref() {
        return Ok(Arc::clone(client));
    }
    let secret = smelt_iroh::default_secret_path()
        .and_then(|path| smelt_iroh::load_or_create_secret(&path))
        .map_err(|error| format!("The iroh key isn't available: {error:#}"))?;
    let relay = parse_relay(Some(&smelt_core::remote_config::load().iroh_relay))?;
    // 轮询例外：iroh 的 relay 心跳、QUIC 保活靠定时发包维持可达，没有事件可替；周期约
    // 15 秒 / 5 秒，上限是这个 endpoint 的寿命：远程访问的 endpoint 绑上、`peer.reset`、
    // 守护退出时关掉。和桌面原来的客户端 endpoint 一样，第一次往外拨时才绑。
    let client = IrohClient::new(secret, relay.as_ref()).map_err(|error| format!("{error:#}"))?;
    *slot = Some(Arc::clone(&client));
    Ok(client)
}

/// 到这个对端的连接器。阻塞。
fn connector(peer: &PeerNodeRecord) -> Result<Arc<RuntimeConnector>, String> {
    let dialer = dialer()?;
    let mut connectors = CONNECTORS.lock().unwrap_or_else(|e| e.into_inner());
    let connectors = connectors.get_or_insert_with(HashMap::new);
    if let Some(connector) = connectors.get(&peer.runtime_id)
        && connector.uses(&dialer)
    {
        return Ok(Arc::clone(connector));
    }
    let endpoint: smelt_iroh::EndpointId = peer
        .endpoint_id
        .parse()
        .map_err(|_| format!("Invalid endpoint id `{}`", peer.endpoint_id))?;
    let relay = parse_relay(peer.relay_url.as_deref())?;
    let connector = Arc::new(dialer.connector(endpoint, relay.as_ref()));
    connectors.insert(peer.runtime_id.clone(), Arc::clone(&connector));
    Ok(connector)
}

/// 在后台关掉放下的连接器和 endpoint：关连接要等 iroh 收尾，不能占着调用方的锁。
fn close_later(connectors: Vec<Arc<RuntimeConnector>>, dial_only: Option<Arc<IrohClient>>) {
    if connectors.is_empty() && dial_only.is_none() {
        return;
    }
    std::thread::spawn(move || {
        for connector in connectors {
            connector.close();
        }
        drop(dial_only);
    });
}

/// 丢掉一个对端的连接器：它的地址、中继变了，或者删掉了。
fn forget(runtime_id: &str) {
    let removed = CONNECTORS
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .as_mut()
        .and_then(|connectors| connectors.remove(runtime_id));
    close_later(removed.into_iter().collect(), None);
}

/// 远程访问的 endpoint 绑上、关掉之后调：丢掉按旧 endpoint 拨的连接器；绑上了的话只拨
/// 出的那个也不要了（同一把密钥只留一个 endpoint）。
pub(crate) fn endpoint_changed() {
    let connectors: Vec<_> = CONNECTORS
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .take()
        .map(|connectors| connectors.into_values().collect())
        .unwrap_or_default();
    let dial_only = DIAL_ONLY.lock().unwrap_or_else(|e| e.into_inner()).take();
    close_later(connectors, dial_only);
}

/// 远程访问要绑 endpoint 之前调：当场关掉只拨出的 endpoint 和按它拨的连接，再绑同一把
/// 密钥的新 endpoint。两个同时在线时，对端可能连到正在关的旧 endpoint 上被拒。阻塞，只在
/// 绑定的后台线程里调。
pub(crate) fn release_dial_only() {
    let Some(dial_only) = DIAL_ONLY.lock().unwrap_or_else(|e| e.into_inner()).take() else {
        return;
    };
    let connectors: Vec<_> = CONNECTORS
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .take()
        .map(|connectors| connectors.into_values().collect())
        .unwrap_or_default();
    for connector in connectors {
        connector.close();
    }
    drop(dial_only);
}

/// 连入的 endpoint 是哪个对端。
pub(crate) fn peer_by_endpoint(endpoint_id: &str) -> Result<Option<String>, String> {
    smelt_core::sqlite_state::default_sqlite_store()
        .and_then(|store| {
            store
                .peer_node_by_endpoint(endpoint_id)
                .map_err(|error| error.to_string())
        })
        .map(|peer| peer.map(|peer| peer.runtime_id))
}

/// 调用方是不是某个智能体会话（或它的子孙进程）。认不出是谁时按是处理。
fn caller_is_agent(conn: &impl crate::protocol::PeerSource, acp_sessions: &AcpSessions) -> bool {
    match conn.peer_identity() {
        Ok(identity) => {
            crate::memory_peer_auth::deny_agent_session_peer(acp_sessions, identity.pid).is_err()
        }
        Err(_) => true,
    }
}

/// `peer.*`、`node.*` 控制方法。`caller_is_agent` 只在本机调用时有意义；对端表改了调
/// `changed`（推 `peers.changed`）。
pub(crate) fn dispatch(
    method: ControlMethod,
    params: serde_json::Value,
    principal: &Principal,
    caller_is_agent: impl FnOnce() -> bool,
    changed: impl FnOnce(),
) -> Result<serde_json::Value, ControlError> {
    let writes = matches!(
        method,
        ControlMethod::NodePair
            | ControlMethod::NodeUnpair
            | ControlMethod::PeerPair
            | ControlMethod::PeerUpdate
            | ControlMethod::PeerRemove
    );
    let result = dispatch_inner(method, params, principal, caller_is_agent);
    if writes && result.is_ok() {
        changed();
    }
    result
}

fn dispatch_inner(
    method: ControlMethod,
    params: serde_json::Value,
    principal: &Principal,
    caller_is_agent: impl FnOnce() -> bool,
) -> Result<serde_json::Value, ControlError> {
    match method {
        ControlMethod::NodePair => {
            let params = decode_params::<<NodePair as ControlCall>::Params>(params)?;
            encode(accept_pair(principal, params)?)
        }
        ControlMethod::NodeUnpair => {
            decode_params::<EmptyParams>(params)?;
            accept_unpair(principal)?;
            encode(EmptyParams::default())
        }
        local => {
            if principal.is_remote() {
                return Err(ControlError::new(
                    ControlErrorCode::PermissionDenied,
                    "Peers are only managed from this computer",
                ));
            }
            let writes = matches!(
                local,
                ControlMethod::PeerPair | ControlMethod::PeerUpdate | ControlMethod::PeerRemove
            );
            if writes && caller_is_agent() {
                return Err(ControlError::new(
                    ControlErrorCode::PermissionDenied,
                    "Agent sessions can't change paired machines",
                ));
            }
            match local {
                ControlMethod::PeerList => {
                    decode_params::<EmptyParams>(params)?;
                    encode(list()?)
                }
                ControlMethod::PeerPair => {
                    let params = decode_params::<<PeerPair as ControlCall>::Params>(params)?;
                    encode(pair(params)?)
                }
                ControlMethod::PeerUpdate => {
                    let params = decode_params::<<PeerUpdate as ControlCall>::Params>(params)?;
                    encode(update(params)?)
                }
                ControlMethod::PeerRemove => {
                    let params = decode_params::<PeerIdParams>(params)?;
                    remove(&params.runtime_id)?;
                    encode(EmptyParams::default())
                }
                ControlMethod::PeerLink => {
                    let params = decode_params::<PeerIdParams>(params)?;
                    encode(link(&params.runtime_id))
                }
                ControlMethod::PeerReset => {
                    decode_params::<EmptyParams>(params)?;
                    endpoint_changed();
                    encode(EmptyParams::default())
                }
                other => Err(ControlError::new(
                    ControlErrorCode::Internal,
                    format!("`{other}` is not a peer method"),
                )),
            }
        }
    }
}

fn encode(value: impl serde::Serialize) -> Result<serde_json::Value, ControlError> {
    serde_json::to_value(value)
        .map_err(|error| ControlError::new(ControlErrorCode::Internal, error.to_string()))
}

fn list() -> Result<PeerList, ControlError> {
    let peers = store()?
        .list_peer_nodes()
        .map_err(|error| failed(error.to_string()))?;
    Ok(PeerList {
        peers: peers.into_iter().map(wire).collect(),
    })
}

fn peer(runtime_id: &str) -> Result<PeerNodeRecord, ControlError> {
    store()?
        .peer_node(runtime_id)
        .map_err(|error| failed(error.to_string()))?
        .ok_or_else(|| {
            ControlError::new(
                ControlErrorCode::NotFound,
                format!("No paired machine `{runtime_id}`"),
            )
        })
}

/// 显示名：去掉首尾空白，最多 64 个字符；空的用 `fallback`。
fn display_name(name: &str, fallback: &str) -> String {
    match name.trim() {
        "" => fallback.to_string(),
        name => name.chars().take(64).collect(),
    }
}

/// 用对方的配对串双向配对：经本节点的 endpoint 连过去交出自己的身份，对方记下本节点后
/// 交回它的身份，这边再记下它。
fn pair(params: PeerPairParams) -> Result<PeerNode, ControlError> {
    let invalid = |message: String| ControlError::new(ControlErrorCode::InvalidParams, message);
    let pairing = smelt_pairing::parse_pairing_uri(&params.pairing_uri)
        .map_err(|error| invalid(error.to_string()))?;
    let target: smelt_iroh::EndpointId = pairing
        .endpoint_id
        .parse()
        .map_err(|_| invalid(format!("Invalid endpoint id `{}`", pairing.endpoint_id)))?;
    let relay = parse_relay(pairing.relay_url.as_deref()).map_err(invalid)?;
    let ours = self_info()?;
    let dialer = dialer().map_err(unavailable)?;
    if dialer.id() == target {
        return Err(invalid("This is the pairing link of this machine".into()));
    }
    let connector = Arc::new(dialer.connector(target, relay.as_ref()));
    let theirs = {
        let connector = Arc::clone(&connector);
        RuntimeClient::new(move || smelt_runtime_client::Connector::connect(connector.as_ref()))
            .call::<NodePair>(
                &NodePairParams {
                    code: pairing.code,
                    node: ours.clone(),
                },
                PAIR_TIMEOUT,
            )
            .map_err(|error| match error {
                smelt_runtime_client::protocol::control::ControlClientError::Protocol(error) => {
                    error
                }
                other => unavailable(other.to_string()),
            })?
    };
    if theirs.runtime_id.is_empty() || theirs.runtime_id == ours.runtime_id {
        connector.close();
        return Err(invalid(
            "The other machine reported an invalid identity".into(),
        ));
    }
    let store = store()?;
    let existing = store
        .peer_node(&theirs.runtime_id)
        .map_err(|error| failed(error.to_string()))?;
    let record = PeerNodeRecord {
        runtime_id: theirs.runtime_id.clone(),
        endpoint_id: pairing.endpoint_id,
        name: existing
            .as_ref()
            .map(|peer| peer.name.clone())
            .unwrap_or_else(|| display_name(&theirs.name, &theirs.runtime_id)),
        relay_url: pairing.relay_url,
        os: theirs.os,
        agent_access: existing.as_ref().is_some_and(|peer| peer.agent_access),
        added_at_ms: existing
            .as_ref()
            .map_or_else(now_ms, |peer| peer.added_at_ms),
    };
    store
        .put_peer_node(&record)
        .map_err(|error| failed(error.to_string()))?;
    // 刚拨通的连接留着接着用。
    let replaced = CONNECTORS
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .get_or_insert_with(HashMap::new)
        .insert(record.runtime_id.clone(), connector);
    close_later(replaced.into_iter().collect(), None);
    crate::dlog(&format!(
        "已和机器配对：{}（{}）",
        record.name, record.runtime_id
    ));
    Ok(wire(record))
}

/// 另一个节点拿本节点发的配对码来配对。它的 endpoint id 取自连接，不信参数。
fn accept_pair(principal: &Principal, params: NodePairParams) -> Result<NodeInfo, ControlError> {
    let endpoint_id = match principal {
        Principal::Device(id) | Principal::Unpaired(id) => id.clone(),
        Principal::Peer { endpoint_id, .. } => endpoint_id.clone(),
        Principal::Local => {
            return Err(ControlError::because(
                ControlErrorCode::FailedPrecondition,
                ControlReason::PairFromRemoteOnly,
            ));
        }
    };
    let ours = self_info()?;
    let node = params.node;
    if node.runtime_id.trim().is_empty() || node.runtime_id == ours.runtime_id {
        return Err(ControlError::new(
            ControlErrorCode::InvalidParams,
            "Invalid runtime id",
        ));
    }
    if !crate::devices::redeem_code(&params.code) {
        return Err(ControlError::because(
            ControlErrorCode::PermissionDenied,
            ControlReason::PairCodeInvalid,
        ));
    }
    let store = store()?;
    let existing = store
        .peer_node(&node.runtime_id)
        .map_err(|error| failed(error.to_string()))?;
    let record = PeerNodeRecord {
        runtime_id: node.runtime_id.clone(),
        endpoint_id,
        name: existing
            .as_ref()
            .map(|peer| peer.name.clone())
            .unwrap_or_else(|| display_name(&node.name, &node.runtime_id)),
        relay_url: node
            .relay_url
            .filter(|relay| parse_relay(Some(relay)).is_ok()),
        os: node.os,
        agent_access: existing.as_ref().is_some_and(|peer| peer.agent_access),
        added_at_ms: existing
            .as_ref()
            .map_or_else(now_ms, |peer| peer.added_at_ms),
    };
    store
        .put_peer_node(&record)
        .map_err(|error| failed(error.to_string()))?;
    forget(&record.runtime_id);
    crate::dlog(&format!(
        "机器已配对：{}（{}）",
        record.name, record.runtime_id
    ));
    Ok(ours)
}

/// 对端请本节点删掉它。
fn accept_unpair(principal: &Principal) -> Result<(), ControlError> {
    let Principal::Peer {
        runtime_id,
        endpoint_id,
    } = principal
    else {
        return Err(ControlError::new(
            ControlErrorCode::PermissionDenied,
            "Only a paired machine can unpair itself",
        ));
    };
    store()?
        .delete_peer_node(runtime_id)
        .map_err(|error| failed(error.to_string()))?;
    forget(runtime_id);
    close_inbound(endpoint_id);
    crate::dlog(&format!("机器取消了配对：{runtime_id}"));
    Ok(())
}

/// 断开这个 endpoint 连进来的连接：删完记录再断，重连时已经不认它了。
fn close_inbound(endpoint_id: &str) {
    if let Some(state) = crate::remote::lifecycle_remote()
        && let Some(endpoint) = state
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .endpoint
            .as_ref()
    {
        endpoint.registry.close(endpoint_id);
    }
}

fn remove(runtime_id: &str) -> Result<(), ControlError> {
    let record = peer(runtime_id)?;
    store()?
        .delete_peer_node(runtime_id)
        .map_err(|error| failed(error.to_string()))?;
    close_inbound(&record.endpoint_id);
    // 顺手请对方也删掉本节点，不让删除等着它：连不上就算了，对方那边留着的记录在那边删。
    std::thread::spawn(move || {
        if let Ok(connector) = connector(&record) {
            let _ = RuntimeClient::new(move || {
                smelt_runtime_client::Connector::connect(connector.as_ref())
            })
            .call::<NodeUnpair>(&EmptyParams::default(), UNPAIR_TIMEOUT);
        }
        forget(&record.runtime_id);
    });
    crate::dlog(&format!("已删除机器：{runtime_id}"));
    Ok(())
}

fn update(params: PeerUpdateParams) -> Result<PeerNode, ControlError> {
    let mut record = peer(&params.runtime_id)?;
    let mut redial = false;
    if let Some(name) = params.name {
        record.name = display_name(&name, &record.name);
    }
    if let Some(relay) = params.relay_url {
        let relay = relay.trim();
        parse_relay(Some(relay))
            .map_err(|error| ControlError::new(ControlErrorCode::InvalidParams, error))?;
        let relay = (!relay.is_empty()).then(|| relay.to_string());
        redial = relay != record.relay_url;
        record.relay_url = relay;
    }
    if let Some(os) = params.os {
        record.os = (!os.is_empty()).then_some(os);
    }
    if let Some(agent_access) = params.agent_access {
        record.agent_access = agent_access;
    }
    store()?
        .put_peer_node(&record)
        .map_err(|error| failed(error.to_string()))?;
    if redial {
        forget(&record.runtime_id);
    }
    Ok(wire(record))
}

fn cached_connector(runtime_id: &str) -> Option<Arc<RuntimeConnector>> {
    CONNECTORS
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .as_ref()
        .and_then(|connectors| connectors.get(runtime_id).cloned())
}

/// 眼下的连接路径。还没拨过号就没有，也不为此去拨。
fn link(runtime_id: &str) -> PeerLinkResult {
    link_of(cached_connector(runtime_id).as_deref())
}

fn link_of(connector: Option<&RuntimeConnector>) -> PeerLinkResult {
    let link = connector
        .and_then(|connector| connector.path_status())
        .map(|path| PeerLink {
            route: match path.kind {
                smelt_iroh::PathKind::Direct => PeerRoute::Direct,
                smelt_iroh::PathKind::Relay => PeerRoute::Relay {
                    address: path.address,
                },
                smelt_iroh::PathKind::Custom => PeerRoute::Other,
            },
            rtt_ms: u32::try_from(path.rtt.as_millis()).unwrap_or(u32::MAX),
        });
    PeerLinkResult { link }
}

#[derive(serde::Deserialize)]
struct ForwardTarget {
    runtime_id: String,
}

fn reply(conn: &mut impl std::io::Write, reply: &ForwardReply) {
    let _ = smelt_runtime_client::protocol::write_json_line(conn, reply);
}

/// `op=forward`：拨通对端后，这条连接和到对端的流之间只搬字节，直到两边都结束。
pub(crate) fn handle_forward<C: ClientSocket + crate::protocol::PeerSource>(
    mut conn: C,
    reader: BufReader<C>,
    payload: &serde_json::Value,
    principal: &Principal,
    acp_sessions: &AcpSessions,
) {
    if principal.is_remote() {
        reply(
            &mut conn,
            &ForwardReply::failed(
                ForwardErrorCode::PermissionDenied,
                "Only this computer can forward through its runtime",
            ),
        );
        return;
    }
    let Ok(target) = serde_json::from_value::<ForwardTarget>(payload.clone()) else {
        reply(
            &mut conn,
            &ForwardReply::failed(ForwardErrorCode::Unknown, "Missing runtime_id"),
        );
        return;
    };
    let record = match store().and_then(|store| {
        store
            .peer_node(&target.runtime_id)
            .map_err(|error| failed(error.to_string()))
    }) {
        Ok(Some(record)) => record,
        Ok(None) => {
            reply(
                &mut conn,
                &ForwardReply::failed(
                    ForwardErrorCode::NotFound,
                    format!("No paired machine `{}`", target.runtime_id),
                ),
            );
            return;
        }
        Err(error) => {
            reply(
                &mut conn,
                &ForwardReply::failed(ForwardErrorCode::Unknown, error.message),
            );
            return;
        }
    };
    if !record.agent_access && caller_is_agent(&conn, acp_sessions) {
        reply(
            &mut conn,
            &ForwardReply::failed(
                ForwardErrorCode::PermissionDenied,
                format!(
                    "Agents on this computer may not use `{}`. Allow it in Smelt's machine settings.",
                    record.name
                ),
            ),
        );
        return;
    }
    let remote = match connector(&record).and_then(|connector| {
        smelt_runtime_client::Connector::connect(connector.as_ref()).map_err(|e| e.to_string())
    }) {
        Ok(remote) => remote,
        Err(error) => {
            reply(
                &mut conn,
                &ForwardReply::failed(
                    ForwardErrorCode::Unreachable,
                    format!("Couldn't reach `{}`: {error}", record.name),
                ),
            );
            return;
        }
    };
    if smelt_runtime_client::protocol::write_json_line(&mut conn, &ForwardReply::ok()).is_err() {
        return;
    }
    splice(conn, reader, remote);
}

/// `op=peer_link_watch`：先回眼下的路径，iroh 换了路径再回，连接断了回一行没有路径的
/// 就结束；调用方断开也结束。只盯已经拨好的连接，不为此拨号：还没拨过就回一行没有
/// 路径的结束。
pub(crate) fn handle_link_watch<C: ClientSocket>(
    mut conn: C,
    payload: &serde_json::Value,
    principal: &Principal,
) {
    if principal.is_remote() {
        return;
    }
    let Ok(target) = serde_json::from_value::<ForwardTarget>(payload.clone()) else {
        return;
    };
    let write = |conn: &mut C, result: &PeerLinkResult| {
        smelt_runtime_client::protocol::write_json_line(conn, result).is_ok()
    };
    let Some(connector) = cached_connector(&target.runtime_id) else {
        let _ = write(&mut conn, &PeerLinkResult::default());
        return;
    };
    // `None`：调用方断开了。
    let (changes, received) = std::sync::mpsc::channel::<Option<smelt_iroh::runtime::LinkChange>>();
    let on_change = changes.clone();
    let _watch = connector.watch(move |change| {
        let _ = on_change.send(Some(change));
    });
    if let Ok(mut reader) = conn.try_clone() {
        std::thread::spawn(move || {
            let mut buffer = [0u8; 64];
            while matches!(std::io::Read::read(&mut reader, &mut buffer), Ok(read) if read > 0) {}
            let _ = changes.send(None);
        });
    }
    if !write(&mut conn, &link_of(Some(&connector))) {
        return;
    }
    while let Ok(Some(change)) = received.recv() {
        let closed = change == smelt_iroh::runtime::LinkChange::Closed;
        let result = if closed {
            PeerLinkResult::default()
        } else {
            link_of(Some(&connector))
        };
        if !write(&mut conn, &result) || closed {
            break;
        }
    }
    let _ = conn.shutdown(Shutdown::Both);
}

/// 两个方向各搬到对端写完：一边写完只关另一边的写方向，让还在回话的一方说完；回话的
/// 方向（对端到本机）结束时两边整个关掉，叫醒还在等读的另一条线程。
fn splice<C: ClientSocket>(
    conn: C,
    mut reader: BufReader<C>,
    remote: smelt_runtime_client::RuntimeStream,
) {
    let (Ok(remote_writer), Ok(mut conn_writer)) = (remote.try_clone(), conn.try_clone()) else {
        return;
    };
    // `reader` 里可能还有首帧之后预读的字节，`io::copy` 先交出它们。
    let up = std::thread::spawn(move || {
        let mut writer = &remote_writer;
        let _ = std::io::copy(&mut reader, &mut writer);
        let _ = remote_writer.shutdown(Shutdown::Write);
    });
    let _ = std::io::copy(&mut &remote, &mut conn_writer);
    let _ = conn_writer.flush();
    let _ = conn.shutdown(Shutdown::Both);
    let _ = remote.shutdown(Shutdown::Both);
    let _ = up.join();
}

#[cfg(test)]
mod tests {
    use super::*;
    use smelt_runtime_client::RuntimeStream;
    use smelt_runtime_client::protocol::forward::{ForwardRequest, read_reply};
    use std::io::{BufRead as _, Read as _, Write as _};

    fn enable_store() {
        // 测试进程的 `~/.smelt` 是它自己的临时沙箱。
        smelt_core::sqlite_state::enable_sqlite_state();
    }

    fn call(
        method: ControlMethod,
        params: serde_json::Value,
        principal: &Principal,
        agent: bool,
    ) -> Result<serde_json::Value, ControlError> {
        dispatch(method, params, principal, || agent, || {})
    }

    fn node(runtime_id: &str) -> serde_json::Value {
        serde_json::json!({"runtime_id": runtime_id, "name": "  far box  ", "os": "linux"})
    }

    /// 另一个节点拿本节点发的码配对：按连接的 endpoint 记下它，交回本节点的身份；
    /// 码只能用一次，之后它按对端节点连进来。
    #[test]
    fn a_node_pairs_with_a_code_and_is_then_known_by_its_endpoint() {
        enable_store();
        let (code, _) = crate::devices::issue_code();
        let stranger = Principal::Unpaired("ep-pairs-with-code".into());
        let ours: NodeInfo = serde_json::from_value(
            call(
                ControlMethod::NodePair,
                serde_json::json!({"code": code, "node": node("rt-pairs-with-code")}),
                &stranger,
                false,
            )
            .unwrap(),
        )
        .unwrap();
        assert_eq!(
            Some(ours.runtime_id.as_str()),
            crate::runtime_identity::runtime_id().as_deref()
        );
        assert_eq!(
            peer_by_endpoint("ep-pairs-with-code").unwrap().as_deref(),
            Some("rt-pairs-with-code")
        );
        let record = peer("rt-pairs-with-code").unwrap();
        assert_eq!(record.name, "far box");
        assert!(!record.agent_access, "新配对的机器默认不给智能体用");

        let reused = call(
            ControlMethod::NodePair,
            serde_json::json!({"code": code, "node": node("rt-pairs-again")}),
            &Principal::Unpaired("ep-other".into()),
            false,
        )
        .unwrap_err();
        assert_eq!(reused.code, ControlErrorCode::PermissionDenied);
    }

    #[test]
    fn a_node_cannot_claim_to_be_this_runtime() {
        enable_store();
        let (code, _) = crate::devices::issue_code();
        let own = crate::runtime_identity::runtime_id().unwrap().to_string();
        let error = call(
            ControlMethod::NodePair,
            serde_json::json!({"code": code, "node": node(&own)}),
            &Principal::Unpaired("ep-impostor".into()),
            false,
        )
        .unwrap_err();
        assert_eq!(error.code, ControlErrorCode::InvalidParams);
    }

    /// 对端请求取消配对只删它自己；别的调用方不能删。
    #[test]
    fn only_the_peer_itself_can_unpair() {
        enable_store();
        let record = PeerNodeRecord {
            runtime_id: "rt-unpair".into(),
            endpoint_id: "ep-unpair".into(),
            name: "box".into(),
            relay_url: None,
            os: None,
            agent_access: false,
            added_at_ms: 1,
        };
        store().unwrap().put_peer_node(&record).unwrap();
        let device = Principal::Device("ep-unpair".into());
        assert!(
            call(
                ControlMethod::NodeUnpair,
                serde_json::json!({}),
                &device,
                false
            )
            .is_err()
        );
        let peer_principal = Principal::Peer {
            runtime_id: "rt-unpair".into(),
            endpoint_id: "ep-unpair".into(),
        };
        call(
            ControlMethod::NodeUnpair,
            serde_json::json!({}),
            &peer_principal,
            false,
        )
        .unwrap();
        assert!(peer("rt-unpair").is_err());
    }

    /// 对端设置只在本机改，智能体会话改不了（包括给自己开权限）。
    #[test]
    fn agents_and_remote_callers_cannot_change_peers() {
        enable_store();
        let record = PeerNodeRecord {
            runtime_id: "rt-guarded".into(),
            endpoint_id: "ep-guarded".into(),
            name: "box".into(),
            relay_url: None,
            os: None,
            agent_access: false,
            added_at_ms: 1,
        };
        store().unwrap().put_peer_node(&record).unwrap();
        let grant = serde_json::json!({"runtime_id": "rt-guarded", "agent_access": true});
        let error = call(
            ControlMethod::PeerUpdate,
            grant.clone(),
            &Principal::Local,
            true,
        )
        .unwrap_err();
        assert_eq!(error.code, ControlErrorCode::PermissionDenied);
        let remote = Principal::Device("phone".into());
        assert!(
            call(
                ControlMethod::PeerList,
                serde_json::json!({}),
                &remote,
                false
            )
            .is_err()
        );
        assert!(
            call(
                ControlMethod::PeerList,
                serde_json::json!({}),
                &Principal::Local,
                true
            )
            .is_ok(),
            "智能体可以看有哪些机器"
        );
        let updated: PeerNode = serde_json::from_value(
            call(ControlMethod::PeerUpdate, grant, &Principal::Local, false).unwrap(),
        )
        .unwrap();
        assert!(updated.agent_access);
        let bad_relay = serde_json::json!({"runtime_id": "rt-guarded", "relay_url": "not a url"});
        assert_eq!(
            call(
                ControlMethod::PeerUpdate,
                bad_relay,
                &Principal::Local,
                false
            )
            .unwrap_err()
            .code,
            ControlErrorCode::InvalidParams
        );
    }

    fn forward_as(principal: Principal, runtime_id: &str) -> ForwardReply {
        let (mut client, server_end) = RuntimeStream::pair().unwrap();
        let acp_sessions = crate::acp_host::new_test_acp_sessions();
        std::thread::spawn(move || {
            let reader = BufReader::new(server_end.try_clone().unwrap());
            let mut reader = reader;
            let mut line = String::new();
            reader.read_line(&mut line).unwrap();
            let payload: serde_json::Value = serde_json::from_str(&line).unwrap();
            handle_forward(server_end, reader, &payload, &principal, &acp_sessions);
        });
        smelt_runtime_client::protocol::write_json_line(
            &mut client,
            &ForwardRequest::new(runtime_id),
        )
        .unwrap();
        read_reply(&mut client).unwrap()
    }

    /// 转发只收本机，不认识的机器直接回不认识，不去拨号。
    #[test]
    fn forward_is_local_only_and_needs_a_known_peer() {
        enable_store();
        let reply = forward_as(Principal::Device("phone".into()), "rt-any");
        assert_eq!(reply.code, Some(ForwardErrorCode::PermissionDenied));
        let peer_principal = Principal::Peer {
            runtime_id: "rt-a".into(),
            endpoint_id: "ep-a".into(),
        };
        let reply = forward_as(peer_principal, "rt-any");
        assert_eq!(
            reply.code,
            Some(ForwardErrorCode::PermissionDenied),
            "对端不能经本节点再转一跳"
        );
        let reply = forward_as(Principal::Local, "rt-never-paired");
        assert_eq!(reply.code, Some(ForwardErrorCode::NotFound));
    }

    /// 两个方向都原样搬，首帧之后预读的字节也先转过去；对端说完关掉，本机读到 EOF。
    #[test]
    fn splice_moves_bytes_both_ways_including_read_ahead() {
        let (mut client, conn) = RuntimeStream::pair().unwrap();
        let (remote, mut far) = RuntimeStream::pair().unwrap();
        client.write_all(b"early").unwrap();
        let mut reader = BufReader::new(conn.try_clone().unwrap());
        assert_eq!(reader.fill_buf().unwrap(), b"early", "模拟首帧之后的预读");
        let spliced = std::thread::spawn(move || splice(conn, reader, remote));

        let mut got = [0u8; 5];
        far.read_exact(&mut got).unwrap();
        assert_eq!(&got, b"early");
        client.write_all(b" ping").unwrap();
        far.read_exact(&mut got).unwrap();
        assert_eq!(&got, b" ping");

        far.write_all(b"pong").unwrap();
        far.shutdown(Shutdown::Both).unwrap();
        let mut answer = Vec::new();
        client.read_to_end(&mut answer).unwrap();
        assert_eq!(answer, b"pong");
        spliced.join().unwrap();
    }
}
