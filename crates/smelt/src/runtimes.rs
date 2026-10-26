//! 桌面挂着的运行时（设计文档 D6）。本机运行时总在；远程运行时来自
//! 客户端库里登记的连接。会话、视图各自记着自己属于哪个运行时，调用、连接都发给它。

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

/// 那台机器的 `SMELT_HOME`：本机直接算；远程的取连上时记下的，还没连上过（或运行时太旧
/// 不报）是 `None`。客户端拿它认出那台机器上哪些会话是智能体对话。
pub(crate) fn smelt_home(key: &RuntimeKey, cx: &gpui::App) -> Option<String> {
    match key {
        // 本机主目录是这台电脑上的路径；比会话 cwd 时按运行时路径字符串用。
        RuntimeKey::Local => {
            smelt_paths::smelt_home().and_then(|path| path.to_str().map(str::to_owned))
        }
        RuntimeKey::Remote(_) => remote_of(key, cx)?.connection.smelt_home.clone(),
    }
}

/// 给终端开一条连接。本机守护没在跑就先拉起来，这只有本机做得到；远程运行时连不上
/// 就是连不上，由调用方按断线处理。
pub(crate) fn connect_terminal(
    runtime: &Runtime,
) -> std::io::Result<smelt_runtime_client::RuntimeStream> {
    if runtime.is_local() {
        // 拉起本机 smeltd 的生命周期见设计文档 D3/D4。
        return crate::local_daemon::connect_daemon();
    }
    runtime.client().connect()
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
        /// 装了远程桌面，侧栏才给「远程桌面」。
        desktop: bool,
        /// 运行时报的 `SMELT_HOME`，旧运行时不报。见 [`smelt_home`]。
        smelt_home: Option<String>,
    },
    Offline(String),
    /// 连得上，但版本和这个桌面对不上（见 `smelt_client::compat`），说明里写着该升级
    /// 哪一边。这时不订阅它的事件：订阅了也解不出来，只会断开重连。
    Incompatible {
        /// 运行时自己报的版本。
        version: String,
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
    /// 经哪个中继连它，和它的连接器共用：读眼下的连接路径、改中继都经这里。
    route: RemoteRoute,
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

/// 一台机器的连接状态写成字：走哪条路、多慢，版本不对时是什么版本。
fn link_text(status: &RuntimeStatus, link: Option<&RuntimeLink>) -> (String, LinkTone) {
    match status {
        RuntimeStatus::Connecting => ("连接中".to_string(), LinkTone::Muted),
        RuntimeStatus::Offline(_) => ("离线".to_string(), LinkTone::Muted),
        RuntimeStatus::Incompatible { version, .. } => {
            let text = format!("版本不兼容 · {version}");
            // 连得上，连接路径照样有：也说走哪条路、多慢。
            let text = match link {
                Some(link) => format!("{text} · {}", link_summary(link)),
                None => text,
            };
            (text, LinkTone::Warning)
        }
        RuntimeStatus::Upgrading(progress) => (progress.clone(), LinkTone::Muted),
        RuntimeStatus::Online { .. } => {
            let Some(link) = link else {
                return ("在线".to_string(), LinkTone::Muted);
            };
            let tone = if link.rtt_ms >= SLOW_RTT_MS {
                LinkTone::Warning
            } else {
                LinkTone::Muted
            };
            match &link.kind {
                LinkKind::Other => (format!("在线 · {}ms", link.rtt_ms), tone),
                _ => (link_summary(link), tone),
            }
        }
    }
}

/// 侧栏标题上的连接指示灯。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum LinkLight {
    /// 在线，延迟正常。
    Good,
    /// 能用但要留意：连接中、升级中、延迟高。
    Degraded,
    /// 用不了：离线、版本不兼容。
    Down,
}

/// 侧栏标题上一台远程机器的连接状态：一盏灯，悬停看完整说明。要人处理的（离线、版本
/// 不兼容、升级中）灯旁再写几个字，不只靠颜色。
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct RuntimeIndicator {
    pub(crate) light: LinkLight,
    pub(crate) label: Option<String>,
    pub(crate) detail: String,
}

