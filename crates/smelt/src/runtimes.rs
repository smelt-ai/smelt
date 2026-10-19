//! 桌面挂着的运行时（设计文档 D6）。本机运行时总在；远程运行时来自客户端库里登记的
//! 连接。会话、视图各自记着自己属于哪个运行时，调用、连接都发给它。

use smelt_runtime_client::RuntimeClient;

pub(crate) use smelt_client::runtime::{Runtime, RuntimeKey};

/// 本机运行时的客户端。连本机 socket；守护的拉起和升级另由 `terminal` 管。
pub(crate) fn local() -> RuntimeClient {
    Runtime::local().client().clone()
}

/// 给终端开一条连接。本机守护没在跑就先拉起来，这只有本机做得到；远程运行时连不上
/// 就是连不上，由调用方按断线处理。
pub(crate) fn connect_terminal(
    runtime: &Runtime,
) -> std::io::Result<std::os::unix::net::UnixStream> {
    if runtime.is_local() {
        crate::terminal::connect_daemon()
    } else {
        runtime.client().connect()
    }
}

/// 给人看的运行时名字：本机就叫本机，远程用配对时记下的主机名。
pub(crate) fn runtime_name(key: &RuntimeKey, cx: &gpui::App) -> String {
    match key {
        RuntimeKey::Local => "本机".to_string(),
        RuntimeKey::Remote(id) => cx
            .try_global::<RuntimeHub>()
            .and_then(|hub| {
                hub.remotes
                    .iter()
                    .find(|remote| remote.runtime.key() == key)
            })
            .map(|remote| remote.connection.name.clone())
            .unwrap_or_else(|| id.clone()),
    }
}

/// 有没有配对过远程运行时。没有时一切都在本机，界面上不必标「只作用于本机」。
pub(crate) fn has_remote_runtimes(cx: &gpui::App) -> bool {
    cx.try_global::<RuntimeHub>()
        .is_some_and(|hub| !hub.remotes.is_empty())
}

/// 设置、能力库里只管本机运行时的页面挂的标签：有远程运行时以后，这些页面改的东西
/// 对远程不生效，得说清楚。
pub(crate) fn local_runtime_scope_chip() -> gpui_component::tag::Tag {
    use gpui::{ParentElement as _, Styled as _};
    gpui_component::tag::Tag::secondary()
        .outline()
        .rounded_full()
        .flex_none()
        .child("只作用于本机运行时")
}

/// 远程运行时不能用时侧栏标的字（按最近一次探测），能用就是 `None`。本机总能用。
pub(crate) fn runtime_unavailable_label(key: &RuntimeKey, cx: &gpui::App) -> Option<&'static str> {
    match key {
        RuntimeKey::Local => None,
        RuntimeKey::Remote(_) => match cx
            .try_global::<RuntimeHub>()
            .and_then(|hub| {
                hub.remotes
                    .iter()
                    .find(|remote| remote.runtime.key() == key)
            })
            .map(|remote| &remote.status)
        {
            Some(RuntimeStatus::Online { .. }) => None,
            Some(RuntimeStatus::Incompatible { .. }) => Some("版本不兼容"),
            Some(RuntimeStatus::Upgrading(_)) => Some("升级中"),
            Some(RuntimeStatus::Connecting | RuntimeStatus::Offline(_)) | None => Some("离线"),
        },
    }
}

fn remote_runtime(runtime_id: &str, client: RuntimeClient) -> Runtime {
    Runtime::new(RuntimeKey::Remote(runtime_id.to_string()), client)
}

/// 远程运行时现在连不连得上。
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum RuntimeStatus {
    Connecting,
    Online {
        version: String,
        platform: Option<String>,
        /// 装了远程桌面，侧栏和机器面板才给「远程桌面」。
        desktop: bool,
    },
    Offline(String),
    /// 连得上，但版本和这个桌面对不上（见 `smelt_client::compat`），说明里写着该升级
    /// 哪一边。这时不订阅它的事件：订阅了也解不出来，只会断开重连。
    Incompatible {
        detail: String,
        /// 是运行时旧：可以请它升到这个桌面的版本（见 [`push_upgrade`]）。
        runtime_outdated: bool,
    },
    /// 正在请它升到这个桌面的版本，带着给人看的进展。
    Upgrading(String),
}

/// 一个登记过的远程运行时。
#[derive(Clone)]
pub(crate) struct RemoteRuntime {
    pub(crate) connection: smelt_client_store::RuntimeConnection,
    pub(crate) runtime: Runtime,
    pub(crate) status: RuntimeStatus,
    /// 读它眼下连接路径的把手，和它的连接器共用。
    probe: PathProbe,
    /// 最近一次读到的连接路径和延迟。只在连着时有。
    pub(crate) link: Option<RuntimeLink>,
}

