//! 桌面挂着的运行时（设计文档 D6）。本机运行时总在；远程运行时是本机运行时的对端节点，
//! 经本机 smeltd 转发去连（见 `docs/cross-machine.md`）。会话、视图各自记着自己属于哪个
//! 运行时，调用、连接都发给它。
//!
//! 旧版桌面用自己的 iroh 身份直连、记在客户端库里的机器，连上后自动迁成对端节点（见
//! [`migrate`]）；迁完之前照旧直连。

use rust_i18n::t;
use smelt_model::runtime_path::PathStyle;
use smelt_runtime_client::RuntimeClient;

pub(crate) use smelt_client::runtime::{Runtime, RuntimeKey};

/// 本机运行时的客户端。连本机 socket；守护的拉起和升级由 [`crate::local_daemon`] 管。
pub(crate) fn local() -> RuntimeClient {
    Runtime::local().client().clone()
}

/// 发给 `key` 那台机器的客户端。远程机器已经移除就是 `None`。
/// 调用方报错，不退回另一台机器：发错机器比报错更糟。
pub(crate) fn client(key: &RuntimeKey, cx: &gpui::App) -> Option<RuntimeClient> {
    match key {
        RuntimeKey::Local => Some(local()),
        RuntimeKey::Remote(_) => remote_of(key, cx).map(|remote| remote.runtime.client().clone()),
    }
}

/// `key` 那台机器（连同客户端）；远程的已经移除就是 `None`。
pub(crate) fn runtime(key: &RuntimeKey, cx: &gpui::App) -> Option<Runtime> {
    client(key, cx).map(|client| Runtime::new(key.clone(), client))
}

/// 那台机器上的路径（会话 cwd、项目根）按哪种系统的规则切：本机就是这台电脑；远程的取
/// 连上时记下的 `os`，还没连上过当 POSIX——运行时开始支持 Windows 之前就在报 `os` 了，
/// 不报的只会是 Linux、macOS。
pub(crate) fn path_style(key: &RuntimeKey, cx: &gpui::App) -> PathStyle {
    match key {
        RuntimeKey::Local => PathStyle::LOCAL,
        RuntimeKey::Remote(_) => {
            PathStyle::of_os(remote_of(key, cx).and_then(|remote| remote.connection.os.as_deref()))
        }
    }
}

/// 开终端、对话长连接的连接器（见 [`smelt_plugin_api::Host::connector`]），可以带去后台
/// 线程。本机守护没在跑就先拉起来，这只有本机做得到；远程运行时连不上就是连不上，由调用方
/// 按断线处理。
pub(crate) fn connector(runtime: &Runtime) -> std::sync::Arc<dyn smelt_runtime_client::Connector> {
    if runtime.is_local() {
        // 拉起本机 smeltd 的生命周期见设计文档 D3/D4。
        return std::sync::Arc::new(crate::local_daemon::connect_daemon);
    }
    let client = runtime.client().clone();
    std::sync::Arc::new(move || client.connect())
}

/// 本机的序列号或主机名；两样都取不到是 `None`，显示时用「本机」（跟着语言）。
static LOCAL_NAME: std::sync::OnceLock<Option<String>> = std::sync::OnceLock::new();

/// 初始化本机名字：启动时在后台读取硬件序列号，render 只读内存缓存。
pub(crate) fn init_local_runtime_name(cx: &gpui::App) {
    cx.spawn(async move |cx| {
        let name = cx
            .background_executor()
            .spawn(async { smelt_sys::hardware_serial().or_else(smelt_sys::hostname) })
            .await;
        let _ = LOCAL_NAME.set(name);
        cx.update(|cx| {
            update_hub(cx, |_| {});
        });
    })
    .detach();
}

/// 本机运行时名称：纯读内存缓存，启动未取到时兜底为「本机」。
pub(crate) fn local_runtime_name() -> String {
    match LOCAL_NAME.get() {
        Some(Some(name)) => name.clone(),
        _ => t!("runtime.local").into_owned(),
    }
}

/// 给人看的运行时名字：本机优先展示电脑序列号，远程用配对时记下的主机名。
pub(crate) fn runtime_name(key: &RuntimeKey, cx: &gpui::App) -> String {
    match key {
        RuntimeKey::Local => local_runtime_name(),
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

/// 远程运行时不能用时给机器标的字（按最近一次探测），能用就是 `None`。本机总能用。
pub(crate) fn runtime_unavailable_label(key: &RuntimeKey, cx: &gpui::App) -> Option<String> {
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
            Some(RuntimeStatus::Incompatible { .. }) => {
                Some(t!("runtime.incompatible").into_owned())
            }
            Some(RuntimeStatus::Upgrading(_)) => Some(t!("runtime.upgrading").into_owned()),
            Some(RuntimeStatus::Connecting | RuntimeStatus::Offline(_)) | None => {
                Some(t!("runtime.offline").into_owned())
            }
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
    },
    Offline(String),
    /// 连得上，但版本和这个桌面对不上（见 `smelt_runtime_client::compat`），说明里写着该升级
    /// 哪一边。这时不订阅它的事件：订阅了也解不出来，只会断开重连。
    Incompatible {
        /// 运行时自己报的版本。
        version: String,
        detail: String,
        fix: IncompatibleFix,
    },
    /// 正在请它升到这个桌面的版本，带着给人看的进展。
    Upgrading(String),
}

/// 版本对不上时该升级哪一边、怎么升。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum IncompatibleFix {
    /// 运行时旧，桌面请它自己升到这个桌面的版本（见 [`push_upgrade`]）。
    UpgradeRuntime,
    /// 运行时旧，但它随那台机器上的 Smelt 一起更新，桌面请不动它（见 [`upgrades_remotely`]）。
    /// 本机守护照样由桌面重启换上 App 里的那份。
    UpdateSmeltThere,
    /// 运行时比桌面新：升级这个桌面。
    UpdateDesktop,
}

impl IncompatibleFix {
    /// 运行时比桌面旧。
    pub(crate) fn runtime_outdated(self) -> bool {
        !matches!(self, Self::UpdateDesktop)
    }
}

/// 这台运行时能不能由桌面远程升级。`runtime_upgrade` 只在 Linux 上实现：Windows 和
/// macOS 上运行时随那台机器上的 Smelt 桌面一起更新。不报 `os` 的旧运行时照常请它升，
/// 它不认这个请求时 [`push_upgrade`] 回 `Unsupported`。
pub(crate) fn upgrades_remotely(os: Option<&str>) -> bool {
    os.is_none_or(|os| os == "linux")
}

/// 一个登记过的远程运行时。
#[derive(Clone)]
pub(crate) struct RemoteRuntime {
    pub(crate) connection: smelt_client_store::RuntimeConnection,
    pub(crate) runtime: Runtime,
    pub(crate) status: RuntimeStatus,
    /// 怎么连它，和它的客户端共用：迁移时就地换掉，拿着这台机器客户端的终端、对话下次
    /// 开流就走新的（见 [`SwitchConnector`]）。
    transport: TransportSlot,
    /// 盯着连接路径的守卫。
    watch: LinkWatchSlot,
    /// 本机智能体能不能经本机运行时用它。还没迁到本机运行时的旧直连机器是 `None`。
    pub(crate) agent_access: Option<bool>,
    /// 它能不能连进来操作这台电脑。`None` 同上。
    pub(crate) inbound: Option<bool>,
    /// 最近一次读到的连接路径和延迟。只在连着时有：连上时、iroh 换了路径时、设置停在机器
    /// 页或悬停在机器名上时读（见 [`sync_link_watch`]、[`sample_links`]），不定时读。
    pub(crate) link: Option<RuntimeLink>,
    /// 人点「升级运行时」之后没升成的原因（已是最新、正忙、太旧、出错）。再点一次清掉。
    pub(crate) upgrade_note: Option<String>,
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
    fn from_peer(link: smelt_model::peer_node::PeerLink) -> Self {
        use smelt_model::peer_node::PeerRoute;
        Self {
            kind: match link.route {
                PeerRoute::Direct => LinkKind::Direct,
                PeerRoute::Relay { address } => LinkKind::Relay(relay_name(&address)),
                PeerRoute::Other => LinkKind::Other,
            },
            rtt_ms: link.rtt_ms,
        }
    }

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

fn remote_of<'a>(key: &RuntimeKey, cx: &'a gpui::App) -> Option<&'a RemoteRuntime> {
    cx.try_global::<RuntimeHub>()?
        .remotes
        .iter()
        .find(|remote| remote.runtime.key() == key)
}

/// 一个运行时最近一次探到的状态。本机还没探过时是 `None`。
fn status_of_key<'a>(key: &RuntimeKey, cx: &'a gpui::App) -> Option<&'a RuntimeStatus> {
    match key {
        RuntimeKey::Local => cx.try_global::<RuntimeHub>()?.local_status.as_ref(),
        RuntimeKey::Remote(_) => remote_of(key, cx).map(|remote| &remote.status),
    }
}

/// 最近一次探测是版本对不上。
pub(crate) fn runtime_incompatible(key: &RuntimeKey, cx: &gpui::App) -> bool {
    status_of_key(key, cx)
        .is_some_and(|status| matches!(status, RuntimeStatus::Incompatible { .. }))
}

/// 本机守护版本对不上、正在换代：这时由订阅线程负责升级（见 `runtime_events`），
/// 别处的空闲升级不要再插一脚。只在有本机 smeltd 的平台上有意义。
pub(crate) fn local_upgrade_in_hand(cx: &gpui::App) -> bool {
    status_of_key(&RuntimeKey::Local, cx).is_some_and(|status| {
        matches!(
            status,
            RuntimeStatus::Incompatible { .. } | RuntimeStatus::Upgrading(_)
        )
    })
}

/// 远程机器眼下连得上（在线，或运行时旧得对不上），而且能远程升级：可以请它升级。
/// 升级走守护协议的 `runtime_upgrade`，不经控制方法，版本对不上也发得过去。
/// 随 Smelt 桌面更新的运行时（Windows、macOS）不给这一项，见 [`upgrades_remotely`]。开发构建
/// 没有对应的发版可升，也不给。
pub(crate) fn runtime_can_upgrade(key: &RuntimeKey, cx: &gpui::App) -> bool {
    if smelt_version::RELEASE.is_none() {
        return false;
    }
    remote_of(key, cx).is_some_and(|remote| match &remote.status {
        RuntimeStatus::Online { platform, .. } => upgrades_remotely(
            platform
                .as_deref()
                .and_then(|platform| platform.split('/').next()),
        ),
        RuntimeStatus::Incompatible { fix, .. } => *fix == IncompatibleFix::UpgradeRuntime,
        _ => false,
    })
}

/// 记下（或清掉）一台远程机器最近一次手动升级没升成的原因。
pub(crate) fn set_upgrade_note(key: &RuntimeKey, note: Option<String>, cx: &mut gpui::App) {
    update_hub(cx, |hub| {
        if let Some(remote) = hub
            .remotes
            .iter_mut()
            .find(|remote| remote.runtime.key() == key)
        {
            remote.upgrade_note = note;
        }
    });
}

/// 连不上或版本对不上的远程机器，菜单里给「重新连接」。
pub(crate) fn runtime_can_reconnect(key: &RuntimeKey, cx: &gpui::App) -> bool {
    remote_of(key, cx).is_some_and(|remote| {
        matches!(
            remote.status,
            RuntimeStatus::Offline(_) | RuntimeStatus::Incompatible { .. }
        )
    })
}

/// 远程机器的连接表，交给插件（[`smelt_plugin_api::Host::connections`]）。只读内存。
pub(crate) fn connections(cx: &gpui::App) -> smelt_plugin_api::Connections {
    use smelt_plugin_api::{Connection, ConnectionLink, ConnectionState, LinkRoute};
    let Some(hub) = cx.try_global::<RuntimeHub>() else {
        return Default::default();
    };
    let remotes = hub
        .remotes
        .iter()
        .map(|remote| {
            let key = remote.runtime.key();
            Connection {
                machine: key.clone(),
                name: remote.connection.name.clone(),
                relay: remote.connection.relay_url.clone(),
                os: remote.connection.os.clone(),
                state: match &remote.status {
                    RuntimeStatus::Connecting => ConnectionState::Connecting,
                    RuntimeStatus::Online {
                        version, platform, ..
                    } => ConnectionState::Online {
                        version: version.clone(),
                        platform: platform.clone(),
                    },
                    RuntimeStatus::Offline(error) => ConnectionState::Offline {
                        error: error.clone(),
                    },
                    RuntimeStatus::Incompatible {
                        version, detail, ..
                    } => ConnectionState::Incompatible {
                        version: version.clone(),
                        detail: detail.clone(),
                    },
                    RuntimeStatus::Upgrading(progress) => ConnectionState::Upgrading {
                        progress: progress.clone(),
                    },
                },
                link: remote.link.as_ref().map(|link| ConnectionLink {
                    route: match &link.kind {
                        LinkKind::Direct => LinkRoute::Direct,
                        LinkKind::Relay(relay) => LinkRoute::Relay {
                            relay: relay.clone(),
                        },
                        LinkKind::Other => LinkRoute::Other,
                    },
                    rtt_ms: link.rtt_ms,
                    slow: link.rtt_ms >= SLOW_RTT_MS,
                }),
                upgrade_note: remote.upgrade_note.clone(),
                can_upgrade: runtime_can_upgrade(key, cx),
                can_reconnect: runtime_can_reconnect(key, cx),
                agent_access: remote.agent_access,
                inbound: remote.inbound,
            }
        })
        .collect();
    smelt_plugin_api::Connections {
        remotes,
        load_error: hub.load_error.clone(),
    }
}