fn indicator(status: &RuntimeStatus, link: Option<&RuntimeLink>) -> RuntimeIndicator {
    let (light, label) = match status {
        RuntimeStatus::Online { .. } => match link {
            Some(link) if link.rtt_ms >= SLOW_RTT_MS => (LinkLight::Degraded, None),
            _ => (LinkLight::Good, None),
        },
        RuntimeStatus::Connecting => (LinkLight::Degraded, None),
        RuntimeStatus::Upgrading(_) => (LinkLight::Degraded, Some("升级中".to_string())),
        RuntimeStatus::Offline(_) => (LinkLight::Down, Some("离线".to_string())),
        RuntimeStatus::Incompatible { version, .. } => {
            (LinkLight::Down, Some(format!("版本不兼容 · {version}")))
        }
    };
    let detail = match status {
        RuntimeStatus::Offline(error) => format!("离线：{error}"),
        _ => link_text(status, link).0,
    };
    RuntimeIndicator {
        light,
        label,
        detail,
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

/// 走哪条路、多慢。
fn link_summary(link: &RuntimeLink) -> String {
    match &link.kind {
        LinkKind::Direct => format!("直连 · {}ms", link.rtt_ms),
        LinkKind::Relay(relay) => format!("中继 {relay} · {}ms", link.rtt_ms),
        LinkKind::Other => format!("{}ms", link.rtt_ms),
    }
}

/// 侧栏标题上的连接指示，见 [`RuntimeIndicator`]。本机只在出了状况（守护没起来、
/// 版本不对、正在升级）时显示：平时它总在，挂一盏绿灯只是噪音。
pub(crate) fn runtime_indicator(key: &RuntimeKey, cx: &gpui::App) -> Option<RuntimeIndicator> {
    match key {
        RuntimeKey::Local => status_of_key(key, cx)
            .filter(|status| !matches!(status, RuntimeStatus::Online { .. }))
            .map(|status| indicator(status, None)),
        RuntimeKey::Remote(_) => {
            let remote = remote_of(key, cx)?;
            Some(indicator(&remote.status, remote.link.as_ref()))
        }
    }
}

/// 一台机器的完整连接说明，设置「机器」页用。
pub(crate) fn runtime_link_detail(key: &RuntimeKey, cx: &gpui::App) -> (String, LinkTone) {
    match key {
        RuntimeKey::Local => ("这台 Mac".to_string(), LinkTone::Muted),
        RuntimeKey::Remote(_) => remote_of(key, cx)
            .map(|remote| link_text(&remote.status, remote.link.as_ref()))
            .unwrap_or_else(|| ("离线".to_string(), LinkTone::Muted)),
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

/// 远程机器眼下连得上（在线，或连上了但版本对不上）：可以请它升级。升级走守护协议的
/// `runtime_upgrade`，不经控制方法，版本对不上也发得过去。
pub(crate) fn runtime_can_upgrade(key: &RuntimeKey, cx: &gpui::App) -> bool {
    remote_of(key, cx).is_some_and(|remote| {
        matches!(
            remote.status,
            RuntimeStatus::Online { .. } | RuntimeStatus::Incompatible { .. }
        )
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

/// 这台机器装了远程桌面、眼下也连着。本机（macOS）没有这套。
pub(crate) fn runtime_has_desktop(key: &RuntimeKey, cx: &gpui::App) -> bool {
    remote_of(key, cx)
        .is_some_and(|remote| matches!(remote.status, RuntimeStatus::Online { desktop: true, .. }))
}

/// 机器列表读不出来，或者有哪台机器离线、版本不对、走中继还很慢：侧栏「设置」按钮上挂个点提醒。
pub(crate) fn any_runtime_needs_attention(cx: &gpui::App) -> bool {
    cx.try_global::<RuntimeHub>().is_some_and(|hub| {
        hub.load_error.is_some()
            || hub
                .local_status
                .as_ref()
                .is_some_and(|status| indicator(status, None).light == LinkLight::Down)
            || hub.remotes.iter().any(|remote| {
                match indicator(&remote.status, remote.link.as_ref()).light {
                    LinkLight::Down => true,
                    // 连接中、升级中是过程，不算；在线却慢才算。
                    LinkLight::Degraded => matches!(remote.status, RuntimeStatus::Online { .. }),
                    LinkLight::Good => false,
                }
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
    /// 启动时没读出登记过的机器（比如客户端库版本比程序新）。这时列表里只剩本机，不说
    /// 一声就像机器全丢了，所以设置「机器」页要显示它。
    pub(crate) load_error: Option<String>,
    /// 本机守护最近一次探到的状态，订阅线程报来（见 `runtime_events`）。还没探过是 `None`。
    pub(crate) local_status: Option<RuntimeStatus>,
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

/// 经哪个中继连一台远程运行时，以及按它拨好的连接器（还没拨过号就是 `None`）。
#[derive(Default)]
struct RouteState {
    relay: Option<smelt_iroh::RelaySettings>,
    connector: Option<std::sync::Arc<smelt_iroh::runtime::RuntimeConnector>>,
}

/// 盯着连接路径的守卫，带着编号：认出守卫换过之后才到的旧事件。
type RouteWatch = Option<(u64, smelt_iroh::runtime::LinkWatch)>;

/// 一台远程运行时的连接路线，它的连接器和 [`RemoteRuntime`] 共用一份：开着的终端、对话
/// 拿的都是同一个连接器，改了中继，它们下次开流就走新中继，不用重开。
#[derive(Clone, Default)]
struct RemoteRoute {
    state: std::sync::Arc<std::sync::Mutex<RouteState>>,
    /// 单独一把锁：第一次拨号时 `state` 会被握着绑 iroh，主线程摘守卫不能跟着等。
    watch: std::sync::Arc<std::sync::Mutex<RouteWatch>>,
}

impl RemoteRoute {
    fn new(relay: Option<smelt_iroh::RelaySettings>) -> Self {
        Self {
            state: std::sync::Arc::new(std::sync::Mutex::new(RouteState {
                relay,
                connector: None,
            })),
            watch: Default::default(),
        }
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, RouteState> {
        self.state.lock().unwrap_or_else(|e| e.into_inner())
    }

    fn lock_watch(&self) -> std::sync::MutexGuard<'_, RouteWatch> {
        self.watch.lock().unwrap_or_else(|e| e.into_inner())
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
    fn watch(&self, runtime_id: &str, events: &smol::channel::Sender<LinkEvent>) {
        static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let Some(connector) = self.lock().connector.clone() else {
            return;
        };
        let generation = NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let runtime_id = runtime_id.to_string();
        let events = events.clone();
        // 回调在 iroh 的 tokio 线程上：只往通道里放，不阻塞。
        let guard = connector.watch(move |change| {
            let _ = events.try_send(LinkEvent {
                runtime_id: runtime_id.clone(),
                generation,
                change,
            });
        });
        *self.lock_watch() = Some((generation, guard));
    }

    fn watching(&self) -> bool {
        self.lock_watch().is_some()
    }

    fn is_current(&self, generation: u64) -> bool {
        self.lock_watch()
            .as_ref()
            .is_some_and(|(current, _)| *current == generation)
    }

    fn unwatch(&self) {
        *self.lock_watch() = None;
    }

    /// 只摘编号对得上的那只，摘了返回 `true`：换过守卫后，旧守卫迟到的「断了」不能摘掉
    /// 新的。
    fn unwatch_if(&self, generation: u64) -> bool {
        let mut watch = self.lock_watch();
        let current = watch
            .as_ref()
            .is_some_and(|(current, _)| *current == generation);
        if current {
            *watch = None;
        }
        current
    }

    /// 改走另一个中继：丢掉按旧中继拨的连接器，下一次开流按新的重拨。
    fn retarget(&self, relay: Option<smelt_iroh::RelaySettings>) {
        let mut route = self.lock();
        route.relay = relay;
        route.connector = None;
        drop(route);
        self.unwatch();
    }
}

/// 一台机器连接上的变化，从 iroh 的线程送到主线程。
struct LinkEvent {
    runtime_id: String,
    /// 报它的那只守卫的编号，见 [`RemoteRoute::watch`]。
    generation: u64,
    change: smelt_iroh::runtime::LinkChange,
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

/// 远程运行时的连接器。iroh 要起 tokio，不能在主线程绑，所以第一次连接（总在后台）时
/// 才绑；之后复用同一个，同一条 QUIC 连接上开流。
struct LazyRemoteConnector {
    endpoint: smelt_iroh::EndpointId,
    route: RemoteRoute,
}

impl smelt_runtime_client::Connector for LazyRemoteConnector {
    fn connect(&self) -> std::io::Result<smelt_runtime_client::RuntimeStream> {
        let connector = {
            let mut route = self.route.lock();
            match route.connector.as_ref() {
                Some(connector) => std::sync::Arc::clone(connector),
                None => {
                    let client = iroh_client().map_err(std::io::Error::other)?;
                    let connector =
                        std::sync::Arc::new(client.connector(self.endpoint, route.relay.as_ref()));
                    route.connector = Some(std::sync::Arc::clone(&connector));
                    connector
                }
            }
        };
        smelt_runtime_client::Connector::connect(connector.as_ref())
    }
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

/// 只解析地址，不碰网络，主线程可以调。
fn remote_client(
    connection: &smelt_client_store::RuntimeConnection,
) -> Result<(RuntimeClient, RemoteRoute), String> {
    let endpoint: smelt_iroh::EndpointId = connection
        .endpoint_id
        .parse()
        .map_err(|_| format!("运行时的 endpoint id 不合法：{}", connection.endpoint_id))?;
    let relay = parse_relay(connection.relay_url.as_deref().unwrap_or_default())
        .map_err(|error| format!("relay 地址不合法：{error}"))?;
    let route = RemoteRoute::new(relay);
    let client = RuntimeClient::new(LazyRemoteConnector {
        endpoint,
        route: route.clone(),
    });
    Ok((client, route))
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

fn incompatible_status(incompatible: &smelt_client::compat::Incompatibility) -> RuntimeStatus {
    RuntimeStatus::Incompatible {
        version: incompatible.runtime_version.clone(),
        detail: incompatible.to_string(),
        runtime_outdated: incompatible.outdated == smelt_client::compat::Outdated::Runtime,
    }
}

/// 这个桌面的版本：请远程运行时升级时就升到它。
pub(crate) const DESKTOP_VERSION: &str = env!("CARGO_PKG_VERSION");

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
            Ok(0) if accepted => return PushUpgrade::Restarting,
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
    if let Err(incompatible) = smelt_client::compat::check(&described) {
        return incompatible_status(&incompatible);
    }
    RuntimeStatus::Online {
        desktop: described.daemon.desktop.unwrap_or(false),
        smelt_home: described.daemon.smelt_home,
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
    let online = matches!(status, RuntimeStatus::Online { .. });
    let mut learned_home = None;
    update_hub(cx, |hub| match key {
        RuntimeKey::Local => hub.local_status = Some(status),
        RuntimeKey::Remote(_) => {
            if let Some(remote) = hub
                .remotes
                .iter_mut()
                .find(|remote| remote.runtime.key() == key)
            {
                // SMELT_HOME 记在机器上并落库：离线时、下次启动还没连上时也要认得出它的
                // 智能体对话。
                if let RuntimeStatus::Online {
                    smelt_home: Some(home),
                    ..
                } = &status
                    && remote.connection.smelt_home.as_ref() != Some(home)
                {
                    remote.connection.smelt_home = Some(home.clone());
                    learned_home = Some(remote.connection.clone());
                }
                if !connected(&status) {
                    remote.link = None;
                }
                remote.status = status;
            }
        }
    });
    if let Some(connection) = learned_home {
        cx.background_executor()
            .spawn(async move {
                if let Err(error) = smelt_client_store::client_store()
                    .and_then(|store| store.put_runtime_connection(&connection))
                {
                    eprintln!("[runtimes] 保存机器的 SMELT_HOME 失败：{error}");
                }
            })
            .detach();
    }
    // 远程机器第一次连上时挂上它的智能体配置；本机启动时就挂了。之后断线重连不重读，
    // 已经挂着的只换客户端。
    if online
        && key != &RuntimeKey::Local
        && let Some(client) = client(key, cx)
    {
        crate::settings::attach_agent_host(key.clone(), client, cx, |_| {});
    }
}

fn update_hub(cx: &mut gpui::App, change: impl FnOnce(&mut RuntimeHub)) {
    let mut hub = cx.try_global::<RuntimeHub>().cloned().unwrap_or_default();
    change(&mut hub);
    cx.set_global(hub);
}

/// 启动时读出登记过的远程运行时（客户端库在本机，同步读，主窗口一起来侧栏就有它们
/// 的分段），再起连接变化的处理循环。连通性由订阅线程报（见 `runtime_events`）。
pub(crate) fn start_runtime_hub(cx: &mut gpui::App) {
    let mut hub = RuntimeHub::default();
    match smelt_client_store::client_store().and_then(|store| store.runtime_connections()) {
        Ok(connections) => {
            hub.remotes = connections
                .into_iter()
                .map(|connection| match remote_client(&connection) {
                    Ok((client, route)) => RemoteRuntime {
                        runtime: remote_runtime(&connection.runtime_id, client),
                        connection,
                        status: RuntimeStatus::Connecting,
                        route,
                        link: None,
                        upgrade_note: None,
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
                        route: RemoteRoute::default(),
                        link: None,
                        upgrade_note: None,
                    },
                })
                .collect();
        }
        Err(error) => hub.load_error = Some(format!("读取机器列表失败：{error}")),
    }
    cx.set_global(hub);
    start_link_events(cx);
}

/// 起连接变化的处理循环：iroh 报来路径换了就重读那台机器的路径和延迟，报来断了就清掉。
/// 只在有事件时醒。
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
    let route = remote.route.clone();
    match event.change {
        smelt_iroh::runtime::LinkChange::PathSelected => {
            if route.is_current(event.generation) {
                sample(vec![(event.runtime_id, route)], cx);
            }
        }
        // 连接断了，守卫也就到头了：摘掉，路径清掉。重拨后状态会先变成离线再变回来，
        // 那时重新装上（见 `sync_link_watch`）。
        smelt_iroh::runtime::LinkChange::Closed => {
            if route.unwatch_if(event.generation) {
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
    let route = remote.route.clone();
    let runtime_id = remote.connection.runtime_id.clone();
    if !connected(status) {
        route.unwatch();
        return;
    }
    if route.watching() {
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
                route.watch(&watched_id, &events);
                // 选路可能在装上之前就定了，装上后先读一次。
                route.read()
            })
            .await;
        cx.update(|cx| apply_links(vec![(runtime_id, link)], cx));
    })
    .detach();
}

/// 现在就重读一台机器的连接路径和延迟：悬停在侧栏的机器名上时调。
pub(crate) fn sample_link(key: &RuntimeKey, cx: &mut gpui::App) {
    let Some(remote) = remote_of(key, cx) else {
        return;
    };
    if !connected(&remote.status) {
        return;
    }
    let target = vec![(remote.connection.runtime_id.clone(), remote.route.clone())];
    sample(target, cx);
}

/// 现在就重读各台连着的机器的连接路径和延迟：设置停在机器页时调。延迟只在有人看时采样，
/// 平时只在 iroh 换了路径时读。
pub(crate) fn sample_links(cx: &mut gpui::App) {
    let targets: Vec<_> = cx
        .try_global::<RuntimeHub>()
        .map(|hub| {
            hub.remotes
                .iter()
                .filter(|remote| connected(&remote.status))
                .map(|remote| (remote.connection.runtime_id.clone(), remote.route.clone()))
                .collect()
        })
        .unwrap_or_default();
    sample(targets, cx);
}

/// 后台读这几台机器的路径和延迟（不拨号），读完记到 hub 上。
fn sample(targets: Vec<(String, RemoteRoute)>, cx: &mut gpui::App) {
    if targets.is_empty() {
        return;
    }
    cx.spawn(async move |cx| {
        let links = cx
            .background_executor()
            .spawn(async move {
                targets
                    .into_iter()
                    .map(|(runtime_id, route)| (runtime_id, route.read()))
                    .collect::<Vec<_>>()
            })
            .await;
        cx.update(|cx| apply_links(links, cx));
    })
    .detach();
}

/// 把读到的连接路径记到各台机器上。没连着的不记（读回来时它可能已经断了）；没变化就不动
/// hub，免得看着它的侧栏跟着重画。
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
            // 配对时已经连上：之后订阅报的还是「在线」，`set_status` 看不出变化，就在这里挂。
            let online = added
                .as_ref()
                .ok()
                .filter(|remote| matches!(remote.status, RuntimeStatus::Online { .. }))
                .map(|remote| remote.runtime.clone());
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
            });
            if let Some(runtime) = online {
                crate::settings::attach_agent_host(
                    runtime.key().clone(),
                    runtime.client().clone(),
                    cx,
                    |_| {},
                );
            }
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
        smelt_home: None,
        added_at_ms: std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|elapsed| elapsed.as_millis() as i64)
            .unwrap_or_default(),
    };
    let (client, route) = remote_client(&connection)?;
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
    connection.smelt_home = described.daemon.smelt_home.clone();
    smelt_client_store::client_store()
        .and_then(|store| store.put_runtime_connection(&connection))
        .map_err(|error| format!("保存运行时失败：{error}"))?;
    Ok(RemoteRuntime {
        runtime: remote_runtime(&connection.runtime_id, client),
        status: described_status(described),
        connection,
        route,
        link: None,
        upgrade_note: None,
    })
}

/// 改一台机器在这台电脑上的显示名：先改 hub，侧栏和设置页立刻换上；客户端库在后台写。
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

/// 最近一次探测是在线。
pub(crate) fn runtime_online(key: &RuntimeKey, cx: &gpui::App) -> bool {
    status_of_key(key, cx).is_some_and(|status| matches!(status, RuntimeStatus::Online { .. }))
}

/// 这台电脑经哪个中继连这台远程机器（配对时记下、或之后改过的）。空的是 n0 的公共 relay。
pub(crate) fn runtime_relay(key: &RuntimeKey, cx: &gpui::App) -> String {
    remote_of(key, cx)
        .and_then(|remote| remote.connection.relay_url.clone())
        .unwrap_or_default()
}

/// 改一台远程机器的中继，`relay` 空着就是 n0 的公共 relay。
///
/// 在线：先请它自己改用新中继（它的远程配置），成了再改这边记下的地址。顺序不能反：先改
/// 这边就连不上它，没法再请它改。不在线：只改这边记下的——那台机器多半已经在那边改过
/// （`smeltd relay`），这边跟上才连得回去。改完立刻重连，开着的终端和对话下次开流就走
/// 新中继（见 [`RemoteRoute`]）。
pub(crate) fn set_runtime_relay(
    runtime_id: String,
    relay: String,
    cx: &mut gpui::App,
) -> gpui::Task<Result<(), String>> {
    use smelt_model::control_api::{RemoteConfigGet, RemoteConfigSet};
    use smelt_runtime_client::protocol::control::EmptyParams;

    let key = RuntimeKey::Remote(runtime_id.clone());
    let Some(remote) = remote_of(&key, cx).cloned() else {
        return gpui::Task::ready(Err("这台机器已经移除了".to_string()));
    };
    let relay = relay.trim().to_string();
    let parsed = match parse_relay(&relay) {
        Ok(parsed) => parsed,
        Err(error) => return gpui::Task::ready(Err(error)),
    };
    let online = matches!(remote.status, RuntimeStatus::Online { .. });
    let route = remote.route.clone();
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
                        .map_err(|error| format!("读不到那台机器的远程配置：{error}"))?;
                    if config.iroh_relay != relay {
                        config.iroh_relay = relay;
                        client
                            .call::<RemoteConfigSet>(&config, timeout)
                            .map_err(|error| format!("那台机器没改成：{error}"))?;
                    }
                }
                smelt_client_store::client_store()
                    .and_then(|store| store.put_runtime_connection(&saved))
                    .map_err(|error| format!("保存中继失败：{error}"))
            })
            .await?;
        cx.update(|cx| {
            route.retarget(parsed);
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
    // 在线才顺手请它吊销本机的配对。离线的连不上，拨号要等到超时，不能让移除跟着等；
    // 那台机器上的配对记录留着，要清就在那台机器上撤。
    let revoke = matches!(remote.status, RuntimeStatus::Online { .. });
    cx.spawn(async move |cx| {
        let removed = cx
            .background_executor()
            .spawn({
                let runtime_id = runtime_id.clone();
                async move {
                    smelt_client_store::client_store()
                        .and_then(|store| store.delete_runtime_connection(&runtime_id))
                        .map(|_| ())
                        .map_err(|error| format!("移除运行时失败：{error}"))
                }
            })
            .await;
        let deleted = removed.is_ok();
        cx.update(|cx| {
            update_hub(cx, |hub| match &removed {
                Ok(()) => hub
                    .remotes
                    .retain(|remote| remote.connection.runtime_id != runtime_id),
                Err(error) => hub.error = Some(error.clone()),
            });
            if removed.is_ok() {
                remote.route.unwatch();
                crate::settings::detach_agent_host(remote.runtime.key(), cx);
            }
        });
        if deleted && revoke {
            cx.background_executor()
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
                })
                .await;
        }
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
            smelt_home: None,
        }
    }

    /// 调用按 key 发给那台机器自己的客户端；机器移除了就取不到，不能悄悄退回本机。
    #[gpui::test]
    fn calls_resolve_to_the_keyed_machine(cx: &mut gpui::TestAppContext) {
        let far = RuntimeKey::Remote("far".into());
        cx.update(|cx| {
            assert!(client(&far, cx).is_none(), "还没挂机器时不该取到客户端");
            cx.set_global(RuntimeHub {
                remotes: vec![RemoteRuntime {
                    connection: smelt_client_store::RuntimeConnection {
                        runtime_id: "far".into(),
                        name: "far".into(),
                        endpoint_id: String::new(),
                        relay_url: None,
                        added_at_ms: 0,
                        smelt_home: Some("/home/far/.smelt".into()),
                    },
                    runtime: remote_runtime(
                        "far",
                        RuntimeClient::new(|| Err(std::io::Error::other("connected far"))),
                    ),
                    status: online(),
                    route: RemoteRoute::default(),
                    link: None,
                    upgrade_note: None,
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
            // 远程机器的 SMELT_HOME 取连上时记下的，跟本机的无关。
            assert_eq!(smelt_home(&far, cx), Some("/home/far/.smelt".to_string()));
            assert!(smelt_home(&RuntimeKey::Remote("gone".into()), cx).is_none());
        });
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

    /// 设置「机器」页里说全：走哪条路、多慢，慢了用警告色；版本不对时连上版本和连接路径。
    #[test]
    fn the_machine_panel_spells_the_link_out() {
        let status = online();
        assert_eq!(
            link_text(&status, Some(&link(LinkKind::Direct, 12))),
            ("直连 · 12ms".into(), LinkTone::Muted)
        );
        assert_eq!(link_text(&status, None), ("在线".into(), LinkTone::Muted));
        assert_eq!(
            link_text(
                &status,
                Some(&link(LinkKind::Relay("n0 usw1-1".into()), 437))
            ),
            ("中继 n0 usw1-1 · 437ms".into(), LinkTone::Warning)
        );
        assert_eq!(
            link_text(&RuntimeStatus::Offline("断了".into()), None),
            ("离线".into(), LinkTone::Muted)
        );
        let incompatible = RuntimeStatus::Incompatible {
            version: "0.9.0-rc.3".into(),
            detail: "运行时 0.9.0-rc.3 较旧，需要升级运行时（控制协议 v1，桌面要 v2）".into(),
            runtime_outdated: true,
        };
        assert_eq!(
            link_text(
                &incompatible,
                Some(&link(LinkKind::Relay("n0 usw1-1".into()), 437))
            ),
            (
                "版本不兼容 · 0.9.0-rc.3 · 中继 n0 usw1-1 · 437ms".into(),
                LinkTone::Warning
            )
        );
    }

    /// 侧栏标题一盏灯：在线绿、慢或过程中黄、用不了红；要人处理的灯旁写字，悬停看全。
    #[test]
    fn the_sidebar_light_follows_the_link() {
        let fast = indicator(
            &online(),
            Some(&link(LinkKind::Relay("n0 usw1-1".into()), 60)),
        );
        assert_eq!(fast.light, LinkLight::Good);
        assert_eq!(fast.label, None);
        assert_eq!(fast.detail, "中继 n0 usw1-1 · 60ms");

        let slow = indicator(
            &online(),
            Some(&link(LinkKind::Relay("n0 usw1-1".into()), 628)),
        );
        assert_eq!(slow.light, LinkLight::Degraded);
        assert_eq!(slow.label, None);

        let offline = indicator(&RuntimeStatus::Offline("connection refused".into()), None);
        assert_eq!(offline.light, LinkLight::Down);
        assert_eq!(offline.label.as_deref(), Some("离线"));
        assert_eq!(offline.detail, "离线：connection refused");

        let incompatible = indicator(
            &RuntimeStatus::Incompatible {
                version: "0.9.0".into(),
                detail: String::new(),
                runtime_outdated: true,
            },
            None,
        );
        assert_eq!(incompatible.light, LinkLight::Down);
        assert_eq!(incompatible.label.as_deref(), Some("版本不兼容 · 0.9.0"));

        let upgrading = indicator(&RuntimeStatus::Upgrading("正在升级".into()), None);
        assert_eq!(upgrading.light, LinkLight::Degraded);
        assert_eq!(upgrading.label.as_deref(), Some("升级中"));
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

    /// 旧运行时少了桌面要用的方法：标成不兼容并说要升级运行时，而不是显示在线、
    /// 之后各处零散地报 MethodNotFound。
    #[test]
    fn older_runtime_is_incompatible() {
        let RuntimeStatus::Incompatible {
            version,
            detail,
            runtime_outdated,
        } = status_of(&describe_without("workspace.load"))
        else {
            panic!("缺方法的运行时应判为不兼容");
        };
        assert!(runtime_outdated, "运行时旧，可以请它升级");
        assert_eq!(version, "0.0.1");
        assert!(detail.contains("0.0.1"), "{detail}");
        assert!(detail.contains("需要升级运行时"), "{detail}");
        assert!(detail.contains("workspace.load"), "{detail}");
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
            runtime_outdated,
        } = status_of(&client)
        else {
            panic!("控制协议对不上应判为不兼容");
        };
        assert!(!runtime_outdated, "运行时比桌面新，只升不降，不能请它升级");
        assert_eq!(version, "9.9.9");
        assert!(detail.contains("需要升级桌面"), "{detail}");
    }

    /// 订阅线程把每次探测到的连通性报回来，订阅断开也报：侧栏的「离线」靠它更新，
    /// 不能只在启动时探一次。
    #[test]
    fn subscription_reports_reachability_changes() {
        let (events, _events_rx) = smol::channel::unbounded();
        let (status, status_rx) = smol::channel::unbounded();
        let handle = crate::runtime_events::spawn_subscription(
            remote_runtime("rt-test", describe_without("no.such.method")),
            events,
            status,
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
            described.events.as_mut().unwrap().projection_version += 1;
            serde_json::to_value(ControlResponse::ok(request_id, described)).unwrap()
        });
        let (events, _events_rx) = smol::channel::unbounded();
        let (status, status_rx) = smol::channel::unbounded();
        let handle = crate::runtime_events::spawn_subscription(
            remote_runtime("rt-new", newer),
            events,
            status,
        );
        let (_, first) = status_rx.recv_blocking().unwrap();
        assert!(
            matches!(first, RuntimeStatus::Incompatible { .. }),
            "{first:?}"
        );

        handle.reconnect();
        // 首档退避 30 秒；叫醒了就不用等。
        let again = smol::block_on(smol::future::or(
            async { status_rx.recv().await.ok() },
            async {
                smol::Timer::after(std::time::Duration::from_secs(5)).await;
                None
            },
        ));
        assert!(
            matches!(again, Some((_, RuntimeStatus::Incompatible { .. }))),
            "{again:?}"
        );
        handle.stop();
    }

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
        assert_eq!(
            push_upgrade(&runtime_replying(&[])),
            PushUpgrade::Unsupported
        );
    }

    /// 接下以后断开：停机重启时旧进程退出就是这样，要重新探测。
    #[test]
    fn hanging_up_after_accepting_means_go_look_again() {
        assert_eq!(
            push_upgrade(&runtime_replying(&[r#"{"state":"accepted"}"#])),
            PushUpgrade::Restarting
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