/// 到一个远程运行时的连接走哪条路。
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum LinkKind {
    /// 两边打通了，点对点直连。
    Direct,
    /// 打洞没成，经中继转发。带着给人看的中继名。
    Relay(String),
    /// 别的传输（iroh 的自定义传输），不多说。
    Other,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct RuntimeLink {
    pub(crate) kind: LinkKind,
    pub(crate) rtt_ms: u32,
}

impl RuntimeLink {
    fn from_path(path: &smelt_iroh::PathStatus) -> Self {
        Self {
            kind: match path.kind {
                smelt_iroh::PathKind::Direct => LinkKind::Direct,
                smelt_iroh::PathKind::Relay => LinkKind::Relay(relay_name(&path.address)),
                smelt_iroh::PathKind::Custom => LinkKind::Other,
            },
            rtt_ms: u32::try_from(path.rtt.as_millis()).unwrap_or(u32::MAX),
        }
    }
}

/// 中继地址（`relay:https://usw1-1.relay.n0.iroh.link./`）里给人看的那段：n0 的公共中继
/// 只留地区（`n0 usw1-1`），自建的留主机名。
fn relay_name(address: &str) -> String {
    let url = address.strip_prefix("relay:").unwrap_or(address);
    let host = url
        .split("://")
        .nth(1)
        .unwrap_or(url)
        .split(['/', ':'])
        .next()
        .unwrap_or_default()
        .trim_end_matches('.');
    match host.strip_suffix(".relay.n0.iroh.link") {
        Some(region) => format!("n0 {region}"),
        None => host.to_string(),
    }
}

/// 延迟到了这个数，中继就值得提醒：终端敲一个字要等这么久才回显。
const SLOW_RTT_MS: u32 = 200;

/// 换一次就让看着运行时列表的界面重画一次，所以延迟小幅抖动不算变。
fn link_changed(old: Option<&RuntimeLink>, new: Option<&RuntimeLink>) -> bool {
    match (old, new) {
        (Some(old), Some(new)) => {
            old.kind != new.kind || old.rtt_ms.abs_diff(new.rtt_ms) >= (old.rtt_ms / 5).max(20)
        }
        (None, None) => false,
        _ => true,
    }
}

/// 状态文字的轻重。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum LinkTone {
    Muted,
    Warning,
}

/// 一台机器的连接状态写成字。`compact` 给侧栏标题：一切正常（直连、在线还没测出路径）
/// 时不写，只在需要注意时给一小段；否则是机器面板里的完整说明。
fn link_text(
    status: &RuntimeStatus,
    link: Option<&RuntimeLink>,
    compact: bool,
) -> Option<(String, LinkTone)> {
    match status {
        RuntimeStatus::Connecting => Some(("连接中".to_string(), LinkTone::Muted)),
        RuntimeStatus::Offline(_) => Some(("离线".to_string(), LinkTone::Muted)),
        RuntimeStatus::Incompatible { .. } => Some(("版本不兼容".to_string(), LinkTone::Warning)),
        RuntimeStatus::Upgrading(_) if compact => Some(("升级中".to_string(), LinkTone::Muted)),
        RuntimeStatus::Upgrading(progress) => Some((progress.clone(), LinkTone::Muted)),
        RuntimeStatus::Online { .. } => {
            let Some(link) = link else {
                return (!compact).then(|| ("在线".to_string(), LinkTone::Muted));
            };
            let tone = if link.rtt_ms >= SLOW_RTT_MS {
                LinkTone::Warning
            } else {
                LinkTone::Muted
            };
            match &link.kind {
                LinkKind::Relay(_) if compact => Some((format!("中继 · {}ms", link.rtt_ms), tone)),
                LinkKind::Relay(relay) => Some((format!("中继 {relay} · {}ms", link.rtt_ms), tone)),
                _ if compact => None,
                LinkKind::Direct => Some((format!("直连 · {}ms", link.rtt_ms), tone)),
                LinkKind::Other => Some((format!("在线 · {}ms", link.rtt_ms), tone)),
            }
        }
    }
}

fn remote_of<'a>(key: &RuntimeKey, cx: &'a gpui::App) -> Option<&'a RemoteRuntime> {
    cx.try_global::<RuntimeHub>()?
        .remotes
        .iter()
        .find(|remote| remote.runtime.key() == key)
}

/// 侧栏标题上的连接状态，见 [`link_text`]。本机不写。
pub(crate) fn runtime_attention(key: &RuntimeKey, cx: &gpui::App) -> Option<(String, LinkTone)> {
    let remote = remote_of(key, cx)?;
    link_text(&remote.status, remote.link.as_ref(), true)
}

/// 机器面板里的完整连接说明。
pub(crate) fn runtime_link_detail(key: &RuntimeKey, cx: &gpui::App) -> (String, LinkTone) {
    match key {
        RuntimeKey::Local => ("这台 Mac".to_string(), LinkTone::Muted),
        RuntimeKey::Remote(_) => remote_of(key, cx)
            .and_then(|remote| link_text(&remote.status, remote.link.as_ref(), false))
            .unwrap_or_else(|| ("离线".to_string(), LinkTone::Muted)),
    }
}

/// 这台机器装了远程桌面、眼下也连着。本机（macOS）没有这套。
pub(crate) fn runtime_has_desktop(key: &RuntimeKey, cx: &gpui::App) -> bool {
    remote_of(key, cx)
        .is_some_and(|remote| matches!(remote.status, RuntimeStatus::Online { desktop: true, .. }))
}

/// 有没有哪台机器离线、版本不对，或者走中继还很慢：机器按钮上挂个点提醒。
pub(crate) fn any_runtime_needs_attention(cx: &gpui::App) -> bool {
    cx.try_global::<RuntimeHub>().is_some_and(|hub| {
        hub.remotes.iter().any(|remote| {
            link_text(&remote.status, remote.link.as_ref(), true)
                .is_some_and(|(_, tone)| tone == LinkTone::Warning)
                || matches!(remote.status, RuntimeStatus::Offline(_))
        })
    })
}

/// 桌面挂着的远程运行时，以及「添加运行时」的输入和进度。设置页只读它。
#[derive(Clone, Default)]
pub(crate) struct RuntimeHub {
    pub(crate) remotes: Vec<RemoteRuntime>,
    /// 设置页里正在输入的配对串。
    pub(crate) pairing_input: String,
    pub(crate) adding: bool,
    /// 最近一次添加、移除的失败原因。
    pub(crate) error: Option<String>,
}

impl gpui::Global for RuntimeHub {}