/// 桌面挂着的远程运行时。插件经 [`connections`] 读。
#[derive(Clone, Default)]
pub(crate) struct RuntimeHub {
    pub(crate) remotes: Vec<RemoteRuntime>,
    /// 启动时没读出登记过的机器（比如客户端库版本比程序新）。这时列表里只剩本机，不说
    /// 一声就像机器全丢了，所以管机器的插件要显示它（见 [`connections`]）。
    pub(crate) load_error: Option<String>,
    /// 本机守护最近一次探到的状态，订阅线程报来（见 `runtime_events`）。还没探过是 `None`。
    pub(crate) local_status: Option<RuntimeStatus>,
    /// 已经从本机运行时读到过对端节点。读不到（守护没在跑、还是不认 `peer.*` 的旧版本）时
    /// 只有客户端库里的旧直连机器，也不迁移、不经它配对。
    peers_loaded: bool,
}

impl gpui::Global for RuntimeHub {}

/// 系统睡醒后换掉往外拨的连接，再请离线的机器立刻重探。
///
/// 长睡醒来（中间还换过网络、开关过代理）后，旧 endpoint 可能再也连不上 relay：之后每次
/// 拨号都等满时限报超时，所有远程机器一直显示离线，重开应用才好（2026-11-09，睡了一个多
/// 小时，relay 和远程机器都好着，app 里一条到 relay 的连接都没有）。iroh 没能自己恢复，
/// 卡在哪还没查清（它的警告现在进 app.log，来源 `iroh`），所以不指望它，醒来就换新的。
///
/// 经本机运行时转发的机器，请本机运行时丢掉往外拨的连接（`peer.reset`）；还在直连的旧
/// 机器，丢掉这个桌面自己的 endpoint。旧连接上还活着的订阅、终端、对话随之断开，按原有的
/// 断线重连接上新的；睡过一觉这些连接多半已经超时，代价很小。
pub(crate) fn reset_iroh_after_wake(cx: &mut gpui::App) {
    let routes: Vec<RemoteRoute> = cx
        .try_global::<RuntimeHub>()
        .map(|hub| {
            hub.remotes
                .iter()
                .filter_map(|remote| remote.transport.legacy_route())
                .collect()
        })
        .unwrap_or_default();
    cx.spawn(async move |cx| {
        let reset = cx
            .background_executor()
            .spawn(async move {
                let peers = local()
                    .call::<smelt_model::control_api::PeerReset>(
                        &Default::default(),
                        smelt_runtime_client::DEFAULT_REQUEST_TIMEOUT,
                    )
                    .is_ok();
                reset_iroh_client(&routes) || peers
            })
            .await;
        if reset {
            smelt_sys::app_log::log("info", "iroh", "系统睡醒，换掉旧的 iroh 连接");
            cx.update(|cx| crate::runtime_events::probe_remotes(cx));
        }
    })
    .detach();
}

/// 这台桌面自己的 iroh endpoint，只给还没迁到本机运行时的旧直连机器用（同一把密钥绑几个
/// 会让 relay 上出现重复节点）。第一次拨号时才绑；[`reset_iroh_client`] 丢掉之后下一次
/// 拨号重绑。旧机器都迁完就再也不绑。
static IROH_CLIENT: std::sync::Mutex<Option<std::sync::Arc<smelt_iroh::runtime::IrohClient>>> =
    std::sync::Mutex::new(None);

/// 这台桌面的旧 iroh 身份。绑定要起 tokio 运行时，只能在后台调。
fn iroh_client() -> Result<std::sync::Arc<smelt_iroh::runtime::IrohClient>, String> {
    let mut slot = IROH_CLIENT.lock().unwrap_or_else(|e| e.into_inner());
    if let Some(client) = slot.as_ref() {
        return Ok(std::sync::Arc::clone(client));
    }
    let secret = smelt_iroh::default_client_secret_path()
        .and_then(|path| smelt_iroh::load_or_create_secret(&path))
        .map_err(|error| {
            t!("runtime.identity_failed", error = format!("{error:#}")).into_owned()
        })?;
    let client = smelt_iroh::runtime::IrohClient::new(secret, None)
        .map_err(|error| t!("runtime.iroh_failed", error = format!("{error:#}")).into_owned())?;
    *slot = Some(std::sync::Arc::clone(&client));
    Ok(client)
}

/// 丢掉这个桌面的 iroh endpoint 和各台旧机器按它拨好的连接器。旧的在最后一份引用放掉时
/// 关闭，通常就在这里：关连接要等 iroh 收尾，阻塞，只在后台调。之前没绑过返回 `false`。
fn reset_iroh_client(routes: &[RemoteRoute]) -> bool {
    // 先清全局这份再清各条路线：反过来的话，夹在中间的拨号会按旧 endpoint 再建一个连接器
    // 留在路线上。就算有拨号赶在这之前拿走了旧的，下一次连接也会认出连接器不是新
    // endpoint 的、重建（见 [`RouteState::connector`]）。
    let old = IROH_CLIENT.lock().unwrap_or_else(|e| e.into_inner()).take();
    if old.is_none() {
        return false;
    }
    for route in routes {
        route.redial();
    }
    drop(old);
    true
}

/// 经哪个中继直连一台旧机器，以及按它拨好的连接器（还没拨过号就是 `None`）。
#[derive(Default)]
struct RouteState {
    relay: Option<smelt_iroh::RelaySettings>,
    connector: Option<std::sync::Arc<smelt_iroh::runtime::RuntimeConnector>>,
}

impl RouteState {
    /// 经 `client` 连 `endpoint` 的连接器：已有的是经它拨的就接着用（同一条 QUIC 连接上
    /// 开流），没有或是经换掉的旧 endpoint 拨的就按眼下的中继新建。
    fn connector(
        &mut self,
        client: &std::sync::Arc<smelt_iroh::runtime::IrohClient>,
        endpoint: smelt_iroh::EndpointId,
    ) -> std::sync::Arc<smelt_iroh::runtime::RuntimeConnector> {
        if let Some(connector) = self.connector.as_ref()
            && connector.uses(client)
        {
            return std::sync::Arc::clone(connector);
        }
        let connector = std::sync::Arc::new(client.connector(endpoint, self.relay.as_ref()));
        self.connector = Some(std::sync::Arc::clone(&connector));
        connector
    }
}

/// 一台旧直连机器的连接路线，它的连接器由开着的终端、对话共用：改了中继，它们下次开流
/// 就走新中继，不用重开。
#[derive(Clone, Default)]
struct RemoteRoute {
    state: std::sync::Arc<std::sync::Mutex<RouteState>>,
}

impl RemoteRoute {
    fn new(relay: Option<smelt_iroh::RelaySettings>) -> Self {
        Self {
            state: std::sync::Arc::new(std::sync::Mutex::new(RouteState {
                relay,
                connector: None,
            })),
        }
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, RouteState> {
        self.state.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// 眼下的连接路径和延迟。还没拨过号就读不到，也不会为了读而去拨号。阻塞，只在后台调。
    fn read(&self) -> Option<RuntimeLink> {
        let connector = self.lock().connector.clone()?;
        connector
            .path_status()
            .map(|path| RuntimeLink::from_path(&path))
    }

    /// 盯着眼下这条连接：iroh 换了选中的路径、连接断了，报到 `events`。还没拨过号就不盯，
    /// 也不为此拨号。阻塞，只在后台调（`watch` 不能在 tokio 运行时里调）。
    fn watch(
        &self,
        runtime_id: &str,
        generation: u64,
        events: &smol::channel::Sender<LinkEvent>,
    ) -> Option<smelt_iroh::runtime::LinkWatch> {
        let connector = self.lock().connector.clone()?;
        let runtime_id = runtime_id.to_string();
        let events = events.clone();
        // 回调在 iroh 的 tokio 线程上：只往通道里放，不阻塞。
        Some(connector.watch(move |change| {
            let _ = events.try_send(LinkEvent {
                runtime_id: runtime_id.clone(),
                generation,
                change: match change {
                    smelt_iroh::runtime::LinkChange::PathSelected => LinkSignal::PathSelected,
                    smelt_iroh::runtime::LinkChange::Closed => LinkSignal::Closed,
                },
            });
        }))
    }

    /// 改走另一个中继：丢掉按旧中继拨的连接器，下一次开流按新的重拨。
    fn retarget(&self, relay: Option<smelt_iroh::RelaySettings>) {
        self.lock().relay = relay;
        self.redial();
    }

    /// 丢掉连接器，下一次开流重拨。
    fn redial(&self) {
        self.lock().connector = None;
    }
}

/// 怎么连一台远程机器。
#[derive(Clone)]
enum Transport {
    /// 本机运行时的对端节点：经本机 smeltd 转发（`op=forward`）。
    Peer,
    /// 还没迁到本机运行时的旧机器：用这个桌面自己的 iroh 身份直连。
    Legacy {
        endpoint: smelt_iroh::EndpointId,
        route: RemoteRoute,
    },
    /// 客户端库里记的地址解析不了。
    Broken(String),
}

/// 一台机器的 [`Transport`]，它的客户端和 hub 里那份共用。
#[derive(Clone)]
struct TransportSlot(std::sync::Arc<std::sync::Mutex<Transport>>);

impl TransportSlot {
    fn new(transport: Transport) -> Self {
        Self(std::sync::Arc::new(std::sync::Mutex::new(transport)))
    }

    fn get(&self) -> Transport {
        self.0.lock().unwrap_or_else(|e| e.into_inner()).clone()
    }

    fn set(&self, transport: Transport) {
        *self.0.lock().unwrap_or_else(|e| e.into_inner()) = transport;
    }

    fn is_peer(&self) -> bool {
        matches!(self.get(), Transport::Peer)
    }

    fn legacy_route(&self) -> Option<RemoteRoute> {
        match self.get() {
            Transport::Legacy { route, .. } => Some(route),
            Transport::Peer | Transport::Broken(_) => None,
        }
    }
}

/// 经本机 smeltd 转发到这个对端。
fn forward_connector(runtime_id: &str) -> smelt_runtime_client::ForwardConnector {
    smelt_runtime_client::ForwardConnector::new(
        smelt_runtime_client::LocalConnector::new(smelt_client::daemon_state::smeltd_sock_path()),
        runtime_id,
    )
}

/// 远程机器的客户端用的连接器：每次连接时按眼下的 [`Transport`] 走。直连要起 tokio 绑
/// iroh，不能在主线程绑，所以第一次连接（总在后台）时才绑；之后复用同一个，同一条 QUIC
/// 连接上开流。
struct SwitchConnector {
    runtime_id: String,
    transport: TransportSlot,
}

impl smelt_runtime_client::Connector for SwitchConnector {
    fn connect(&self) -> std::io::Result<smelt_runtime_client::RuntimeStream> {
        match self.transport.get() {
            Transport::Peer => {
                smelt_runtime_client::Connector::connect(&forward_connector(&self.runtime_id))
            }
            Transport::Legacy { endpoint, route } => legacy_connect(endpoint, &route),
            Transport::Broken(error) => Err(std::io::Error::other(error)),
        }
    }
}

fn legacy_connect(
    endpoint: smelt_iroh::EndpointId,
    route: &RemoteRoute,
) -> std::io::Result<smelt_runtime_client::RuntimeStream> {
    let client = iroh_client().map_err(std::io::Error::other)?;
    let connector = route.lock().connector(&client, endpoint);
    smelt_runtime_client::Connector::connect(connector.as_ref())
}

/// 盯着连接路径的守卫，带着编号：认出守卫换过之后才到的旧事件。旧机器是 iroh 的
/// `LinkWatch`，对端节点是到本机运行时的 `peer_link_watch` 长连接；丢掉就不再盯。
type WatchGuard = Box<dyn Send>;

#[derive(Clone, Default)]
struct LinkWatchSlot(std::sync::Arc<std::sync::Mutex<Option<(u64, WatchGuard)>>>);

impl LinkWatchSlot {
    fn lock(&self) -> std::sync::MutexGuard<'_, Option<(u64, WatchGuard)>> {
        self.0.lock().unwrap_or_else(|e| e.into_inner())
    }

    fn watching(&self) -> bool {
        self.lock().is_some()
    }

    fn set(&self, generation: u64, guard: WatchGuard) {
        *self.lock() = Some((generation, guard));
    }

    fn is_current(&self, generation: u64) -> bool {
        self.lock()
            .as_ref()
            .is_some_and(|(current, _)| *current == generation)
    }

    fn unwatch(&self) {
        *self.lock() = None;
    }

    /// 只摘编号对得上的那只，摘了返回 `true`：换过守卫后，旧守卫迟到的「断了」不能摘掉
    /// 新的。
    fn unwatch_if(&self, generation: u64) -> bool {
        let mut watch = self.lock();
        let current = watch
            .as_ref()
            .is_some_and(|(current, _)| *current == generation);
        if current {
            *watch = None;
        }
        current
    }
}

fn next_watch_generation() -> u64 {
    static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
}

/// 到本机运行时的 `peer_link_watch` 长连接，丢掉就关掉它，读线程随之退出。
struct PeerLinkWatch(smelt_runtime_client::RuntimeStream);

impl Drop for PeerLinkWatch {
    fn drop(&mut self) {
        let _ = self.0.shutdown(std::net::Shutdown::Both);
    }
}

/// 请本机运行时盯着到这个对端的连接：它每报一行路径就送一个 [`LinkSignal::Read`]，连接断了
/// 或本机运行时关了送 [`LinkSignal::Closed`]。阻塞，只在后台调。
fn watch_peer_link(
    runtime_id: &str,
    generation: u64,
    events: &smol::channel::Sender<LinkEvent>,
) -> Option<PeerLinkWatch> {
    use smelt_runtime_client::protocol::daemon::DaemonOperation;
    use std::io::BufRead as _;

    let mut stream = smelt_runtime_client::Connector::connect(
        &smelt_runtime_client::LocalConnector::new(smelt_client::daemon_state::smeltd_sock_path()),
    )
    .ok()?;
    let request = serde_json::json!({
        "op": DaemonOperation::PeerLinkWatch,
        "runtime_id": runtime_id,
    });
    smelt_runtime_client::protocol::write_json_line(&mut stream, &request).ok()?;
    let guard = PeerLinkWatch(stream.try_clone().ok()?);
    let runtime_id = runtime_id.to_string();
    let events = events.clone();
    std::thread::spawn(move || {
        let send = |change| {
            events
                .try_send(LinkEvent {
                    runtime_id: runtime_id.clone(),
                    generation,
                    change,
                })
                .is_ok()
        };
        for line in std::io::BufReader::new(stream).lines() {
            let Ok(line) = line else { break };
            let Ok(result) = serde_json::from_str::<smelt_model::peer_node::PeerLinkResult>(&line)
            else {
                continue;
            };
            if !send(LinkSignal::Read(result.link.map(RuntimeLink::from_peer))) {
                return;
            }
        }
        send(LinkSignal::Closed);
    });
    Some(guard)
}

/// 本机运行时眼下到这个对端的路径，不为此拨号。阻塞，只在后台调。
fn peer_link(runtime_id: &str) -> Option<RuntimeLink> {
    local()
        .call::<smelt_model::control_api::PeerLink>(
            &smelt_model::peer_node::PeerIdParams {
                runtime_id: runtime_id.to_string(),
            },
            smelt_runtime_client::DEFAULT_REQUEST_TIMEOUT,
        )
        .ok()?
        .link
        .map(RuntimeLink::from_peer)
}

/// 一台机器连接上的变化，从读它的线程送到主线程。
struct LinkEvent {
    runtime_id: String,
    /// 报它的那只守卫的编号，见 [`LinkWatchSlot`]。
    generation: u64,
    change: LinkSignal,
}

enum LinkSignal {
    /// iroh 换了选中的路径：重读一次（旧直连机器）。
    PathSelected,
    /// 本机运行时报来的眼下路径（对端节点）。
    Read(Option<RuntimeLink>),
    /// 连接断了，守卫到头了。
    Closed,
}

/// 连接变化送到主线程的通道，见 [`start_link_events`]。
struct LinkEvents(smol::channel::Sender<LinkEvent>);

impl gpui::Global for LinkEvents {}

/// 连着：在线，或者连得上但版本不对。只有这时有连接路径可读。
fn connected(status: &RuntimeStatus) -> bool {
    matches!(
        status,
        RuntimeStatus::Online { .. } | RuntimeStatus::Incompatible { .. }
    )
}

/// 中继地址：空的是 n0 的公共 relay。
fn parse_relay(relay: &str) -> Result<Option<smelt_iroh::RelaySettings>, String> {
    match relay.trim() {
        "" => Ok(None),
        relay => smelt_iroh::RelaySettings::parse(relay)
            .map(Some)
            .map_err(|error| format!("{error:#}")),
    }
}

/// 客户端库里一台旧机器怎么直连。只解析地址，不碰网络，主线程可以调。
fn legacy_transport(connection: &smelt_client_store::RuntimeConnection) -> Transport {
    let endpoint = match connection.endpoint_id.parse::<smelt_iroh::EndpointId>() {
        Ok(endpoint) => endpoint,
        Err(_) => {
            return Transport::Broken(
                t!("runtime.bad_endpoint", id = connection.endpoint_id).into_owned(),
            );
        }
    };
    match parse_relay(connection.relay_url.as_deref().unwrap_or_default()) {
        Ok(relay) => Transport::Legacy {
            endpoint,
            route: RemoteRoute::new(relay),
        },
        Err(error) => Transport::Broken(t!("runtime.bad_relay", error = error).into_owned()),
    }
}

/// 直连一台旧机器的客户端，不经 hub：迁移时用旧身份向它要配对码、撤销旧设备。
fn legacy_client(
    connection: &smelt_client_store::RuntimeConnection,
) -> Result<RuntimeClient, String> {
    match legacy_transport(connection) {
        Transport::Legacy { endpoint, route } => {
            Ok(RuntimeClient::new(move || legacy_connect(endpoint, &route)))
        }
        Transport::Broken(error) => Err(error),
        Transport::Peer => unreachable!("legacy_transport 只给直连"),
    }
}

/// hub 里的一台机器：客户端按 `transport` 连，迁移时就地换。
fn remote_entry(
    connection: smelt_client_store::RuntimeConnection,
    transport: Transport,
    access: Option<PeerAccess>,
) -> RemoteRuntime {
    let status = match &transport {
        Transport::Broken(error) => RuntimeStatus::Offline(error.clone()),
        Transport::Peer | Transport::Legacy { .. } => RuntimeStatus::Connecting,
    };
    let transport = TransportSlot::new(transport);
    let client = RuntimeClient::new(SwitchConnector {
        runtime_id: connection.runtime_id.clone(),
        transport: transport.clone(),
    });
    RemoteRuntime {
        runtime: remote_runtime(&connection.runtime_id, client),
        connection,
        status,
        transport,
        watch: LinkWatchSlot::default(),
        agent_access: access.map(|access| access.agents),
        inbound: access.map(|access| access.inbound),
        link: None,
        upgrade_note: None,
    }
}

/// 对端节点的两项权限，见 [`RemoteRuntime::agent_access`]、[`RemoteRuntime::inbound`]。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct PeerAccess {
    agents: bool,
    inbound: bool,
}

impl PeerAccess {
    fn of(peer: &smelt_model::peer_node::PeerNode) -> Self {
        Self {
            agents: peer.agent_access,
            inbound: peer.inbound,
        }
    }
}

/// 对端节点记成桌面这边的连接形状。
fn peer_connection(
    peer: &smelt_model::peer_node::PeerNode,
) -> smelt_client_store::RuntimeConnection {
    smelt_client_store::RuntimeConnection {
        runtime_id: peer.runtime_id.clone(),
        name: peer.name.clone(),
        endpoint_id: peer.endpoint_id.clone(),
        relay_url: peer.relay_url.clone(),
        os: peer.os.clone(),
        added_at_ms: peer.added_at_ms,
    }
}

/// 本机守护：没在跑就按 App 里的版本拉起（运行中退出了也靠这里补上），再对版本。
/// 阻塞，只在后台调。
pub(crate) fn local_status(client: &RuntimeClient) -> RuntimeStatus {
    crate::local_daemon::ensure_daemon_running();
    status_of(client)
}

/// 问一次运行时并对版本。阻塞，只在后台调。
pub(crate) fn status_of(client: &RuntimeClient) -> RuntimeStatus {
    match client.describe() {
        Ok(described) => described_status(described),
        Err(error) => RuntimeStatus::Offline(error.to_string()),
    }
}

fn incompatible_status(
    incompatible: &smelt_runtime_client::compat::Incompatibility,
    os: Option<&str>,
) -> RuntimeStatus {
    use smelt_runtime_client::compat::Outdated;

    let fix = match incompatible.outdated {
        Outdated::Client => IncompatibleFix::UpdateDesktop,
        Outdated::Runtime if upgrades_remotely(os) => IncompatibleFix::UpgradeRuntime,
        Outdated::Runtime => IncompatibleFix::UpdateSmeltThere,
    };
    RuntimeStatus::Incompatible {
        version: incompatible.runtime_version.clone(),
        detail: incompatible.to_string(),
        fix,
    }
}

/// 这个桌面出自的发版：请远程运行时升级时就升到它。tag 之后的构建，完整版本号带构建信息（见
/// `smelt_version`），升级要的是 GitHub 上真有的那个 Release。开发构建没有对应的发版，这里
/// 退回完整版本号只为显示，[`push_upgrade`] 不拿它去请运行时升级。
pub(crate) const DESKTOP_VERSION: &str = match smelt_version::RELEASE {
    Some(release) => release,
    None => smelt_version::VERSION,
};

/// 请运行时升级（[`push_upgrade`]）的结局。
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum PushUpgrade {
    /// 运行时接下后断开了：多半是停机重启到了新版本，也可能是中途断了网。
    /// 重新探测才知道。
    Restarting,
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
    match smelt_version::RELEASE {
        Some(release) => push_upgrade_to(client, release),
        None => PushUpgrade::Failed(t!("upgrade.dev_build").into_owned()),
    }
}

fn push_upgrade_to(client: &RuntimeClient, version: &str) -> PushUpgrade {
    use smelt_runtime_client::protocol::daemon::DaemonOperation;
    use smelt_runtime_client::protocol::runtime_upgrade::RuntimeUpgradeReply;
    use std::io::BufRead as _;

    let mut stream = match client.connect() {
        Ok(stream) => stream,
        Err(error) => {
            return PushUpgrade::Failed(t!("upgrade.connect_failed", error = error).into_owned());
        }
    };
    let request = serde_json::json!({
        "op": DaemonOperation::RuntimeUpgrade,
        "version": version,
    });
    if let Err(error) = smelt_runtime_client::protocol::write_json_line(&mut stream, &request) {
        return PushUpgrade::Failed(t!("upgrade.send_failed", error = error).into_owned());
    }
    let _ = stream.set_read_timeout(Some(PUSH_UPGRADE_TIMEOUT));
    let mut reader = std::io::BufReader::new(stream);
    let mut accepted = false;
    loop {
        let mut line = String::new();
        match reader.read_line(&mut line) {
            Ok(0) if accepted => return PushUpgrade::Restarting,
            Ok(0) => return PushUpgrade::Unsupported,
            Ok(_) => {}
            Err(error) if accepted => {
                return PushUpgrade::Failed(t!("upgrade.timeout", error = error).into_owned());
            }
            Err(error) => {
                return PushUpgrade::Failed(t!("upgrade.read_failed", error = error).into_owned());
            }
        }
        match serde_json::from_str::<RuntimeUpgradeReply>(line.trim()) {
            Ok(RuntimeUpgradeReply::Accepted) => accepted = true,
            Ok(RuntimeUpgradeReply::Current) => return PushUpgrade::Current,
            Ok(RuntimeUpgradeReply::Staged { reason }) => return PushUpgrade::Staged(reason),
            Ok(RuntimeUpgradeReply::Failed { error }) => return PushUpgrade::Failed(error),
            Err(_) => {
                return PushUpgrade::Failed(
                    t!("upgrade.bad_reply", reply = line.trim()).into_owned(),
                );
            }
        }
    }
}