/// 这台桌面的 iroh 身份，所有远程运行时共用。绑定要起 tokio 运行时，只能在后台调。
pub(crate) fn iroh_client() -> Result<std::sync::Arc<smelt_iroh::runtime::IrohClient>, String> {
    static CLIENT: std::sync::Mutex<Option<std::sync::Arc<smelt_iroh::runtime::IrohClient>>> =
        std::sync::Mutex::new(None);
    let mut slot = CLIENT.lock().unwrap_or_else(|e| e.into_inner());
    if let Some(client) = slot.as_ref() {
        return Ok(std::sync::Arc::clone(client));
    }
    let secret = smelt_iroh::default_client_secret_path()
        .and_then(|path| smelt_iroh::load_or_create_secret(&path))
        .map_err(|error| format!("读取本机设备身份失败：{error:#}"))?;
    let client = smelt_iroh::runtime::IrohClient::new(secret, None)
        .map_err(|error| format!("起不了 iroh：{error:#}"))?;
    *slot = Some(std::sync::Arc::clone(&client));
    Ok(client)
}

type ConnectorSlot =
    std::sync::Arc<std::sync::Mutex<Option<std::sync::Arc<smelt_iroh::runtime::RuntimeConnector>>>>;

/// 远程运行时的连接器。iroh 要起 tokio，不能在主线程绑，所以第一次连接（总在后台）时
/// 才绑；之后复用同一个，同一条 QUIC 连接上开流。
struct LazyRemoteConnector {
    endpoint: smelt_iroh::EndpointId,
    relay: Option<smelt_iroh::RelaySettings>,
    connector: ConnectorSlot,
}

/// 读一个远程运行时眼下的连接路径。和它的连接器共用一个槽：还没拨过号就读不到，也不会
/// 为了读而去拨号。
#[derive(Clone, Default)]
struct PathProbe(ConnectorSlot);

impl PathProbe {
    /// 阻塞，只在后台调。
    fn read(&self) -> Option<RuntimeLink> {
        let connector = self.0.lock().unwrap_or_else(|e| e.into_inner()).clone()?;
        connector
            .path_status()
            .map(|path| RuntimeLink::from_path(&path))
    }
}

impl smelt_runtime_client::Connector for LazyRemoteConnector {
    fn connect(&self) -> std::io::Result<std::os::unix::net::UnixStream> {
        let connector = {
            let mut slot = self.connector.lock().unwrap_or_else(|e| e.into_inner());
            match slot.as_ref() {
                Some(connector) => std::sync::Arc::clone(connector),
                None => {
                    let client = iroh_client().map_err(std::io::Error::other)?;
                    let connector =
                        std::sync::Arc::new(client.connector(self.endpoint, self.relay.as_ref()));
                    *slot = Some(std::sync::Arc::clone(&connector));
                    connector
                }
            }
        };
        smelt_runtime_client::Connector::connect(connector.as_ref())
    }
}

/// 只解析地址，不碰网络，主线程可以调。
fn remote_client(
    connection: &smelt_client_store::RuntimeConnection,
) -> Result<(RuntimeClient, PathProbe), String> {
    let endpoint: smelt_iroh::EndpointId = connection
        .endpoint_id
        .parse()
        .map_err(|_| format!("运行时的 endpoint id 不合法：{}", connection.endpoint_id))?;
    let relay = connection
        .relay_url
        .as_deref()
        .map(smelt_iroh::RelaySettings::parse)
        .transpose()
        .map_err(|error| format!("relay 地址不合法：{error:#}"))?;
    let probe = PathProbe::default();
    let client = RuntimeClient::new(LazyRemoteConnector {
        endpoint,
        relay,
        connector: probe.0.clone(),
    });
    Ok((client, probe))
}

/// 问一次运行时并对版本。阻塞，只在后台调。
pub(crate) fn status_of(client: &RuntimeClient) -> RuntimeStatus {
    match client.describe() {
        Ok(described) => described_status(described),
        Err(error) => match smelt_client::compat::describe_error(&error) {
            Some(incompatible) => incompatible_status(&incompatible),
            None => RuntimeStatus::Offline(error.to_string()),
        },
    }
}

fn incompatible_status(incompatible: &smelt_client::compat::Incompatibility) -> RuntimeStatus {
    RuntimeStatus::Incompatible {
        detail: incompatible.to_string(),
        runtime_outdated: incompatible.outdated == smelt_client::compat::Outdated::Runtime,
    }
}

/// 这个桌面的版本：请远程运行时升级时就升到它。
pub(crate) const DESKTOP_VERSION: &str = env!("CARGO_PKG_VERSION");

/// 请运行时升级（[`push_upgrade`]）的结局。
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum PushUpgrade {
    /// 运行时接下后断开了：多半是交接到了新版本（旧进程退出），也可能是中途断了网。
    /// 重新探测才知道。
    HandedOff,
    /// 装着的已经是这个版本。
    Current,
    /// 新版本装好了，运行时正忙，空闲时自己换过去。
    Staged(String),
    /// 运行时太旧，不认识这个请求：只能到那台机器上手动装一次。
    Unsupported,
    Failed(String),
}

/// 下载要走公网，慢的机器上几十 MB 要等一阵。
const PUSH_UPGRADE_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(15 * 60);

/// 请远程运行时升到这个桌面的版本：它自己去 GitHub Release 下载、装好、交接（协议见
/// `smelt_runtime_client::protocol::runtime_upgrade`）。阻塞，只在后台调。
pub(crate) fn push_upgrade(client: &RuntimeClient) -> PushUpgrade {
    use smelt_runtime_client::protocol::daemon::DaemonOperation;
    use smelt_runtime_client::protocol::runtime_upgrade::RuntimeUpgradeReply;
    use std::io::BufRead as _;

    let mut stream = match client.connect() {
        Ok(stream) => stream,
        Err(error) => return PushUpgrade::Failed(format!("连不上运行时：{error}")),
    };
    let request = serde_json::json!({
        "op": DaemonOperation::RuntimeUpgrade,
        "version": DESKTOP_VERSION,
    });
    if let Err(error) = smelt_runtime_client::protocol::write_json_line(&mut stream, &request) {
        return PushUpgrade::Failed(format!("发不出升级请求：{error}"));
    }
    let _ = stream.set_read_timeout(Some(PUSH_UPGRADE_TIMEOUT));
    let mut reader = std::io::BufReader::new(stream);
    let mut accepted = false;
    loop {
        let mut line = String::new();
        match reader.read_line(&mut line) {
            Ok(0) if accepted => return PushUpgrade::HandedOff,
            Ok(0) => return PushUpgrade::Unsupported,
            Ok(_) => {}
            Err(error) if accepted => {
                return PushUpgrade::Failed(format!("等运行时升级超时：{error}"));
            }
            Err(error) => return PushUpgrade::Failed(format!("读不到运行时的回复：{error}")),
        }
        match serde_json::from_str::<RuntimeUpgradeReply>(line.trim()) {
            Ok(RuntimeUpgradeReply::Accepted) => accepted = true,
            Ok(RuntimeUpgradeReply::Current) => return PushUpgrade::Current,
            Ok(RuntimeUpgradeReply::Staged { reason }) => return PushUpgrade::Staged(reason),
            Ok(RuntimeUpgradeReply::Failed { error }) => return PushUpgrade::Failed(error),
            Err(_) => return PushUpgrade::Failed(format!("看不懂运行时的回复：{}", line.trim())),
        }
    }
}

/// 运行时太旧、不会自己升级时，到那台机器上执行的命令：装这个桌面的版本。
pub(crate) fn manual_upgrade_command() -> String {
    format!(
        "curl -fsSL https://github.com/smelt-ai/smelt/releases/latest/download/install.sh | SMELT_VERSION={DESKTOP_VERSION} bash"
    )
}

fn described_status(
    described: smelt_runtime_client::protocol::control::SystemDescribeResult,
) -> RuntimeStatus {
    if let Err(incompatible) = smelt_client::compat::check(&described) {
        return incompatible_status(&incompatible);
    }
    RuntimeStatus::Online {
        desktop: described.daemon.desktop.unwrap_or(false),
        version: described.daemon.version,
        platform: described
            .daemon
            .os
            .zip(described.daemon.arch)
            .map(|(os, arch)| format!("{os}/{arch}")),
    }
}

/// 事件订阅报来的连通性（见 `runtime_events`）：记到运行时上给人看。没变就不动 hub，
/// 订阅每次重连都会探测一次，不能每次都让看着 hub 的界面重画。
pub(crate) fn set_status(key: &RuntimeKey, status: RuntimeStatus, cx: &mut gpui::App) {
    let changed = cx.try_global::<RuntimeHub>().is_some_and(|hub| {
        hub.remotes
            .iter()
            .any(|remote| remote.runtime.key() == key && remote.status != status)
    });
    if !changed {
        return;
    }
    update_hub(cx, |hub| {
        if let Some(remote) = hub
            .remotes
            .iter_mut()
            .find(|remote| remote.runtime.key() == key)
        {
            remote.status = status;
        }
    });
}

fn update_hub(cx: &mut gpui::App, change: impl FnOnce(&mut RuntimeHub)) {
    let mut hub = cx.try_global::<RuntimeHub>().cloned().unwrap_or_default();
    change(&mut hub);
    cx.set_global(hub);
}

/// 启动时读出登记过的远程运行时（客户端库在本机，同步读，主窗口一起来侧栏就有它们
/// 的分段），再在后台探一次状态。
pub(crate) fn start_runtime_hub(cx: &mut gpui::App) {
    let mut hub = RuntimeHub::default();
    match smelt_client_store::client_store().and_then(|store| store.runtime_connections()) {
        Ok(connections) => {
            hub.remotes = connections
                .into_iter()
                .map(|connection| match remote_client(&connection) {
                    Ok((client, probe)) => RemoteRuntime {
                        runtime: remote_runtime(&connection.runtime_id, client),
                        connection,
                        status: RuntimeStatus::Connecting,
                        probe,
                        link: None,
                    },
                    Err(error) => RemoteRuntime {
                        runtime: remote_runtime(
                            &connection.runtime_id,
                            RuntimeClient::new(move || {
                                Err(std::io::Error::other("运行时配置无效"))
                            }),
                        ),
                        connection,
                        status: RuntimeStatus::Offline(error),
                        probe: PathProbe::default(),
                        link: None,
                    },
                })
                .collect();
        }
        Err(error) => hub.error = Some(format!("读取运行时列表失败：{error}")),
    }
    cx.set_global(hub);
    refresh_runtime_status(cx);
    watch_runtime_links(cx);
}

/// 隔一会儿读一次各台机器的连接路径和延迟。只读已经连着的，不为此拨号；没变化就不动
/// hub，免得看着它的侧栏跟着重画。
fn watch_runtime_links(cx: &mut gpui::App) {
    const INTERVAL: std::time::Duration = std::time::Duration::from_secs(10);
    cx.spawn(async move |cx| {
        loop {
            let probes = cx.update(|cx| {
                cx.try_global::<RuntimeHub>()
                    .map(|hub| {
                        hub.remotes
                            .iter()
                            .map(|remote| {
                                (
                                    remote.connection.runtime_id.clone(),
                                    matches!(remote.status, RuntimeStatus::Online { .. })
                                        .then(|| remote.probe.clone()),
                                )
                            })
                            .collect::<Vec<_>>()
                    })
                    .unwrap_or_default()
            });
            let links = cx
                .background_executor()
                .spawn(async move {
                    probes
                        .into_iter()
                        .map(|(runtime_id, probe)| (runtime_id, probe.and_then(|p| p.read())))
                        .collect::<Vec<_>>()
                })
                .await;
            cx.update(|cx| {
                let changed = cx.try_global::<RuntimeHub>().is_some_and(|hub| {
                    links.iter().any(|(runtime_id, link)| {
                        hub.remotes.iter().any(|remote| {
                            remote.connection.runtime_id == *runtime_id
                                && link_changed(remote.link.as_ref(), link.as_ref())
                        })
                    })
                });
                if changed {
                    update_hub(cx, |hub| {
                        for (runtime_id, link) in links {
                            if let Some(remote) = hub
                                .remotes
                                .iter_mut()
                                .find(|remote| remote.connection.runtime_id == runtime_id)
                                && link_changed(remote.link.as_ref(), link.as_ref())
                            {
                                remote.link = link;
                            }
                        }
                    });
                }
            });
            cx.background_executor().timer(INTERVAL).await;
        }
    })
    .detach();
}