/// 运行时太旧、不会自己升级时，到那台机器上执行的命令：装这个桌面的版本。脚本也取这个
/// 版本的 Release 里的：latest 只指向正式版，预发布期间可能是一个还没有 install.sh 的旧版。
pub(crate) fn manual_upgrade_command() -> String {
    format!(
        "curl -fsSL https://github.com/smelt-ai/smelt/releases/download/v{DESKTOP_VERSION}/install.sh | SMELT_VERSION={DESKTOP_VERSION} bash"
    )
}

fn described_status(
    described: smelt_runtime_client::protocol::control::SystemDescribeResult,
) -> RuntimeStatus {
    if let Err(incompatible) = smelt_runtime_client::compat::check(&described) {
        return incompatible_status(&incompatible, described.daemon.os.as_deref());
    }
    RuntimeStatus::Online {
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
    sync_link_watch(key, &status, cx);
    if matches!(key, RuntimeKey::Remote(_)) && matches!(status, RuntimeStatus::Online { .. }) {
        maybe_migrate(key, cx);
    }
    let changed = cx.try_global::<RuntimeHub>().is_some_and(|hub| match key {
        RuntimeKey::Local => hub.local_status.as_ref() != Some(&status),
        RuntimeKey::Remote(_) => hub
            .remotes
            .iter()
            .any(|remote| remote.runtime.key() == key && remote.status != status),
    });
    if !changed {
        return;
    }
    let mut learned = None;
    update_hub(cx, |hub| match key {
        RuntimeKey::Local => hub.local_status = Some(status),
        RuntimeKey::Remote(_) => {
            if let Some(remote) = hub
                .remotes
                .iter_mut()
                .find(|remote| remote.runtime.key() == key)
            {
                // 所在系统记在机器上并落库：离线时、下次启动还没连上时也要切得对它的路径。
                if let RuntimeStatus::Online { platform, .. } = &status
                    && let Some(os) = platform
                        .as_deref()
                        .and_then(|platform| platform.split('/').next())
                    && remote.connection.os.as_deref() != Some(os)
                {
                    remote.connection.os = Some(os.to_string());
                    learned = Some((remote.connection.clone(), remote.transport.is_peer()));
                }
                if !connected(&status) {
                    remote.link = None;
                }
                remote.status = status;
            }
        }
    });
    if let Some((connection, peer)) = learned {
        save_machine(connection, peer, "保存机器的系统", cx);
    }
}

/// 记下一台机器在这边的名字、系统：对端节点记在本机运行时，旧直连机器记在客户端库。后台写。
fn save_machine(
    connection: smelt_client_store::RuntimeConnection,
    peer: bool,
    what: &'static str,
    cx: &gpui::App,
) {
    cx.background_executor()
        .spawn(async move {
            let saved = if peer {
                local()
                    .call::<smelt_model::control_api::PeerUpdate>(
                        &smelt_model::peer_node::PeerUpdateParams {
                            runtime_id: connection.runtime_id.clone(),
                            name: Some(connection.name.clone()),
                            os: connection.os.clone(),
                            ..Default::default()
                        },
                        smelt_runtime_client::DEFAULT_REQUEST_TIMEOUT,
                    )
                    .map(|_| ())
                    .map_err(|error| error.to_string())
            } else {
                smelt_client_store::client_store()
                    .and_then(|store| store.put_runtime_connection(&connection))
                    .map_err(|error| error.to_string())
            };
            if let Err(error) = saved {
                eprintln!("[runtimes] {what}失败：{error}");
            }
        })
        .detach();
}

fn update_hub(cx: &mut gpui::App, change: impl FnOnce(&mut RuntimeHub)) {
    let mut hub = cx.try_global::<RuntimeHub>().cloned().unwrap_or_default();
    change(&mut hub);
    cx.set_global(hub);
}

/// 启动时最多等本机运行时这么久交出对端节点：本机 socket，守护在跑就是几毫秒；没在跑
/// 是立刻连不上，不用等。
const STARTUP_PEERS_TIMEOUT: std::time::Duration = std::time::Duration::from_millis(1500);

/// 启动时读出登记过的远程机器：本机运行时的对端节点和客户端库里还没迁的旧机器。都同步读
/// （都在本机），主窗口一起来插件就读得到它们的连接。本机运行时这会儿读不到（还没拉起、
/// 是旧版本）的，连上之后再读（见 [`peers_changed`]）。再起连接变化的处理循环。连通性由
/// 订阅线程报（见 `runtime_events`）。
pub(crate) fn start_runtime_hub(cx: &mut gpui::App) {
    init_local_runtime_name(cx);
    let mut hub = RuntimeHub::default();
    match smelt_client_store::client_store().and_then(|store| store.runtime_connections()) {
        Ok(connections) => {
            hub.remotes = connections
                .into_iter()
                .map(|connection| {
                    let transport = legacy_transport(&connection);
                    remote_entry(connection, transport, None)
                })
                .collect();
        }
        Err(error) => hub.load_error = Some(t!("runtime.load_failed", error = error).into_owned()),
    }
    if let Ok(peers) = load_peers(STARTUP_PEERS_TIMEOUT) {
        let migrated = merge_peers(&mut hub, peers);
        forget_legacy(migrated, cx);
    }
    cx.set_global(hub);
    start_link_events(cx);
    start_peer_changes(cx);
}

/// 本机运行时的对端节点。阻塞。
fn load_peers(
    timeout: std::time::Duration,
) -> Result<Vec<smelt_model::peer_node::PeerNode>, String> {
    local()
        .call::<smelt_model::control_api::PeerList>(&Default::default(), timeout)
        .map(|list| list.peers)
        .map_err(|error| error.to_string())
}

/// 按本机运行时的对端表更新 hub：新的加上，旧直连机器已经是对端的就地换成经本机转发，
/// 不再是对端的移走。交回换过来的旧机器（客户端库里那一行该删了）。
fn merge_peers(hub: &mut RuntimeHub, peers: Vec<smelt_model::peer_node::PeerNode>) -> Vec<String> {
    let mut migrated = Vec::new();
    hub.remotes.retain(|remote| {
        !remote.transport.is_peer()
            || peers
                .iter()
                .any(|peer| peer.runtime_id == remote.connection.runtime_id)
    });
    for peer in peers {
        let connection = peer_connection(&peer);
        match hub
            .remotes
            .iter_mut()
            .find(|remote| remote.connection.runtime_id == peer.runtime_id)
        {
            Some(remote) => {
                if !remote.transport.is_peer() {
                    remote.transport.set(Transport::Peer);
                    remote.watch.unwatch();
                    remote.link = None;
                    migrated.push(peer.runtime_id.clone());
                }
                remote.connection = connection;
                remote.agent_access = Some(peer.agent_access);
                remote.inbound = Some(peer.inbound);
            }
            None => hub.remotes.push(remote_entry(
                connection,
                Transport::Peer,
                Some(PeerAccess::of(&peer)),
            )),
        }
    }
    hub.peers_loaded = true;
    migrated
}

/// 迁成对端节点的旧机器：从客户端库删掉。没有旧机器了就连这个桌面的 iroh endpoint 一起
/// 关掉。后台做。
fn forget_legacy(runtime_ids: Vec<String>, cx: &mut gpui::App) {
    if runtime_ids.is_empty() {
        return;
    }
    cx.background_executor()
        .spawn(async move {
            for runtime_id in runtime_ids {
                if let Err(error) = smelt_client_store::client_store()
                    .and_then(|store| store.delete_runtime_connection(&runtime_id))
                {
                    eprintln!("[runtimes] 删掉已迁移的旧机器失败：{error}");
                }
            }
        })
        .detach();
    release_iroh_if_unused(cx);
}

/// 没有旧直连机器了：关掉这个桌面自己的 iroh endpoint（它一直挂着 relay 心跳）。后台关。
fn release_iroh_if_unused(cx: &mut gpui::App) {
    let legacy_left = cx.try_global::<RuntimeHub>().is_some_and(|hub| {
        hub.remotes
            .iter()
            .any(|remote| remote.transport.legacy_route().is_some())
    });
    if !legacy_left {
        cx.background_executor()
            .spawn(async { reset_iroh_client(&[]) })
            .detach();
    }
}

/// 本机运行时的对端变了（或刚连上它）就重读一遍，见 [`peers_changed`]。
static PEER_CHANGES: std::sync::OnceLock<smol::channel::Sender<()>> = std::sync::OnceLock::new();

/// 请主线程重读本机运行时的对端节点：本机运行时推来 `peers.changed`、或刚连上它时调。
/// 哪个线程都能调，连着来的几次并成一次。
pub(crate) fn peers_changed() {
    if let Some(changes) = PEER_CHANGES.get() {
        let _ = changes.try_send(());
    }
}

fn start_peer_changes(cx: &mut gpui::App) {
    let (changes, received) = smol::channel::bounded::<()>(1);
    if PEER_CHANGES.set(changes).is_err() {
        return;
    }
    cx.spawn(async move |cx| {
        while received.recv().await.is_ok() {
            let peers = cx
                .background_executor()
                .spawn(async { load_peers(smelt_runtime_client::DEFAULT_REQUEST_TIMEOUT) })
                .await;
            let Ok(peers) = peers else {
                continue;
            };
            cx.update(|cx| {
                let mut migrated = Vec::new();
                update_hub(cx, |hub| migrated = merge_peers(hub, peers));
                forget_legacy(migrated, cx);
                // 刚读到对端表：连着的旧机器这就可以迁了。
                let online: Vec<RuntimeKey> = cx
                    .try_global::<RuntimeHub>()
                    .map(|hub| {
                        hub.remotes
                            .iter()
                            .filter(|remote| matches!(remote.status, RuntimeStatus::Online { .. }))
                            .map(|remote| remote.runtime.key().clone())
                            .collect()
                    })
                    .unwrap_or_default();
                for key in online {
                    maybe_migrate(&key, cx);
                }
            });
        }
    })
    .detach();
}

/// 正在迁的旧机器，同一台不同时迁两次。
static MIGRATING: std::sync::Mutex<Option<std::collections::HashSet<String>>> =
    std::sync::Mutex::new(None);

/// 连上的旧直连机器，本机运行时认得对端节点了，就迁过去（见 [`migrate`]）。
fn maybe_migrate(key: &RuntimeKey, cx: &mut gpui::App) {
    let Some(hub) = cx.try_global::<RuntimeHub>() else {
        return;
    };
    if !hub.peers_loaded {
        return;
    }
    let Some(remote) = hub
        .remotes
        .iter()
        .find(|remote| remote.runtime.key() == key)
    else {
        return;
    };
    if remote.transport.legacy_route().is_none() {
        return;
    }
    let connection = remote.connection.clone();
    {
        let mut migrating = MIGRATING.lock().unwrap_or_else(|e| e.into_inner());
        if !migrating
            .get_or_insert_with(Default::default)
            .insert(connection.runtime_id.clone())
        {
            return;
        }
    }
    let key = key.clone();
    cx.spawn(async move |cx| {
        let runtime_id = connection.runtime_id.clone();
        let migrated = cx
            .background_executor()
            .spawn({
                let connection = connection.clone();
                async move { migrate(&connection) }
            })
            .await;
        match migrated {
            Ok(Some(peer)) => {
                smelt_sys::app_log::log(
                    "info",
                    "runtimes",
                    &format!("{} 已迁到本机运行时", connection.name),
                );
                cx.update(|cx| {
                    let mut moved = Vec::new();
                    update_hub(cx, |hub| {
                        let mut peers: Vec<_> = Vec::new();
                        for remote in &hub.remotes {
                            if remote.transport.is_peer() {
                                peers.push(smelt_model::peer_node::PeerNode {
                                    runtime_id: remote.connection.runtime_id.clone(),
                                    endpoint_id: remote.connection.endpoint_id.clone(),
                                    name: remote.connection.name.clone(),
                                    relay_url: remote.connection.relay_url.clone(),
                                    os: remote.connection.os.clone(),
                                    agent_access: remote.agent_access.unwrap_or_default(),
                                    inbound: remote.inbound.unwrap_or_default(),
                                    added_at_ms: remote.connection.added_at_ms,
                                });
                            }
                        }
                        peers.push(peer);
                        moved = merge_peers(hub, peers);
                    });
                    // 旧身份的连接随撤销断开，订阅按断线重连，接上的就是本机运行时转发的。
                    crate::runtime_events::reconnect(&key, cx);
                    forget_legacy(moved, cx);
                });
                // 撤销旧身份：那台机器上不再留这个桌面原来的设备。连不上就算了，那边的设备
                // 列表里留着一条用不上的。
                cx.background_executor()
                    .spawn(async move { revoke_legacy(&connection) })
                    .await;
                cx.update(release_iroh_if_unused);
            }
            // 它还不认节点配对，等它升级。
            Ok(None) => {}
            Err(error) => smelt_sys::app_log::log(
                "warn",
                "runtimes",
                &format!("{} 迁到本机运行时失败：{error}", connection.name),
            ),
        }
        MIGRATING
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get_or_insert_with(Default::default)
            .remove(&runtime_id);
    })
    .detach();
}

/// 把一台旧直连机器迁成本机运行时的对端节点：用这个桌面的旧身份请它发一个配对码，交给
/// 本机运行时去双向配对，再把这边的名字、系统带过去。它还不认节点配对（旧版本）是
/// `Ok(None)`。阻塞，只在后台调。
fn migrate(
    connection: &smelt_client_store::RuntimeConnection,
) -> Result<Option<smelt_model::peer_node::PeerNode>, String> {
    use smelt_model::control_api::{DevicePairBegin, PeerPair, PeerUpdate};
    use smelt_runtime_client::protocol::control::ControlMethod;

    let legacy = legacy_client(connection)?;
    let timeout = smelt_runtime_client::DEFAULT_REQUEST_TIMEOUT;
    let described = legacy.describe().map_err(|error| error.to_string())?;
    if !described
        .control
        .methods
        .iter()
        .any(|method| method == ControlMethod::NodePair.as_str())
    {
        return Ok(None);
    }
    let offer = legacy
        .call::<DevicePairBegin>(&Default::default(), timeout)
        .map_err(|error| error.to_string())?;
    let local = local();
    let peer = local
        .call::<PeerPair>(
            &smelt_model::peer_node::PeerPairParams {
                pairing_uri: offer.pairing_uri,
            },
            LOCAL_PAIR_TIMEOUT,
        )
        .map_err(|error| error.to_string())?;
    if peer.runtime_id != connection.runtime_id {
        return Err(format!(
            "paired with {} instead of {}",
            peer.runtime_id, connection.runtime_id
        ));
    }
    local
        .call::<PeerUpdate>(
            &smelt_model::peer_node::PeerUpdateParams {
                runtime_id: peer.runtime_id.clone(),
                name: Some(connection.name.clone()),
                os: connection.os.clone(),
                ..Default::default()
            },
            timeout,
        )
        .map(Some)
        .map_err(|error| error.to_string())
}

/// 用旧身份请那台机器撤销这个桌面原来的设备。阻塞，只在后台调。
fn revoke_legacy(connection: &smelt_client_store::RuntimeConnection) {
    let (Ok(legacy), Ok(me)) = (legacy_client(connection), iroh_client()) else {
        return;
    };
    let _ = legacy.call::<smelt_model::control_api::DeviceRevoke>(
        &smelt_model::remote_config::DeviceIdParams {
            endpoint_id: me.id().to_string(),
        },
        smelt_runtime_client::DEFAULT_REQUEST_TIMEOUT,
    );
}

/// 起连接变化的处理循环：路径换了就重读或记下那台机器的路径和延迟，断了就清掉。只在有
/// 事件时醒。
fn start_link_events(cx: &mut gpui::App) {
    let (events, received) = smol::channel::unbounded::<LinkEvent>();
    cx.set_global(LinkEvents(events));
    cx.spawn(async move |cx| {
        while let Ok(event) = received.recv().await {
            cx.update(|cx| on_link_event(event, cx));
        }
    })
    .detach();
}

fn on_link_event(event: LinkEvent, cx: &mut gpui::App) {
    let Some(remote) = remote_by_id(&event.runtime_id, cx) else {
        return;
    };
    let watch = remote.watch.clone();
    let transport = remote.transport.get();
    match event.change {
        LinkSignal::PathSelected => {
            if watch.is_current(event.generation) {
                sample(vec![(event.runtime_id, transport)], cx);
            }
        }
        LinkSignal::Read(link) => {
            if watch.is_current(event.generation) {
                apply_links(vec![(event.runtime_id, link)], cx);
            }
        }
        // 连接断了，守卫也就到头了：摘掉，路径清掉。重拨后状态会先变成离线再变回来，
        // 那时重新装上（见 `sync_link_watch`）。
        LinkSignal::Closed => {
            if watch.unwatch_if(event.generation) {
                apply_links(vec![(event.runtime_id, None)], cx);
            }
        }
    }
}

/// 连着（在线，或者连得上但版本不对）就盯着这条连接的路径，还没盯就装上，装上时读一次；
/// 断了就摘掉。连接断了又重拨时状态总会先变成离线再变回来，这里跟着重装。
fn sync_link_watch(key: &RuntimeKey, status: &RuntimeStatus, cx: &mut gpui::App) {
    let Some(remote) = remote_of(key, cx) else {
        return;
    };
    let watch = remote.watch.clone();
    let transport = remote.transport.get();
    let runtime_id = remote.connection.runtime_id.clone();
    if !connected(status) {
        watch.unwatch();
        return;
    }
    if watch.watching() {
        return;
    }
    let Some(events) = cx.try_global::<LinkEvents>().map(|events| events.0.clone()) else {
        return;
    };
    cx.spawn(async move |cx| {
        let watched_id = runtime_id.clone();
        let link = cx
            .background_executor()
            .spawn(async move {
                let generation = next_watch_generation();
                match transport {
                    Transport::Legacy { route, .. } => {
                        if let Some(guard) = route.watch(&watched_id, generation, &events) {
                            watch.set(generation, Box::new(guard));
                        }
                        // 选路可能在装上之前就定了，装上后先读一次。
                        Some(route.read())
                    }
                    // 本机运行时先报一行眼下的路径。
                    Transport::Peer => {
                        if let Some(guard) = watch_peer_link(&watched_id, generation, &events) {
                            watch.set(generation, Box::new(guard));
                        }
                        None
                    }
                    Transport::Broken(_) => None,
                }
            })
            .await;
        if let Some(link) = link {
            cx.update(|cx| apply_links(vec![(runtime_id, link)], cx));
        }
    })
    .detach();
}

/// 现在就重读各台连着的机器的连接路径和延迟：设置停在机器页时调。延迟只在有人看时采样，
/// 平时只在换了路径时读。
pub(crate) fn sample_links(cx: &mut gpui::App) {
    let targets: Vec<_> = cx
        .try_global::<RuntimeHub>()
        .map(|hub| {
            hub.remotes
                .iter()
                .filter(|remote| connected(&remote.status))
                .map(|remote| (remote.connection.runtime_id.clone(), remote.transport.get()))
                .collect()
        })
        .unwrap_or_default();
    sample(targets, cx);
}

/// 后台读这几台机器的路径和延迟（不拨号），读完记到 hub 上。
fn sample(targets: Vec<(String, Transport)>, cx: &mut gpui::App) {
    if targets.is_empty() {
        return;
    }
    cx.spawn(async move |cx| {
        let links = cx
            .background_executor()
            .spawn(async move {
                targets
                    .into_iter()
                    .map(|(runtime_id, transport)| {
                        let link = match transport {
                            Transport::Legacy { route, .. } => route.read(),
                            Transport::Peer => peer_link(&runtime_id),
                            Transport::Broken(_) => None,
                        };
                        (runtime_id, link)
                    })
                    .collect::<Vec<_>>()
            })
            .await;
        cx.update(|cx| apply_links(links, cx));
    })
    .detach();
}

/// 把读到的连接路径记到各台机器上。没连着的不记（读回来时它可能已经断了）；没变化就不动
/// hub，免得看着连接表的插件跟着重画。
fn apply_links(links: Vec<(String, Option<RuntimeLink>)>, cx: &mut gpui::App) {
    let changed = cx.try_global::<RuntimeHub>().is_some_and(|hub| {
        links.iter().any(|(runtime_id, link)| {
            hub.remotes.iter().any(|remote| {
                remote.connection.runtime_id == *runtime_id
                    && link_changed(
                        remote.link.as_ref(),
                        link.as_ref().filter(|_| connected(&remote.status)),
                    )
            })
        })
    });
    if !changed {
        return;
    }
    update_hub(cx, |hub| {
        for (runtime_id, link) in links {
            if let Some(remote) = hub
                .remotes
                .iter_mut()
                .find(|remote| remote.connection.runtime_id == runtime_id)
            {
                let link = link.filter(|_| connected(&remote.status));
                if link_changed(remote.link.as_ref(), link.as_ref()) {
                    remote.link = link;
                }
            }
        }
    });
}

fn remote_by_id<'a>(runtime_id: &str, cx: &'a gpui::App) -> Option<&'a RemoteRuntime> {
    cx.try_global::<RuntimeHub>()?
        .remotes
        .iter()
        .find(|remote| remote.connection.runtime_id == runtime_id)
}