/// 重新探一遍每个远程运行时（打开设置窗口时调）。
pub(crate) fn refresh_runtime_status(cx: &mut gpui::App) {
    let Some(hub) = cx.try_global::<RuntimeHub>() else {
        return;
    };
    for remote in hub.remotes.clone() {
        // 订阅线程正请它升级、等它换好：这时探到的旧版本盖上去，会把那条线程停掉。
        if matches!(remote.status, RuntimeStatus::Upgrading(_)) {
            continue;
        }
        let runtime_id = remote.connection.runtime_id.clone();
        let client = remote.runtime.client().clone();
        cx.spawn(async move |cx| {
            let status = cx
                .background_executor()
                .spawn(async move { status_of(&client) })
                .await;
            cx.update(|cx| {
                update_hub(cx, |hub| {
                    if let Some(remote) = hub
                        .remotes
                        .iter_mut()
                        .find(|remote| remote.connection.runtime_id == runtime_id)
                    {
                        remote.status = status;
                    }
                })
            });
        })
        .detach();
    }
}

pub(crate) fn set_pairing_input(value: &str, cx: &mut gpui::App) {
    let value = value.trim().to_string();
    update_hub(cx, |hub| hub.pairing_input = value);
}

/// 用配对串添加一个远程运行时：以这台机器的主机名配对，问出它的 runtime_id 和主机名，
/// 记进客户端库。同一个运行时重新配对就更新原来那条。
pub(crate) fn add_runtime(cx: &mut gpui::App) {
    let uri = cx
        .try_global::<RuntimeHub>()
        .map(|hub| hub.pairing_input.clone())
        .unwrap_or_default();
    if uri.is_empty() {
        return;
    }
    update_hub(cx, |hub| {
        hub.adding = true;
        hub.error = None;
    });
    cx.spawn(async move |cx| {
        let added = cx
            .background_executor()
            .spawn(async move { pair_with(&uri) })
            .await;
        cx.update(|cx| {
            update_hub(cx, |hub| {
                hub.adding = false;
                match added {
                    Ok(remote) => {
                        hub.pairing_input.clear();
                        hub.remotes.retain(|known| {
                            known.connection.runtime_id != remote.connection.runtime_id
                        });
                        hub.remotes.push(remote);
                    }
                    Err(error) => hub.error = Some(error),
                }
            })
        });
    })
    .detach();
}

pub(crate) fn pair_with(uri: &str) -> Result<RemoteRuntime, String> {
    use smelt_model::control_api::DevicePair;
    use smelt_model::remote_config::PairParams;

    let pairing = smelt_pairing::parse_pairing_uri(uri)?;
    let mut connection = smelt_client_store::RuntimeConnection {
        runtime_id: String::new(),
        name: String::new(),
        endpoint_id: pairing.endpoint_id,
        relay_url: pairing.relay_url,
        added_at_ms: std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|elapsed| elapsed.as_millis() as i64)
            .unwrap_or_default(),
    };
    let (client, probe) = remote_client(&connection)?;
    client
        .call::<DevicePair>(
            &PairParams {
                code: pairing.code,
                name: smelt_sys::hostname().unwrap_or_else(|| "Smelt 桌面".to_string()),
            },
            PAIR_TIMEOUT,
        )
        .map_err(|error| format!("配对失败：{error}"))?;
    let described = client
        .describe()
        .map_err(|error| format!("配对成功，但读取运行时信息失败：{error}"))?;
    connection.runtime_id = described
        .daemon
        .runtime_id
        .clone()
        .ok_or_else(|| "这个运行时太旧，没有 runtime_id，先升级它".to_string())?;
    connection.name = described
        .daemon
        .hostname
        .clone()
        .unwrap_or_else(|| connection.endpoint_id.chars().take(8).collect());
    smelt_client_store::client_store()
        .and_then(|store| store.put_runtime_connection(&connection))
        .map_err(|error| format!("保存运行时失败：{error}"))?;
    Ok(RemoteRuntime {
        runtime: remote_runtime(&connection.runtime_id, client),
        status: described_status(described),
        connection,
        probe,
        link: None,
    })
}

/// 改一台机器在这台电脑上的显示名：先改 hub，侧栏和机器面板立刻换上；客户端库在后台写。
pub(crate) fn rename_runtime(runtime_id: String, name: String, cx: &mut gpui::App) {
    let mut renamed = None;
    update_hub(cx, |hub| {
        if let Some(remote) = hub
            .remotes
            .iter_mut()
            .find(|remote| remote.connection.runtime_id == runtime_id)
        {
            remote.connection.name = name;
            renamed = Some(remote.connection.clone());
        }
    });
    let Some(connection) = renamed else {
        return;
    };
    cx.background_executor()
        .spawn(async move {
            if let Err(error) = smelt_client_store::client_store()
                .and_then(|store| store.put_runtime_connection(&connection))
            {
                eprintln!("[runtimes] 保存机器名字失败：{error}");
            }
        })
        .detach();
}

/// 配对要走公网拨号，比本机调用慢得多。
const PAIR_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(30);

/// 移除一个远程运行时：先请它撤销这台设备（连不上就算了），再从客户端库删掉。
pub(crate) fn remove_runtime(runtime_id: String, cx: &mut gpui::App) {
    let Some(remote) = cx.try_global::<RuntimeHub>().and_then(|hub| {
        hub.remotes
            .iter()
            .find(|remote| remote.connection.runtime_id == runtime_id)
            .cloned()
    }) else {
        return;
    };
    cx.spawn(async move |cx| {
        let removed = cx
            .background_executor()
            .spawn(async move {
                if let Ok(client) = iroh_client() {
                    let _ = remote
                        .runtime
                        .client()
                        .call::<smelt_model::control_api::DeviceRevoke>(
                            &smelt_model::remote_config::DeviceIdParams {
                                endpoint_id: client.id().to_string(),
                            },
                            smelt_runtime_client::DEFAULT_REQUEST_TIMEOUT,
                        );
                }
                smelt_client_store::client_store()
                    .and_then(|store| store.delete_runtime_connection(&runtime_id))
                    .map(|_| runtime_id)
                    .map_err(|error| format!("移除运行时失败：{error}"))
            })
            .await;
        cx.update(|cx| {
            update_hub(cx, |hub| match removed {
                Ok(runtime_id) => hub
                    .remotes
                    .retain(|remote| remote.connection.runtime_id != runtime_id),
                Err(error) => hub.error = Some(error),
            })
        });
    })
    .detach();
}

#[cfg(test)]
mod tests {
    use super::*;

    fn link(kind: LinkKind, rtt_ms: u32) -> RuntimeLink {
        RuntimeLink { kind, rtt_ms }
    }

    fn online() -> RuntimeStatus {
        RuntimeStatus::Online {
            version: "0.9.0".into(),
            platform: None,
            desktop: false,
        }
    }

    #[test]
    fn relay_names_keep_only_what_helps() {
        assert_eq!(
            relay_name("relay:https://usw1-1.relay.n0.iroh.link./"),
            "n0 usw1-1"
        );
        assert_eq!(
            relay_name("relay:https://216-75-75-181.sslip.io/"),
            "216-75-75-181.sslip.io"
        );
    }

    /// 延迟抖一下就让侧栏重画不值得；换了路径、或者变化够大才算变。
    #[test]
    fn small_latency_jitter_is_not_a_change() {
        let relay = |rtt| Some(link(LinkKind::Relay("n0 usw1-1".into()), rtt));
        assert!(!link_changed(relay(437).as_ref(), relay(450).as_ref()));
        assert!(link_changed(relay(437).as_ref(), relay(720).as_ref()));
        assert!(link_changed(
            relay(437).as_ref(),
            Some(link(LinkKind::Direct, 437)).as_ref()
        ));
        assert!(link_changed(relay(437).as_ref(), None));
        assert!(!link_changed(None, None));
    }

    /// 侧栏只在需要注意时写字：直连不写，中继写延迟，慢了用警告色。
    #[test]
    fn the_sidebar_only_speaks_up_when_it_matters() {
        let status = online();
        assert_eq!(
            link_text(&status, Some(&link(LinkKind::Direct, 12)), true),
            None
        );
        assert_eq!(link_text(&status, None, true), None);
        assert_eq!(
            link_text(
                &status,
                Some(&link(LinkKind::Relay("n0 usw1-1".into()), 437)),
                true
            ),
            Some(("中继 · 437ms".into(), LinkTone::Warning))
        );
        assert_eq!(
            link_text(
                &status,
                Some(&link(LinkKind::Relay("relay.example".into()), 60)),
                true
            ),
            Some(("中继 · 60ms".into(), LinkTone::Muted))
        );
        assert_eq!(
            link_text(&RuntimeStatus::Offline("断了".into()), None, true),
            Some(("离线".into(), LinkTone::Muted))
        );
        // 机器面板里说全。
        assert_eq!(
            link_text(&status, Some(&link(LinkKind::Direct, 12)), false),
            Some(("直连 · 12ms".into(), LinkTone::Muted))
        );
        assert_eq!(
            link_text(
                &status,
                Some(&link(LinkKind::Relay("n0 usw1-1".into()), 437)),
                false
            ),
            Some(("中继 n0 usw1-1 · 437ms".into(), LinkTone::Warning))
        );
    }

    /// 每次连接都用 `respond` 回一条 `system.describe`，模拟另一个版本的运行时。
    fn runtime_answering(
        respond: impl Fn(&str) -> serde_json::Value + Send + Sync + Clone + 'static,
    ) -> RuntimeClient {
        use std::io::{BufRead, Write};
        RuntimeClient::new(move || {
            let (client, server) = std::os::unix::net::UnixStream::pair()?;
            let respond = respond.clone();
            std::thread::spawn(move || {
                let mut line = String::new();
                std::io::BufReader::new(server.try_clone().unwrap())
                    .read_line(&mut line)
                    .unwrap();
                let request: serde_json::Value = serde_json::from_str(&line).unwrap();
                let response = respond(request["request_id"].as_str().unwrap());
                writeln!(&server, "{response}").unwrap();
            });
            Ok(client)
        })
    }