/// 用配对串添加一台远程机器，交回它的 key 和名字。本机运行时认得对端节点时交给它双向
/// 配对；还是旧版本（会话在跑、还没换守护）时照旧用这个桌面自己的身份直连配对，等本机
/// 运行时换上新版本后自动迁过去。同一台机器重新配对就更新原来那条。
pub(crate) fn add_runtime(
    uri: String,
    cx: &mut gpui::App,
) -> gpui::Task<Result<(RuntimeKey, String), String>> {
    let uri = uri.trim().to_string();
    if uri.is_empty() {
        return gpui::Task::ready(Err(t!("pair.empty").into_owned()));
    }
    if let Err(error) = smelt_pairing::parse_pairing_uri(&uri) {
        use smelt_pairing::PairingError;
        return gpui::Task::ready(Err(match error {
            PairingError::NotPairing => t!("pair.not_pairing_link"),
            PairingError::MissingEndpoint => t!("pair.missing_endpoint"),
            PairingError::MissingCode => t!("pair.missing_code"),
        }
        .into_owned()));
    }
    let peers = cx
        .try_global::<RuntimeHub>()
        .is_some_and(|hub| hub.peers_loaded);
    cx.spawn(async move |cx| {
        let remote = cx
            .background_executor()
            .spawn(async move {
                if peers {
                    pair_peer(&uri)
                } else {
                    pair_legacy(&uri)
                }
            })
            .await?;
        let added = (remote.runtime.key().clone(), remote.connection.name.clone());
        cx.update(|cx| {
            update_hub(cx, |hub| {
                hub.remotes
                    .retain(|known| known.connection.runtime_id != remote.connection.runtime_id);
                hub.remotes.push(remote);
            });
        });
        Ok(added)
    })
}

/// 本机运行时配对要它自己拨号（最长 30 秒），这边多等一些。
const LOCAL_PAIR_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(40);

/// 请本机运行时用配对串双向配对，再经它问一次那台机器的状态。阻塞，只在后台调。
fn pair_peer(uri: &str) -> Result<RemoteRuntime, String> {
    let peer = local()
        .call::<smelt_model::control_api::PeerPair>(
            &smelt_model::peer_node::PeerPairParams {
                pairing_uri: uri.to_string(),
            },
            LOCAL_PAIR_TIMEOUT,
        )
        .map_err(|error| t!("pair.failed", error = error).into_owned())?;
    let mut remote = remote_entry(
        peer_connection(&peer),
        Transport::Peer,
        Some(PeerAccess::of(&peer)),
    );
    remote.status = status_of(remote.runtime.client());
    Ok(remote)
}

/// 用这个桌面自己的 iroh 身份直连配对，记进客户端库。阻塞，只在后台调。
pub(crate) fn pair_legacy(uri: &str) -> Result<RemoteRuntime, String> {
    use smelt_model::control_api::DevicePair;
    use smelt_model::remote_config::PairParams;

    let pairing = smelt_pairing::parse_pairing_uri(uri).map_err(|error| error.to_string())?;
    let mut connection = smelt_client_store::RuntimeConnection {
        runtime_id: String::new(),
        name: String::new(),
        endpoint_id: pairing.endpoint_id,
        relay_url: pairing.relay_url,
        os: None,
        added_at_ms: std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|elapsed| elapsed.as_millis() as i64)
            .unwrap_or_default(),
    };
    let client = legacy_client(&connection)?;
    client
        .call::<DevicePair>(
            &PairParams {
                code: pairing.code,
                name: smelt_sys::hostname()
                    .unwrap_or_else(|| t!("pair.default_device_name").into_owned()),
            },
            PAIR_TIMEOUT,
        )
        .map_err(|error| t!("pair.failed", error = error).into_owned())?;
    let described = client
        .describe()
        .map_err(|error| t!("pair.describe_failed", error = error).into_owned())?;
    connection.runtime_id = described
        .daemon
        .runtime_id
        .clone()
        .ok_or_else(|| t!("pair.too_old").into_owned())?;
    connection.name = described
        .daemon
        .hostname
        .clone()
        .unwrap_or_else(|| connection.endpoint_id.chars().take(8).collect());
    connection.os = described.daemon.os.clone();
    smelt_client_store::client_store()
        .and_then(|store| store.put_runtime_connection(&connection))
        .map_err(|error| t!("pair.save_failed", error = error).into_owned())?;
    let transport = legacy_transport(&connection);
    let mut remote = remote_entry(connection, transport, None);
    remote.status = described_status(described);
    Ok(remote)
}