    fn describe_without(method: &'static str) -> RuntimeClient {
        use smelt_runtime_client::protocol::control::{
            CONTROL_API_MIN_SUPPORTED_VERSION, CONTROL_API_VERSION, ControlMethod,
            ControlProtocolDescription, ControlResponse, DaemonRuntimeInfo,
            EventProtocolDescription, SystemDescribeResult,
        };
        runtime_answering(move |request_id| {
            let described = SystemDescribeResult {
                control: ControlProtocolDescription {
                    current_version: CONTROL_API_VERSION,
                    min_supported_version: CONTROL_API_MIN_SUPPORTED_VERSION,
                    max_supported_version: CONTROL_API_VERSION,
                    methods: ControlMethod::ALL
                        .iter()
                        .map(ToString::to_string)
                        .filter(|name| name != method)
                        .collect(),
                },
                daemon: DaemonRuntimeInfo {
                    version: "0.0.1".to_string(),
                    exe_mtime: 0,
                    exe: None,
                    pid: 1,
                    started_at: 0,
                    session_count: 0,
                    agent_event_version: 1,
                    runtime_id: Some("runtime-a".to_string()),
                    os: Some("linux".to_string()),
                    arch: Some("x86_64".to_string()),
                    hostname: None,
                    desktop: None,
                },
                events: Some(EventProtocolDescription {
                    protocol_version: smelt_event_protocol::EVENT_BUS_PROTOCOL_VERSION,
                    projection_version: smelt_event_protocol::CORE_PROJECTION_VERSION,
                    session_projection_version:
                        smelt_event_protocol::CORE_SESSION_PROJECTION_VERSION,
                    session_state_schema_version:
                        smelt_event_protocol::CORE_SESSION_STATE_SCHEMA_VERSION,
                }),
            };
            serde_json::to_value(ControlResponse::ok(request_id, described)).unwrap()
        })
    }

    #[test]
    fn matching_runtime_is_online() {
        assert!(matches!(
            status_of(&describe_without("no.such.method")),
            RuntimeStatus::Online { .. }
        ));
    }

    /// 旧运行时少了桌面要用的方法：标成不兼容并说要升级运行时，而不是显示在线、
    /// 之后各处零散地报 MethodNotFound。
    #[test]
    fn older_runtime_is_incompatible() {
        let RuntimeStatus::Incompatible {
            detail,
            runtime_outdated,
        } = status_of(&describe_without("workspace.load"))
        else {
            panic!("缺方法的运行时应判为不兼容");
        };
        assert!(runtime_outdated, "运行时旧，可以请它升级");
        assert!(detail.contains("需要升级运行时"), "{detail}");
        assert!(detail.contains("workspace.load"), "{detail}");
    }

    /// 运行时的控制协议比桌面新，连 describe 都被拒：仍要认出是版本问题，提示升级桌面。
    #[test]
    fn runtime_rejecting_our_control_version_is_incompatible() {
        use smelt_runtime_client::protocol::control::{
            CONTROL_API_VERSION, ControlError, ControlResponse,
        };
        let client = runtime_answering(|request_id| {
            serde_json::to_value(ControlResponse::<()>::error_with_version(
                CONTROL_API_VERSION + 1,
                request_id,
                ControlError::unsupported_version(
                    CONTROL_API_VERSION,
                    CONTROL_API_VERSION + 1,
                    CONTROL_API_VERSION + 1,
                ),
            ))
            .unwrap()
        });
        let RuntimeStatus::Incompatible {
            detail,
            runtime_outdated,
        } = status_of(&client)
        else {
            panic!("控制协议对不上应判为不兼容");
        };
        assert!(!runtime_outdated, "运行时比桌面新，只升不降，不能请它升级");
        assert!(detail.contains("需要升级桌面"), "{detail}");
    }

    /// 订阅线程把每次探测到的连通性报回来，订阅断开也报：侧栏的「离线」靠它更新，
    /// 不能只在启动时探一次。
    #[test]
    fn subscription_reports_reachability_changes() {
        use std::sync::Arc;
        use std::sync::atomic::{AtomicBool, Ordering};
        let (events, _events_rx) = smol::channel::unbounded();
        let (status, status_rx) = smol::channel::unbounded();
        let stop = Arc::new(AtomicBool::new(false));
        crate::runtime_events::spawn_subscription(
            remote_runtime("rt-test", describe_without("no.such.method")),
            events,
            status,
            Arc::clone(&stop),
        );
        let (key, first) = status_rx.recv_blocking().unwrap();
        assert_eq!(key, RuntimeKey::Remote("rt-test".into()));
        assert!(matches!(first, RuntimeStatus::Online { .. }), "{first:?}");
        // 假运行时只答 describe，订阅随即断开：断开要报离线。
        let (_, second) = status_rx.recv_blocking().unwrap();
        assert!(matches!(second, RuntimeStatus::Offline(_)), "{second:?}");
        stop.store(true, Ordering::Release);
    }