/// 改一台机器在这台电脑上的显示名：先改 hub，看着连接表的插件立刻换上；在后台写下来。
pub(crate) fn rename_runtime(runtime_id: String, name: String, cx: &mut gpui::App) {
    let mut renamed = None;
    update_hub(cx, |hub| {
        if let Some(remote) = hub
            .remotes
            .iter_mut()
            .find(|remote| remote.connection.runtime_id == runtime_id)
        {
            remote.connection.name = name;
            renamed = Some((remote.connection.clone(), remote.transport.is_peer()));
        }
    });
    if let Some((connection, peer)) = renamed {
        save_machine(connection, peer, "保存机器名字", cx);
    }
}

/// 改一台机器的权限，只改给了的：本机智能体能不能经本机运行时用它（`agents`），它能不能
/// 连进来操作这台电脑（`inbound`）。只有对端节点有这两项设置。
pub(crate) fn set_machine_access(
    runtime_id: String,
    agents: Option<bool>,
    inbound: Option<bool>,
    cx: &mut gpui::App,
) -> gpui::Task<Result<(), String>> {
    let peer = remote_by_id(&runtime_id, cx).is_some_and(|remote| remote.transport.is_peer());
    if !peer {
        return gpui::Task::ready(Err(t!("runtime.not_migrated").into_owned()));
    }
    cx.spawn(async move |cx| {
        let updated = cx
            .background_executor()
            .spawn({
                let runtime_id = runtime_id.clone();
                async move {
                    local()
                        .call::<smelt_model::control_api::PeerUpdate>(
                            &smelt_model::peer_node::PeerUpdateParams {
                                runtime_id,
                                agent_access: agents,
                                inbound,
                                ..Default::default()
                            },
                            smelt_runtime_client::DEFAULT_REQUEST_TIMEOUT,
                        )
                        .map_err(|error| error.to_string())
                }
            })
            .await?;
        cx.update(|cx| {
            update_hub(cx, |hub| {
                if let Some(remote) = hub
                    .remotes
                    .iter_mut()
                    .find(|remote| remote.connection.runtime_id == runtime_id)
                {
                    remote.agent_access = Some(updated.agent_access);
                    remote.inbound = Some(updated.inbound);
                }
            });
        });
        Ok(())
    })
}

/// 最近一次探测是在线。
pub(crate) fn runtime_online(key: &RuntimeKey, cx: &gpui::App) -> bool {
    status_of_key(key, cx).is_some_and(|status| matches!(status, RuntimeStatus::Online { .. }))
}

/// 这边记下的中继改成 `relay`：对端节点改本机运行时里的，旧机器改客户端库里的。阻塞。
fn save_relay(
    connection: &smelt_client_store::RuntimeConnection,
    peer: bool,
) -> Result<(), String> {
    if peer {
        local()
            .call::<smelt_model::control_api::PeerUpdate>(
                &smelt_model::peer_node::PeerUpdateParams {
                    runtime_id: connection.runtime_id.clone(),
                    relay_url: Some(connection.relay_url.clone().unwrap_or_default()),
                    ..Default::default()
                },
                smelt_runtime_client::DEFAULT_REQUEST_TIMEOUT,
            )
            .map(|_| ())
            .map_err(|error| error.to_string())
    } else {
        smelt_client_store::client_store()
            .and_then(|store| store.put_runtime_connection(connection))
            .map_err(|error| error.to_string())
    }
}

/// 改一台远程机器的中继，`relay` 空着就是 n0 的公共 relay。
///
/// 在线：先请它自己改用新中继（它的远程配置），成了再改这边记下的地址。顺序不能反：先改
/// 这边就连不上它，没法再请它改。它换中继会关掉回复要走的那条连接，回复常常丢：丢了就
/// 按新中继连过去读它的配置，确认改了再记（见 [`RelaySetReply`]）。
///
/// 不在线：只改这边记下的——那台机器多半已经在那边改过（`smeltd relay`），这边跟上才
/// 连得回去。改完立刻重连，开着的终端和对话下次开流就走新中继。
pub(crate) fn set_runtime_relay(
    runtime_id: String,
    relay: String,
    cx: &mut gpui::App,
) -> gpui::Task<Result<(), String>> {
    use smelt_model::control_api::{RemoteConfigGet, RemoteConfigSet};
    use smelt_runtime_client::protocol::control::EmptyParams;

    let key = RuntimeKey::Remote(runtime_id);
    let Some(remote) = remote_of(&key, cx).cloned() else {
        return gpui::Task::ready(Err(t!("runtime.removed").into_owned()));
    };
    let relay = relay.trim().to_string();
    let parsed = match parse_relay(&relay) {
        Ok(parsed) => parsed,
        Err(error) => return gpui::Task::ready(Err(error)),
    };
    let online = matches!(remote.status, RuntimeStatus::Online { .. });
    let peer = remote.transport.is_peer();
    let route = remote.transport.legacy_route();
    let previous = remote.connection.clone();
    let mut connection = remote.connection.clone();
    connection.relay_url = (!relay.is_empty()).then(|| relay.clone());
    cx.spawn(async move |cx| {
        let saved = connection.clone();
        cx.background_executor()
            .spawn(async move {
                if online {
                    let client = remote.runtime.client();
                    let timeout = smelt_runtime_client::DEFAULT_REQUEST_TIMEOUT;
                    let mut config = client
                        .call::<RemoteConfigGet>(&EmptyParams::default(), timeout)
                        .map_err(|error| t!("relay.read_failed", error = error).into_owned())?;
                    if config.iroh_relay != relay {
                        config.iroh_relay = relay.clone();
                        match relay_set_reply(client.call::<RemoteConfigSet>(&config, timeout)) {
                            RelaySetReply::Applied => {}
                            RelaySetReply::Rejected(error) => {
                                return Err(t!("relay.rejected", error = error).into_owned());
                            }
                            // 改没改成以它眼下的配置为准：按新中继连过去问一次。它要重绑
                            // 新中继，拨号由 iroh 的拨号时限兜着，不另外等。对端节点要先让
                            // 本机运行时改走新中继，没改成再改回去。
                            RelaySetReply::Lost(error) => {
                                let confirm = if peer {
                                    save_relay(&saved, true).map_err(|save_error| {
                                        t!("relay.save_failed", error = save_error).into_owned()
                                    })?;
                                    client.clone()
                                } else {
                                    legacy_client(&saved)?
                                };
                                let now = confirm
                                    .call::<RemoteConfigGet>(&EmptyParams::default(), timeout)
                                    .map_err(|confirm_error| {
                                        t!(
                                            "relay.lost",
                                            error = error,
                                            confirm_error = confirm_error
                                        )
                                        .into_owned()
                                    });
                                let changed = now.is_ok_and(|now| now.iroh_relay == relay);
                                if !changed {
                                    if peer {
                                        let _ = save_relay(&previous, true);
                                    }
                                    return Err(t!("relay.rejected", error = error).into_owned());
                                }
                            }
                        }
                    }
                }
                save_relay(&saved, peer)
                    .map_err(|error| t!("relay.save_failed", error = error).into_owned())
            })
            .await?;
        cx.update(|cx| {
            if let Some(route) = route {
                route.retarget(parsed);
            }
            update_hub(cx, |hub| {
                if let Some(remote) = hub
                    .remotes
                    .iter_mut()
                    .find(|remote| remote.connection.runtime_id == connection.runtime_id)
                {
                    remote.connection.relay_url = connection.relay_url;
                    remote.link = None;
                }
            });
            crate::runtime_events::reconnect(&key, cx);
        });
        Ok(())
    })
}

/// 请远程机器改中继之后，从回复里能看出的结果。
#[derive(Debug, PartialEq, Eq)]
enum RelaySetReply {
    Applied,
    /// 它明确回了错：没改。
    Rejected(String),
    /// 没拿到回复。它换中继要关掉旧 endpoint，回复走的正是这条连接，常常发不回来；
    /// 这时它多半已经改了，不能当成没改。
    Lost(String),
}

fn relay_set_reply<T>(
    result: Result<T, smelt_runtime_client::protocol::control::ControlClientError>,
) -> RelaySetReply {
    use smelt_runtime_client::protocol::control::ControlClientError;
    match result {
        Ok(_) => RelaySetReply::Applied,
        Err(error @ ControlClientError::Protocol(_)) => RelaySetReply::Rejected(error.to_string()),
        Err(error) => RelaySetReply::Lost(error.to_string()),
    }
}

/// 配对要走公网拨号，比本机调用慢得多。
const PAIR_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(30);

/// 移除一台远程机器。对端节点请本机运行时删掉（它在后台请对方也删掉本节点）；旧直连机器
/// 从客户端库删掉，在线的再请它撤销这台设备（连不上就算了）。删完就交回，不等对方。
pub(crate) fn remove_runtime(
    runtime_id: String,
    cx: &mut gpui::App,
) -> gpui::Task<Result<(), String>> {
    let Some(remote) = cx.try_global::<RuntimeHub>().and_then(|hub| {
        hub.remotes
            .iter()
            .find(|remote| remote.connection.runtime_id == runtime_id)
            .cloned()
    }) else {
        return gpui::Task::ready(Err(smelt_plugin_api::machine_removed()));
    };
    let peer = remote.transport.is_peer();
    // 在线才顺手请它吊销本机的配对。离线的连不上，拨号要等到超时，不能让移除跟着等；
    // 那台机器上的配对记录留着，要清就在那台机器上撤。
    let revoke = !peer && matches!(remote.status, RuntimeStatus::Online { .. });
    cx.spawn(async move |cx| {
        let removed = cx
            .background_executor()
            .spawn({
                let runtime_id = runtime_id.clone();
                async move {
                    if peer {
                        local()
                            .call::<smelt_model::control_api::PeerRemove>(
                                &smelt_model::peer_node::PeerIdParams { runtime_id },
                                smelt_runtime_client::DEFAULT_REQUEST_TIMEOUT,
                            )
                            .map(|_| ())
                            .map_err(|error| {
                                t!("runtime.remove_failed", error = error).into_owned()
                            })
                    } else {
                        smelt_client_store::client_store()
                            .and_then(|store| store.delete_runtime_connection(&runtime_id))
                            .map(|_| ())
                            .map_err(|error| {
                                t!("runtime.remove_failed", error = error).into_owned()
                            })
                    }
                }
            })
            .await;
        removed?;
        cx.update(|cx| {
            update_hub(cx, |hub| {
                hub.remotes
                    .retain(|remote| remote.connection.runtime_id != runtime_id)
            });
            remote.watch.unwatch();
        });
        if revoke {
            // 撤销不拖住移除：在后台发完就算。
            let connection = remote.connection.clone();
            cx.background_executor()
                .spawn(async move { revoke_legacy(&connection) })
                .detach();
        }
        Ok(())
    })
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
        }
    }

    /// 调用按 key 发给那台机器自己的客户端；机器移除了就取不到，不能悄悄退回本机。
    #[gpui::test]
    fn calls_resolve_to_the_keyed_machine(cx: &mut gpui::TestAppContext) {
        let far = RuntimeKey::Remote("far".into());
        cx.update(|cx| {
            assert!(client(&far, cx).is_none(), "还没挂机器时不该取到客户端");
            cx.set_global(RuntimeHub {
                remotes: vec![{
                    let mut far = remote_entry(
                        smelt_client_store::RuntimeConnection {
                            runtime_id: "far".into(),
                            name: "far".into(),
                            endpoint_id: String::new(),
                            relay_url: None,
                            added_at_ms: 0,
                            os: Some("windows".into()),
                        },
                        Transport::Broken("connected far".into()),
                        None,
                    );
                    far.status = online();
                    far
                }],
                ..Default::default()
            });
            let error = client(&far, cx)
                .expect("挂着的机器要取到客户端")
                .connect()
                .expect_err("假连接器只会报错");
            assert_eq!(
                error.to_string(),
                "connected far",
                "取到的不是那台机器的客户端"
            );
            assert!(client(&RuntimeKey::Remote("gone".into()), cx).is_none());
            assert!(client(&RuntimeKey::Local, cx).is_some());
            // 路径按那台机器的系统切，跟本机是什么系统无关。
            assert_eq!(path_style(&far, cx), PathStyle::Windows);
            assert_eq!(
                path_style(&RuntimeKey::Remote("gone".into()), cx),
                PathStyle::Posix
            );
            assert_eq!(path_style(&RuntimeKey::Local, cx), PathStyle::LOCAL);
        });
    }

    fn connection(runtime_id: &str) -> smelt_client_store::RuntimeConnection {
        smelt_client_store::RuntimeConnection {
            runtime_id: runtime_id.into(),
            name: format!("{runtime_id} here"),
            endpoint_id: String::new(),
            relay_url: None,
            added_at_ms: 0,
            os: None,
        }
    }

    fn peer(runtime_id: &str, agent_access: bool) -> smelt_model::peer_node::PeerNode {
        smelt_model::peer_node::PeerNode {
            runtime_id: runtime_id.into(),
            endpoint_id: format!("ep-{runtime_id}"),
            name: format!("{runtime_id} peer"),
            relay_url: None,
            os: Some("linux".into()),
            agent_access,
            inbound: false,
            added_at_ms: 1,
        }
    }

    fn legacy(runtime_id: &str) -> RemoteRuntime {
        remote_entry(
            connection(runtime_id),
            Transport::Legacy {
                endpoint: smelt_iroh::load_or_create_secret(&std::env::temp_dir().join(format!(
                    "smelt-legacy-key-{}-{runtime_id}",
                    std::process::id()
                )))
                .unwrap()
                .public(),
                route: RemoteRoute::default(),
            },
            None,
        )
    }

    /// 本机运行时的对端表是机器列表的来源：旧直连机器已经是对端的就地换过去（客户端库里那行
    /// 要删），新对端加上，不再是对端的移走；还没迁的旧机器留着。
    #[test]
    fn peers_replace_migrated_machines_and_follow_the_runtime() {
        let mut hub = RuntimeHub {
            remotes: vec![
                legacy("moved"),
                legacy("still-old"),
                remote_entry(
                    connection("unpaired"),
                    Transport::Peer,
                    Some(PeerAccess {
                        agents: false,
                        inbound: false,
                    }),
                ),
            ],
            ..Default::default()
        };
        let migrated = merge_peers(&mut hub, vec![peer("moved", false), peer("new", true)]);
        assert_eq!(migrated, vec!["moved".to_string()]);
        assert!(hub.peers_loaded);
        let ids: Vec<_> = hub
            .remotes
            .iter()
            .map(|remote| remote.connection.runtime_id.as_str())
            .collect();
        assert_eq!(
            ids,
            ["moved", "still-old", "new"],
            "原来的位置不变，新的排在后面"
        );
        let moved = &hub.remotes[0];
        assert!(moved.transport.is_peer());
        assert_eq!(moved.agent_access, Some(false));
        assert_eq!(moved.inbound, Some(false));
        assert_eq!(
            moved.connection.name, "moved peer",
            "名字以本机运行时记的为准"
        );
        assert!(hub.remotes[1].transport.legacy_route().is_some());
        assert_eq!(hub.remotes[1].agent_access, None);
        assert_eq!(hub.remotes[2].agent_access, Some(true));
    }

    /// 迁移就地换掉连接方式：之前拿走的同一个客户端，下次连接就走新的。
    #[test]
    fn a_held_client_follows_the_transport_switch() {
        let remote = remote_entry(connection("far"), Transport::Broken("old way".into()), None);
        let held = remote.runtime.client().clone();
        assert_eq!(held.connect().unwrap_err().to_string(), "old way");
        remote.transport.set(Transport::Broken("new way".into()));
        assert_eq!(held.connect().unwrap_err().to_string(), "new way");
    }

    /// 改中继的回复丢了不等于没改：那台机器换中继时会关掉回复要走的那条连接。
    #[test]
    fn lost_relay_reply_is_not_taken_as_rejected() {
        use smelt_runtime_client::protocol::control::{
            ControlClientError, ControlError, ControlErrorCode,
        };
        assert_eq!(relay_set_reply(Ok(())), RelaySetReply::Applied);
        assert!(matches!(
            relay_set_reply::<()>(Err(ControlClientError::Protocol(ControlError::new(
                ControlErrorCode::InvalidParams,
                "bad relay",
            )))),
            RelaySetReply::Rejected(_)
        ));
        assert!(matches!(
            relay_set_reply::<()>(Err(ControlClientError::Transport(
                "smeltd closed the control request without a response".into()
            ))),
            RelaySetReply::Lost(_)
        ));
        assert!(matches!(
            relay_set_reply::<()>(Err(ControlClientError::InvalidResponse("eof".into()))),
            RelaySetReply::Lost(_)
        ));
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

    /// 延迟抖一下就让看着连接表的插件重画不值得；换了路径、或者变化够大才算变。
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

    /// 每次连接都用 `respond` 回一条 `system.describe`，模拟另一个版本的运行时。
    fn runtime_answering(
        respond: impl Fn(&serde_json::Value) -> serde_json::Value + Send + Sync + Clone + 'static,
    ) -> RuntimeClient {
        use std::io::{BufRead, Write};
        RuntimeClient::new(move || {
            let (client, server) = smelt_runtime_client::RuntimeStream::pair()?;
            let respond = respond.clone();
            std::thread::spawn(move || {
                let mut line = String::new();
                std::io::BufReader::new(server.try_clone().unwrap())
                    .read_line(&mut line)
                    .unwrap();
                let request: serde_json::Value = serde_json::from_str(&line).unwrap();
                let response = respond(&request);
                writeln!(&server, "{response}").unwrap();
            });
            Ok(client)
        })
    }

    fn describe_without(method: &'static str) -> RuntimeClient {
        use smelt_runtime_client::protocol::control::ControlResponse;
        runtime_answering(move |request| {
            let request_id = request["request_id"].as_str().unwrap();
            serde_json::to_value(ControlResponse::ok(request_id, described_without(method)))
                .unwrap()
        })
    }

    /// 版本 0.0.1、和这个桌面同一套协议、只少了 `method` 的运行时会回的 describe。
    fn described_without(
        method: &str,
    ) -> smelt_runtime_client::protocol::control::SystemDescribeResult {
        use smelt_runtime_client::protocol::control::{
            CONTROL_API_MIN_SUPPORTED_VERSION, CONTROL_API_VERSION, ControlMethod,
            ControlProtocolDescription, DaemonRuntimeInfo, EventProtocolDescription,
            SystemDescribeResult,
        };
        SystemDescribeResult {
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
                smelt_home: None,
            },
            events: Some(EventProtocolDescription {
                protocol_version: smelt_event_protocol::EVENT_BUS_PROTOCOL_VERSION,
                projection_version: smelt_event_protocol::CORE_PROJECTION_VERSION,
                session_projection_version: smelt_event_protocol::CORE_SESSION_PROJECTION_VERSION,
                session_state_schema_version:
                    smelt_event_protocol::CORE_SESSION_STATE_SCHEMA_VERSION,
            }),
        }
    }

    #[test]
    fn matching_runtime_is_online() {
        assert!(matches!(
            status_of(&describe_without("no.such.method")),
            RuntimeStatus::Online { .. }
        ));
    }

    /// 控制协议比桌面旧的运行时回的 describe。
    fn older_runtime() -> smelt_runtime_client::protocol::control::SystemDescribeResult {
        use smelt_runtime_client::protocol::control::CONTROL_API_VERSION;
        let mut described = described_without("no.such.method");
        described.control.min_supported_version = CONTROL_API_VERSION - 1;
        described.control.max_supported_version = CONTROL_API_VERSION - 1;
        described
    }

    /// 控制协议比桌面旧：标成不兼容并说要升级运行时。
    #[test]
    fn older_runtime_is_incompatible() {
        use smelt_runtime_client::protocol::control::ControlResponse;
        let client = runtime_answering(|request| {
            let request_id = request["request_id"].as_str().unwrap();
            serde_json::to_value(ControlResponse::ok(request_id, older_runtime())).unwrap()
        });
        let RuntimeStatus::Incompatible {
            version,
            detail,
            fix,
        } = status_of(&client)
        else {
            panic!("控制协议旧的运行时应判为不兼容");
        };
        assert_eq!(
            fix,
            IncompatibleFix::UpgradeRuntime,
            "Linux 运行时旧，可以请它升级"
        );
        assert_eq!(version, "0.0.1");
        assert!(detail.contains("0.0.1"), "{detail}");
        assert!(detail.contains("update the runtime"), "{detail}");
        assert!(detail.contains("control protocol"), "{detail}");
    }

    /// Windows 上的运行时随那台机器上的 Smelt 一起更新：旧了也不请它远程升级。
    #[test]
    fn older_windows_runtime_is_updated_on_that_machine() {
        use smelt_runtime_client::protocol::control::ControlResponse;
        let client = runtime_answering(|request| {
            let request_id = request["request_id"].as_str().unwrap();
            let mut described = older_runtime();
            described.daemon.os = Some("windows".to_string());
            serde_json::to_value(ControlResponse::ok(request_id, described)).unwrap()
        });
        let RuntimeStatus::Incompatible { fix, .. } = status_of(&client) else {
            panic!("控制协议旧的运行时应判为不兼容");
        };
        assert_eq!(fix, IncompatibleFix::UpdateSmeltThere);
        assert!(fix.runtime_outdated());
    }

    /// 缺方法照常在线：只是那一项功能用不了，用到时报错。
    #[test]
    fn runtime_missing_a_method_is_online() {
        assert!(matches!(
            status_of(&describe_without("pi.mcp.login_start")),
            RuntimeStatus::Online { .. }
        ));
    }

    /// 运行时的控制协议比桌面新，拒绝桌面的版本：按它的版本再问一次，读出它是什么版本，
    /// 提示升级桌面。
    #[test]
    fn runtime_rejecting_our_control_version_is_incompatible() {
        use smelt_runtime_client::protocol::control::{
            CONTROL_API_VERSION, ControlError, ControlResponse,
        };
        let newer = CONTROL_API_VERSION + 1;
        let client = runtime_answering(move |request| {
            let request_id = request["request_id"].as_str().unwrap();
            if request["control_version"] == newer {
                let mut described = described_without("no.such.method");
                described.control.min_supported_version = newer;
                described.control.max_supported_version = newer;
                described.daemon.version = "9.9.9".to_string();
                serde_json::to_value(ControlResponse::ok_with_version(
                    newer, request_id, described,
                ))
                .unwrap()
            } else {
                serde_json::to_value(ControlResponse::<()>::error_with_version(
                    newer,
                    request_id,
                    ControlError::unsupported_version(CONTROL_API_VERSION, newer, newer),
                ))
                .unwrap()
            }
        });
        let RuntimeStatus::Incompatible {
            version,
            detail,
            fix,
        } = status_of(&client)
        else {
            panic!("控制协议对不上应判为不兼容");
        };
        assert_eq!(
            fix,
            IncompatibleFix::UpdateDesktop,
            "运行时比桌面新，只升不降，不能请它升级"
        );
        assert_eq!(version, "9.9.9");
        assert!(detail.contains("update this app"), "{detail}");
    }

    /// 订阅线程把每次探测到的连通性报回来，订阅断开也报：机器的「离线」靠它更新，
    /// 不能只在启动时探一次。
    #[test]
    fn subscription_reports_reachability_changes() {
        let (states, _states_rx) = smol::channel::unbounded();
        let (topics, _topics_rx) = smol::channel::unbounded();
        let (commands, _commands_rx) = smol::channel::unbounded();
        let events = crate::runtime_events::RuntimeEventSenders {
            states,
            topics,
            commands,
        };
        let (status, status_rx) = smol::channel::unbounded();
        let handle = crate::runtime_events::spawn_subscription(
            remote_runtime("rt-test", describe_without("no.such.method")),
            events,
            status,
            crate::runtime_events::WantedTopics::default(),
        );
        let (key, first) = status_rx.recv_blocking().unwrap();
        assert_eq!(key, RuntimeKey::Remote("rt-test".into()));
        assert!(matches!(first, RuntimeStatus::Online { .. }), "{first:?}");
        // 假运行时只答 describe，订阅随即断开：断开要报离线。
        let (_, second) = status_rx.recv_blocking().unwrap();
        assert!(matches!(second, RuntimeStatus::Offline(_)), "{second:?}");
        handle.stop();
    }

    /// 版本对不上的订阅线程不退出：退避着再探，「重新连接」叫醒它立刻再探。
    #[test]
    fn incompatible_runtime_is_probed_again_on_reconnect() {
        use smelt_runtime_client::protocol::control::ControlResponse;
        // 比桌面新的运行时：不会去请它升级。
        let newer = runtime_answering(|request| {
            let request_id = request["request_id"].as_str().unwrap();
            let mut described = described_without("no.such.method");
            described.events.as_mut().unwrap().protocol_version += 1;
            serde_json::to_value(ControlResponse::ok(request_id, described)).unwrap()
        });
        let (states, _states_rx) = smol::channel::unbounded();
        let (topics, _topics_rx) = smol::channel::unbounded();
        let (commands, _commands_rx) = smol::channel::unbounded();
        let events = crate::runtime_events::RuntimeEventSenders {
            states,
            topics,
            commands,
        };
        let (status, status_rx) = smol::channel::unbounded();
        let handle = crate::runtime_events::spawn_subscription(
            remote_runtime("rt-new", newer),
            events,
            status,
            crate::runtime_events::WantedTopics::default(),
        );
        let (_, first) = status_rx.recv_blocking().unwrap();
        assert!(
            matches!(first, RuntimeStatus::Incompatible { .. }),
            "{first:?}"
        );

        handle.reconnect();
        // 首档退避 30 秒；叫醒了就不用等。叫醒先报「连接中」，再报探到的结果。
        let next = || {
            smol::block_on(smol::future::or(
                async { status_rx.recv().await.ok() },
                async {
                    smol::Timer::after(std::time::Duration::from_secs(5)).await;
                    None
                },
            ))
        };
        let connecting = next();
        assert!(
            matches!(connecting, Some((_, RuntimeStatus::Connecting))),
            "{connecting:?}"
        );
        let again = next();
        assert!(
            matches!(again, Some((_, RuntimeStatus::Incompatible { .. }))),
            "{again:?}"
        );
        handle.stop();
    }

    const UPGRADE_TARGET: &str = "2026.1027.0";

    /// 假运行时：读到首行升级请求后按顺序回这几行，然后断开。
    fn runtime_replying(lines: &'static [&'static str]) -> RuntimeClient {
        use std::io::{BufRead, Write};
        RuntimeClient::new(move || {
            let (client, server) = smelt_runtime_client::RuntimeStream::pair()?;
            std::thread::spawn(move || {
                let mut line = String::new();
                std::io::BufReader::new(server.try_clone().unwrap())
                    .read_line(&mut line)
                    .unwrap();
                let request: serde_json::Value = serde_json::from_str(&line).unwrap();
                assert_eq!(request["op"], "runtime_upgrade");
                assert_eq!(request["version"], UPGRADE_TARGET);
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
        assert_eq!(
            push_upgrade_to(&runtime_replying(&[]), UPGRADE_TARGET),
            PushUpgrade::Unsupported
        );
    }

    /// 接下以后断开：停机重启时旧进程退出就是这样，要重新探测。
    #[test]
    fn hanging_up_after_accepting_means_go_look_again() {
        assert_eq!(
            push_upgrade_to(
                &runtime_replying(&[r#"{"state":"accepted"}"#]),
                UPGRADE_TARGET
            ),
            PushUpgrade::Restarting
        );
    }

    #[test]
    fn the_runtime_says_how_the_upgrade_ended() {
        assert_eq!(
            push_upgrade_to(
                &runtime_replying(&[
                    r#"{"state":"accepted"}"#,
                    r#"{"state":"failed","error":"GitHub 上没有 v9.9.9"}"#,
                ]),
                UPGRADE_TARGET,
            ),
            PushUpgrade::Failed("GitHub 上没有 v9.9.9".into())
        );
        assert_eq!(
            push_upgrade_to(
                &runtime_replying(&[
                    r#"{"state":"accepted"}"#,
                    r#"{"state":"staged","reason":"ACP 会话仍有未完成请求"}"#,
                ]),
                UPGRADE_TARGET,
            ),
            PushUpgrade::Staged("ACP 会话仍有未完成请求".into())
        );
        assert_eq!(
            push_upgrade_to(
                &runtime_replying(&[r#"{"state":"current"}"#]),
                UPGRADE_TARGET
            ),
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

        let remote = pair_legacy(&uri).unwrap();
        assert!(matches!(remote.status, RuntimeStatus::Online { .. }));
        let stored = smelt_client_store::client_store()
            .unwrap()
            .runtime_connections()
            .unwrap();
        assert_eq!(stored, vec![remote.connection]);

        let again = legacy_client(&stored[0]).unwrap().describe().unwrap();
        assert_eq!(
            again.daemon.runtime_id.as_deref(),
            Some(stored[0].runtime_id.as_str())
        );
        eprintln!("已添加 {}（{}）", stored[0].name, stored[0].runtime_id);
        let _ = std::fs::remove_dir_all(home);
    }

    /// 对着一台真实运行时走一遍桌面要用的远程链路：配对、对版本、在那台机器上开终端、订阅
    /// 首帧带着工作区目录、订阅里收到这个终端的状态，最后杀掉终端、撤销这台设备。默认不跑：
    /// `SMELT_TEST_PAIRING_URI='smelt+iroh://…' cargo test -p smelt runtimes -- --ignored`
    #[test]
    #[ignore = "需要一台真实运行时和一次性配对串"]
    fn drives_a_real_remote_runtime() {
        use smelt_runtime_client::protocol::control::{EmptyParams, SystemDescribe};

        let uri = std::env::var("SMELT_TEST_PAIRING_URI").expect("设置 SMELT_TEST_PAIRING_URI");
        let home = std::env::temp_dir().join(format!(
            "smelt-remote-test-{}",
            uuid::Uuid::new_v4().simple()
        ));
        // SAFETY: nextest 每个测试一个进程，这里没有别的线程在读环境变量。
        unsafe { std::env::set_var("SMELT_HOME", &home) };

        let remote = pair_legacy(&uri).unwrap();
        let runtime = remote.runtime.clone();
        runtime
            .client()
            .call::<SystemDescribe>(&EmptyParams::default(), std::time::Duration::from_secs(30))
            .unwrap();

        let (tx, rx) = std::sync::mpsc::channel();
        let subscriber = runtime.client().clone();
        std::thread::spawn(move || {
            smelt_client::daemon_state::subscribe_daemon_states_blocking(
                &subscriber,
                false,
                |event| tx.send(event).is_ok(),
            );
        });

        // 和终端插件一样发 open 开一个终端；连接留着就是附着着。
        let sid = format!("remote-test-{}", uuid::Uuid::new_v4().simple());
        let mut terminal = runtime.client().connect().unwrap();
        {
            use std::io::Write as _;
            let open = serde_json::json!({
                "op": smelt_runtime_client::protocol::daemon::DaemonOperation::Open,
                "id": sid,
                "cols": 80,
                "rows": 24,
                "initial_launch": "echo smelt-remote-ok; sleep 60",
                "create_if_missing": true,
            });
            writeln!(terminal, "{open}").unwrap();
        }

        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
        let mut seen = false;
        let mut directory = false;
        while !seen && std::time::Instant::now() < deadline {
            let Ok(event) = rx.recv_timeout(std::time::Duration::from_secs(1)) else {
                continue;
            };
            seen = match event {
                smelt_client::daemon_state::DaemonStateEvent::Snapshot {
                    sessions,
                    workspace,
                    ..
                } => {
                    directory |= workspace.is_some();
                    sessions.iter().any(|state| state.id == sid)
                }
                smelt_client::daemon_state::DaemonStateEvent::Update(state) => state.id == sid,
                _ => false,
            };
        }
        assert!(seen, "订阅里没看到远程终端 {sid}");
        assert!(directory, "订阅首帧没带工作区目录");

        drop(terminal);
        let _ = runtime.client().request(
            &serde_json::json!({
                "op": smelt_runtime_client::protocol::daemon::DaemonOperation::Kill,
                "id": sid,
            }),
            smelt_runtime_client::DEFAULT_REQUEST_TIMEOUT,
        );
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
                .call::<SystemDescribe>(&EmptyParams::default(), std::time::Duration::from_secs(30))
                .is_err(),
            "撤销之后不该还能调"
        );
        eprintln!(
            "远程链路走通：{}（{}）",
            remote.connection.name, remote.connection.runtime_id
        );
        let _ = std::fs::remove_dir_all(home);
    }

    /// 对着两台真实运行时走一遍旧机器的迁移：用这个桌面的旧身份直连配对，再迁成本机运行时
    /// 的对端节点、撤销旧设备，之后经本机运行时转发照样连得上。`SMELT_HOME` 指向一台在跑
    /// 新版 smeltd 的本机运行时，配对串是另一台的。默认不跑：
    /// `SMELT_HOME=… SMELT_TEST_PAIRING_URI='smelt+iroh://…' cargo test -p smelt migrates_a_real -- --ignored`
    #[test]
    #[ignore = "需要两台真实运行时和一次性配对串"]
    fn migrates_a_real_legacy_machine() {
        use smelt_model::control_api::{DeviceList, PeerList};

        let uri = std::env::var("SMELT_TEST_PAIRING_URI").expect("设置 SMELT_TEST_PAIRING_URI");
        let legacy = pair_legacy(&uri).unwrap();
        let connection = legacy.connection.clone();
        let me = iroh_client().unwrap().id().to_string();
        let devices = |client: &RuntimeClient| {
            client
                .call::<DeviceList>(&Default::default(), PAIR_TIMEOUT)
                .unwrap()
                .devices
                .into_iter()
                .map(|device| device.endpoint_id)
                .collect::<Vec<_>>()
        };
        assert!(
            devices(legacy.runtime.client()).contains(&me),
            "旧身份要先配对上"
        );

        let peer = migrate(&connection)
            .unwrap()
            .expect("新版运行时认得节点配对");
        assert_eq!(peer.runtime_id, connection.runtime_id);
        assert_eq!(peer.name, connection.name, "名字跟着带过去");
        let peers = local()
            .call::<PeerList>(&Default::default(), PAIR_TIMEOUT)
            .unwrap()
            .peers;
        assert!(
            peers
                .iter()
                .any(|known| known.runtime_id == peer.runtime_id)
        );

        let forwarded = remote_entry(
            peer_connection(&peer),
            Transport::Peer,
            Some(PeerAccess::of(&peer)),
        );
        assert!(
            devices(forwarded.runtime.client()).contains(&me),
            "迁完、撤销之前旧设备还在"
        );
        revoke_legacy(&connection);
        let described = forwarded.runtime.client().describe().unwrap();
        assert_eq!(
            described.daemon.runtime_id.as_deref(),
            Some(peer.runtime_id.as_str())
        );
        assert!(
            !devices(forwarded.runtime.client()).contains(&me),
            "撤销之后那边不该再有旧设备"
        );
        eprintln!("已迁移 {}（{}）", peer.name, peer.runtime_id);
    }

    /// 睡醒换掉 iroh endpoint 后，路线上按旧 endpoint 拨的连接器不能再用：它拴着的那个
    /// endpoint 可能已经连不上 relay，接着用就一直拨号超时。
    #[test]
    fn route_rebuilds_connector_once_endpoint_is_replaced() {
        // 不碰网络：relay 指向本机不存在的端口，绑定不等它。
        let relay = smelt_iroh::RelaySettings::parse("127.0.0.1:9").unwrap();
        let bind = |name: &str| {
            let path = std::env::temp_dir()
                .join(format!("smelt-route-test-{}-{name}", std::process::id()));
            let _ = std::fs::remove_file(&path);
            let secret = smelt_iroh::load_or_create_secret(&path).unwrap();
            let _ = std::fs::remove_file(&path);
            smelt_iroh::runtime::IrohClient::new(secret, Some(&relay)).unwrap()
        };
        let old = bind("old");
        let new = bind("new");
        let target = new.id();
        let mut route = RouteState::default();

        let first = route.connector(&old, target);
        assert!(
            std::sync::Arc::ptr_eq(&first, &route.connector(&old, target)),
            "endpoint 没换就接着用同一个连接器"
        );

        let rebuilt = route.connector(&new, target);
        assert!(rebuilt.uses(&new), "换了 endpoint 要按新的重建");
        assert!(!rebuilt.uses(&old));
        assert!(std::sync::Arc::ptr_eq(
            &rebuilt,
            &route.connector(&new, target)
        ));
    }
}