    /// 假运行时：读到首行升级请求后按顺序回这几行，然后断开。
    fn runtime_replying(lines: &'static [&'static str]) -> RuntimeClient {
        use std::io::{BufRead, Write};
        RuntimeClient::new(move || {
            let (client, server) = std::os::unix::net::UnixStream::pair()?;
            std::thread::spawn(move || {
                let mut line = String::new();
                std::io::BufReader::new(server.try_clone().unwrap())
                    .read_line(&mut line)
                    .unwrap();
                let request: serde_json::Value = serde_json::from_str(&line).unwrap();
                assert_eq!(request["op"], "runtime_upgrade");
                assert_eq!(request["version"], DESKTOP_VERSION);
                for reply in lines {
                    writeln!(&server, "{reply}").unwrap();
                }
            });
            Ok(client)
        })
    }

    /// 不认识这个请求的旧运行时一行不回就断开：认成「不支持」，提示手动装，而不是
    /// 当成升级成功。
    #[test]
    fn a_runtime_that_hangs_up_without_a_word_cannot_upgrade_itself() {
        assert_eq!(push_upgrade(&runtime_replying(&[])), PushUpgrade::Unsupported);
    }

    /// 接下以后断开：交接时旧进程退出就是这样，要重新探测。
    #[test]
    fn hanging_up_after_accepting_means_go_look_again() {
        assert_eq!(
            push_upgrade(&runtime_replying(&[r#"{"state":"accepted"}"#])),
            PushUpgrade::HandedOff
        );
    }

    #[test]
    fn the_runtime_says_how_the_upgrade_ended() {
        assert_eq!(
            push_upgrade(&runtime_replying(&[
                r#"{"state":"accepted"}"#,
                r#"{"state":"failed","error":"GitHub 上没有 v9.9.9"}"#,
            ])),
            PushUpgrade::Failed("GitHub 上没有 v9.9.9".into())
        );
        assert_eq!(
            push_upgrade(&runtime_replying(&[
                r#"{"state":"accepted"}"#,
                r#"{"state":"staged","reason":"ACP 会话仍有未完成请求"}"#,
            ])),
            PushUpgrade::Staged("ACP 会话仍有未完成请求".into())
        );
        assert_eq!(
            push_upgrade(&runtime_replying(&[r#"{"state":"current"}"#])),
            PushUpgrade::Current
        );
    }

    #[test]
    fn unreachable_runtime_is_offline() {
        let client = RuntimeClient::new(|| Err(std::io::Error::other("connection refused")));
        assert!(matches!(status_of(&client), RuntimeStatus::Offline(_)));
    }

    /// 对着一台真实运行时走一遍「添加运行时」：配对、问出身份、记进客户端库，再连一次。
    /// 要联网、要现成的配对串，默认不跑：
    /// `SMELT_TEST_PAIRING_URI='smelt+iroh://…' cargo test -p smelt runtimes -- --ignored`
    #[test]
    #[ignore = "需要一台真实运行时和一次性配对串"]
    fn pairs_with_a_real_runtime() {
        let uri = std::env::var("SMELT_TEST_PAIRING_URI").expect("设置 SMELT_TEST_PAIRING_URI");
        let home =
            std::env::temp_dir().join(format!("smelt-pair-test-{}", uuid::Uuid::new_v4().simple()));
        // SAFETY: nextest 每个测试一个进程，这里没有别的线程在读环境变量。
        unsafe { std::env::set_var("SMELT_HOME", &home) };

        let remote = pair_with(&uri).unwrap();
        assert!(matches!(remote.status, RuntimeStatus::Online { .. }));
        let stored = smelt_client_store::client_store()
            .unwrap()
            .runtime_connections()
            .unwrap();
        assert_eq!(stored, vec![remote.connection]);

        let again = remote_client(&stored[0]).unwrap().0.describe().unwrap();
        assert_eq!(
            again.daemon.runtime_id.as_deref(),
            Some(stored[0].runtime_id.as_str())
        );
        eprintln!("已添加 {}（{}）", stored[0].name, stored[0].runtime_id);
        let _ = std::fs::remove_dir_all(home);
    }

    /// 对着一台真实运行时走一遍桌面要用的远程链路：配对、读工作区目录、在那台机器上开
    /// 终端、订阅里收到这个终端的状态，最后杀掉终端、撤销这台设备。默认不跑：
    /// `SMELT_TEST_PAIRING_URI='smelt+iroh://…' cargo test -p smelt runtimes -- --ignored`
    #[test]
    #[ignore = "需要一台真实运行时和一次性配对串"]
    fn drives_a_real_remote_runtime() {
        use smelt_runtime_client::protocol::control::EmptyParams;
        use smelt_runtime_client::protocol::workspace::WorkspaceLoadCall;

        let uri = std::env::var("SMELT_TEST_PAIRING_URI").expect("设置 SMELT_TEST_PAIRING_URI");
        let home = std::env::temp_dir().join(format!(
            "smelt-remote-test-{}",
            uuid::Uuid::new_v4().simple()
        ));
        // SAFETY: nextest 每个测试一个进程，这里没有别的线程在读环境变量。
        unsafe { std::env::set_var("SMELT_HOME", &home) };

        let remote = pair_with(&uri).unwrap();
        let runtime = remote.runtime.clone();
        runtime
            .client()
            .call::<WorkspaceLoadCall>(&EmptyParams::default(), std::time::Duration::from_secs(30))
            .unwrap();

        let (tx, rx) = std::sync::mpsc::channel();
        let subscriber = runtime.client().clone();
        std::thread::spawn(move || {
            crate::terminal::subscribe_daemon_states_blocking(&subscriber, |event| {
                tx.send(event).is_ok()
            });
        });

        let sid = format!("remote-test-{}", uuid::Uuid::new_v4().simple());
        let terminal = crate::terminal::Terminal::spawn(
            &runtime,
            24,
            80,
            None,
            &sid,
            Some("echo smelt-remote-ok; sleep 60"),
        )
        .unwrap();

        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
        let mut seen = false;
        while !seen && std::time::Instant::now() < deadline {
            let Ok(event) = rx.recv_timeout(std::time::Duration::from_secs(1)) else {
                continue;
            };
            seen = match event {
                crate::terminal::DaemonStateEvent::Snapshot { sessions, .. } => {
                    sessions.iter().any(|state| state.id == sid)
                }
                crate::terminal::DaemonStateEvent::Update(state) => state.id == sid,
                _ => false,
            };
        }
        assert!(seen, "订阅里没看到远程终端 {sid}");

        drop(terminal);
        crate::terminal::kill_remote(&runtime, &sid);
        // 撤销自己时运行时会先断掉这台设备的连接，回执不一定发得出来；看之后还能不能调。
        let me = iroh_client().unwrap().id().to_string();
        let _ = runtime
            .client()
            .call::<smelt_model::control_api::DeviceRevoke>(
                &smelt_model::remote_config::DeviceIdParams { endpoint_id: me },
                smelt_runtime_client::DEFAULT_REQUEST_TIMEOUT,
            );
        assert!(
            runtime
                .client()
                .call::<WorkspaceLoadCall>(
                    &EmptyParams::default(),
                    std::time::Duration::from_secs(30)
                )
                .is_err(),
            "撤销之后不该还能调"
        );
        eprintln!(
            "远程链路走通：{}（{}）",
            remote.connection.name, remote.connection.runtime_id
        );
        let _ = std::fs::remove_dir_all(home);
    }
}
