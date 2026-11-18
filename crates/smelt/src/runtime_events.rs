//! 每个运行时一条常驻事件订阅（设计文档 P2）。阻塞的 socket 读循环放专门的 OS 线程，
//! 断线、运行时没起来就退避重连；事件打上所属运行时，送进同一个处理循环。本机那条随
//! 应用常驻，远程的跟着 [`RuntimeHub`] 增删。
//!
//! 每次连之前先对版本：对不上就不订阅，把原因记到 [`RuntimeHub`] 上给人看，丢掉它的会话
//! 状态镜像，隔一阵再探，对上了才订阅。不然订阅解不出投影就断，断了又连，会话状态永远
//! 到不了，也没人知道为什么。
//!
//! 本机那条还看着本机守护：没在跑（没起来过，或者运行中退出了）就按 App 里的版本拉起；
//! 版本对不上（App 更新跨过了协议版本，旧守护还在跑）就立刻请它交接到 App 里的版本，
//! 不等空闲——对不上时桌面什么都用不了，等也没有意义。
//!
//! 连不上和对不上都退避重探；「重新连接」（[`reconnect`]）叫醒线程立刻再试，退避从头算。
//! 远程机器回来不会通知这台桌面，离线时的退避翻倍到分钟级；应用回到前台、打开设置时
//! （[`probe_remotes`]）叫醒连不上的再探一次。
//!
//! 订阅线程是远程运行时连通性的唯一来源：每次探测的结果、订阅断开，都报回 [`RuntimeHub`]，
//! 插件看到的「离线」跟着它走，不是只在启动时探一次。
//!
//! 运行时比桌面旧时先请它升到桌面的版本（设计文档 D7，见 [`upgrade_then_probe`]），升完
//! 再探；每条订阅线程只请一次，升了还对不上就照常标不兼容，不反复请，人点「重新连接」
//! 才再请一次。人也可以随时点「升级运行时」（[`upgrade_remote`]）：连得上就请，不管兼不兼容。
//!
//! 核心订阅以外的话题由插件登记（`smelt_plugin_api::register_topics`），宿主不认识它们：
//! 按所有插件要的并集，每一轮在核心订阅之前每个话题单独订一条，核心订阅断了就关掉，下一轮
//! 重开；一轮当中登记变了，就地增减（见 [`OpenTopics`]）。运行时不认识某个话题时只拒绝那条
//! 单独订阅，核心订阅和其他话题不受影响。本机外观回执请求宿主自己要处理，也走独立订阅。

use rust_i18n::t;
use std::collections::{BTreeMap, HashMap};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::{Duration, Instant};

use gpui::{App, Global, Subscription};

use crate::runtimes::{IncompatibleFix, Runtime, RuntimeHub, RuntimeKey, RuntimeStatus};
use smelt_client::daemon_state::{
    DaemonStateEvent, TopicMessage, TopicSubscription, TopicSubscriptionCloser,
    daemon_reconnect_backoff, subscribe_daemon_states_blocking,
};

pub(crate) type RuntimeEvent = (RuntimeKey, DaemonStateEvent);
/// 单独订阅收到的：哪台运行时、哪条订阅的话题。宿主不解读，原样转给插件
/// （`HostEvent::RuntimeTopic`）。
pub(crate) type RuntimeTopicEvent = (RuntimeKey, String, TopicMessage);

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

/// 插件要订的话题和各自的序号（`smelt_plugin_api::subscribed_topics`），主线程写、订阅线程读。
#[derive(Clone, Default)]
pub(crate) struct WantedTopics(Arc<Mutex<BTreeMap<String, u64>>>);

impl WantedTopics {
    fn get(&self) -> BTreeMap<String, u64> {
        lock(&self.0).clone()
    }

    fn set(&self, topics: BTreeMap<String, u64>) {
        *lock(&self.0) = topics;
    }
}

/// 一条订阅线程这一轮开着的话题订阅；不在一轮里（离线、版本对不上、断开后退避中）是 `None`。
type RoundTopics = Arc<Mutex<Option<OpenTopics>>>;

/// 一轮里开着的话题订阅，每个话题一条：后加的话题旧运行时不认，会拒掉整条订阅，不能连累
/// 别的。各记着订上时的序号。
struct OpenTopics {
    runtime: Runtime,
    sender: smol::channel::Sender<RuntimeTopicEvent>,
    open: BTreeMap<String, (u64, SeparateSubscription)>,
}

impl OpenTopics {
    fn new(runtime: &Runtime, sender: &smol::channel::Sender<RuntimeTopicEvent>) -> Self {
        Self {
            runtime: runtime.clone(),
            sender: sender.clone(),
            open: BTreeMap::new(),
        }
    }

    /// 关掉不要了的，订上新要的；开着的话题又有插件新要了（序号变大）就重订，新来的也拿到
    /// 快照（见 `smelt_plugin_api` 的 topic 模块文档）。阻塞（连运行时），不在主线程调。上一次
    /// 没连上的这次再试。
    fn reconcile(&mut self, wanted: &BTreeMap<String, u64>) {
        let opened = self
            .open
            .iter()
            .map(|(topic, (requested, _))| (topic, *requested));
        for topic in stale_topics(opened, wanted) {
            let Some((_, subscription)) = self.open.remove(&topic) else {
                continue;
            };
            subscription.close();
            if wanted.contains_key(&topic) {
                // 原来订着的插件在重订的空当里可能错过事件：按积压处理，没有快照的话题重读，
                // 带快照的等重订上的那份。
                let _ =
                    self.sender
                        .try_send((self.runtime.key().clone(), topic, TopicMessage::Lagged));
            }
        }
        for (topic, requested) in wanted {
            if self.open.contains_key(topic) {
                continue;
            }
            let key = self.runtime.key().clone();
            let sender = self.sender.clone();
            let name = topic.clone();
            let opened = SeparateSubscription::open(&self.runtime, topic.clone(), move |message| {
                sender
                    .try_send((key.clone(), name.clone(), message))
                    .is_ok()
            });
            if let Some(subscription) = opened {
                self.open.insert(topic.clone(), (*requested, subscription));
            }
        }
    }

    fn close(self) {
        for (_, (_, subscription)) in self.open {
            subscription.close();
        }
    }
}

/// 开着的话题里要关掉的：没人要了的，和订上之后又有插件新要了、得重订的。
fn stale_topics<'a>(
    opened: impl Iterator<Item = (&'a String, u64)>,
    wanted: &BTreeMap<String, u64>,
) -> Vec<String> {
    opened
        .filter(|(topic, opened)| {
            wanted
                .get(*topic)
                .is_none_or(|requested| requested > opened)
        })
        .map(|(topic, _)| topic.clone())
        .collect()
}

/// 事件的接收端：核心订阅的，和单独订阅的。
pub(crate) struct RuntimeEventReceivers {
    pub(crate) states: smol::channel::Receiver<RuntimeEvent>,
    pub(crate) topics: smol::channel::Receiver<RuntimeTopicEvent>,
    /// 命令行转来的 App 命令，只有本机运行时有。
    pub(crate) commands: smol::channel::Receiver<smelt_event_protocol::AppCommandRequested>,
}

/// 核心事件和单独话题的发送端，每条订阅线程一份。
#[derive(Clone)]
pub(crate) struct RuntimeEventSenders {
    pub(crate) states: smol::channel::Sender<RuntimeEvent>,
    pub(crate) topics: smol::channel::Sender<RuntimeTopicEvent>,
    pub(crate) commands: smol::channel::Sender<smelt_event_protocol::AppCommandRequested>,
}

struct RuntimeSubscriptions {
    sender: RuntimeEventSenders,
    /// 插件要订的话题，各条订阅线程共用。
    wanted: WantedTopics,
    /// 订阅线程把远程运行时的连通性报给主线程：哪个运行时、现在怎样。
    status: smol::channel::Sender<(RuntimeKey, RuntimeStatus)>,
    /// 本机那条。
    local: SubscriptionHandle,
    remotes: HashMap<RuntimeKey, SubscriptionHandle>,
    _hub_watch: Option<Subscription>,
    /// 插件登记的话题变了就增减订阅（[`sync_topics`]）。
    _topics_watch: Subscription,
    /// 系统睡醒：换掉远程机器共用的 iroh endpoint，见 [`crate::runtimes::reset_iroh_after_wake`]。
    _system_wake: Subscription,
}

/// 叫醒退避中的订阅线程。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Wake {
    /// 立刻再探一次。退避不从头算：还是连不上就接着按原来的间隔等，应用每回前台一次
    /// 就只多探一次，不会又从 250ms 起密集地拨。
    Probe,
    /// 人点了「重新连接」：再探，退避从头算，运行时旧就再请它升级一次（每条线程本来
    /// 只请一次）。
    Reconnect,
}

/// 一条订阅线程的把手：叫醒它，或停下它。把手丢了线程也停。
pub(crate) struct SubscriptionHandle {
    stop: Arc<AtomicBool>,
    wake: std::sync::mpsc::Sender<Wake>,
    /// 这条线程眼下这一轮开着的话题订阅。
    round: RoundTopics,
}

impl SubscriptionHandle {
    fn wake(&self, wake: Wake) {
        let _ = self.wake.send(wake);
    }

    /// 见 [`reconnect`]。
    pub(crate) fn reconnect(&self) {
        self.wake(Wake::Reconnect);
    }

    pub(crate) fn stop(&self) {
        self.stop.store(true, Ordering::Release);
    }
}

/// 版本对不上要等人去升级一边，不必勤探：从 30 秒起翻倍，封顶 10 分钟。
// 轮询例外：版本对不上要等有人去升级某一边，升级完不会通知这台桌面，只能隔一阵再探；
// 周期 30s 起翻倍，上限 10min。
fn incompatible_backoff(attempt: u32) -> Duration {
    (Duration::from_secs(30) * 2u32.pow(attempt.min(5))).min(Duration::from_secs(600))
}

/// 远程机器连不上时隔多久再探：250ms 起翻倍，封顶 5 分钟。刚断的（升级重启、换网络）
/// 几秒内就能接回；关机好几天的，一小时只拨十来次。
// 轮询例外：远程机器开机、联网后不会通知这台桌面，连上之前收不到它的任何东西，只能隔一阵
// 拨一次；每次拨号都要过 relay 和地址发现，关机时一直勤探白白耗电耗流量，所以封顶拉到分钟
// 级，应用回到前台、打开设置（[`probe_remotes`]）和「重新连接」会立刻叫醒；周期 250ms
// 起翻倍，上限 5min。
fn remote_offline_backoff(attempt: u32) -> Duration {
    (Duration::from_millis(250) * 2u32.pow(attempt.min(11))).min(Duration::from_secs(300))
}

/// 请这台运行时的订阅线程立刻重连（机器菜单的「重新连接」）。
pub(crate) fn reconnect(key: &RuntimeKey, cx: &App) {
    if let Some(handle) = cx
        .try_global::<RuntimeSubscriptions>()
        .and_then(|subscriptions| subscriptions.remotes.get(key))
    {
        handle.reconnect();
    }
}

/// 人点了「升级运行时」：不管现在兼不兼容，连得上就请它升到这个桌面的版本。
///
/// 不经订阅线程：在线时它正阻塞在事件流上，叫不醒。升级走守护协议的 `runtime_upgrade`，
/// 另开一条连接发，版本对不上也发得过去。升上去了运行时会重启，订阅线程自己察觉断线、
/// 重连、再探；没升成就当场再探一次，把状态恢复成探到的样子，原因记在这台机器上。
pub(crate) fn upgrade_remote(key: &RuntimeKey, cx: &mut App) {
    use crate::runtimes::{DESKTOP_VERSION, PushUpgrade};

    let Some(runtime) = crate::runtimes::runtime(key, cx) else {
        return;
    };
    let Some(status_tx) = cx
        .try_global::<RuntimeSubscriptions>()
        .map(|subscriptions| subscriptions.status.clone())
    else {
        return;
    };
    crate::runtimes::set_upgrade_note(key, None, cx);
    let _ = status_tx.try_send((
        key.clone(),
        RuntimeStatus::Upgrading(
            t!("upgrade.upgrading_to", version = DESKTOP_VERSION).into_owned(),
        ),
    ));
    let key = key.clone();
    cx.spawn(async move |cx| {
        let client = runtime.client().clone();
        let (outcome, status) = cx
            .background_executor()
            .spawn(async move {
                let outcome = crate::runtimes::push_upgrade(&client);
                // 升上去了它正在重启，这会儿探只会是离线，留给订阅线程。
                let status = (outcome != PushUpgrade::Restarting)
                    .then(|| crate::runtimes::status_of(&client));
                (outcome, status)
            })
            .await;
        let note = match outcome {
            PushUpgrade::Restarting => None,
            PushUpgrade::Current => {
                Some(t!("upgrade.current", version = DESKTOP_VERSION).into_owned())
            }
            PushUpgrade::Staged(reason) => Some(t!("upgrade.staged", reason = reason).into_owned()),
            PushUpgrade::Unsupported => Some(
                t!(
                    "upgrade.unsupported",
                    command = crate::runtimes::manual_upgrade_command()
                )
                .into_owned(),
            ),
            PushUpgrade::Failed(error) => Some(t!("upgrade.failed", error = error).into_owned()),
        };
        let restarting = note.is_none();
        let _ = status_tx.try_send((
            key.clone(),
            status
                .unwrap_or_else(|| RuntimeStatus::Upgrading(t!("upgrade.restarting").into_owned())),
        ));
        cx.update(|cx| {
            crate::runtimes::set_upgrade_note(&key, note, cx);
            // 版本对不上时订阅线程睡在退避里，可能要等好几分钟：叫醒它照新版本再探。
            if restarting
                && let Some(handle) = cx
                    .try_global::<RuntimeSubscriptions>()
                    .and_then(|subscriptions| subscriptions.remotes.get(&key))
            {
                handle.wake(Wake::Probe);
            }
        });
    })
    .detach();
}

/// 请连不上、版本对不上的远程运行时立刻再探一次（打开设置、应用回到前台时），不重新
/// 请求升级。在线的连着订阅，断了自己会知道；连接中、升级中的正在探，都不用叫。
pub(crate) fn probe_remotes(cx: &App) {
    if let Some(subscriptions) = cx.try_global::<RuntimeSubscriptions>() {
        for (key, handle) in &subscriptions.remotes {
            if crate::runtimes::runtime_can_reconnect(key, cx) {
                handle.wake(Wake::Probe);
            }
        }
    }
}

impl Global for RuntimeSubscriptions {}

/// 起本机和已登记远程运行时的订阅，返回事件的接收端。要在 [`RuntimeHub`] 装好之后调。
pub(crate) fn start(cx: &mut App) -> RuntimeEventReceivers {
    let (states, states_rx) = smol::channel::unbounded();
    let (topics, topics_rx) = smol::channel::unbounded();
    let (commands, commands_rx) = smol::channel::unbounded();
    let sender = RuntimeEventSenders {
        states,
        topics,
        commands,
    };
    let (status, status_rx) = smol::channel::unbounded::<(RuntimeKey, RuntimeStatus)>();
    cx.spawn(async move |cx| {
        while let Ok((key, status)) = status_rx.recv().await {
            cx.update(|cx| {
                // 刚变成对不上：不订阅了，它的会话状态镜像和移除时一样丢掉，对上了重新订阅再建。
                if matches!(status, RuntimeStatus::Incompatible { .. })
                    && !crate::runtimes::runtime_incompatible(&key, cx)
                {
                    crate::DaemonStates::forget_runtime(&key, cx);
                }
                crate::runtimes::set_status(&key, status, cx);
            });
        }
    })
    .detach();
    // 插件在这之前已经登记好（`plugin_host::install`），本机这条第一轮就订上它们要的话题。
    let wanted = WantedTopics::default();
    wanted.set(smelt_plugin_api::subscribed_topics(cx));
    let local = spawn_subscription(
        Runtime::local(),
        sender.clone(),
        status.clone(),
        wanted.clone(),
    );
    let system_wake = cx.on_system_wake(crate::runtimes::reset_iroh_after_wake);
    let topics_watch = cx.subscribe(&smelt_plugin_api::events(cx), |_, event, cx| {
        if matches!(event, smelt_plugin_api::HostEvent::TopicsChanged) {
            sync_topics(cx);
        }
    });
    cx.set_global(RuntimeSubscriptions {
        sender,
        wanted,
        status,
        local,
        remotes: HashMap::new(),
        _hub_watch: None,
        _topics_watch: topics_watch,
        _system_wake: system_wake,
    });
    sync_remote_subscriptions(cx);
    let watch = cx.observe_global::<RuntimeHub>(sync_remote_subscriptions);
    cx.global_mut::<RuntimeSubscriptions>()._hub_watch = Some(watch);
    RuntimeEventReceivers {
        states: states_rx,
        topics: topics_rx,
        commands: commands_rx,
    }
}

/// 外观请求单独订阅，不让新话题进入 v7 核心订阅。丢事件时请求方会得到结果未知，
/// 不重放可能已经执行过的操作。
/// App 命令请求也单独订阅，理由和外观一样；丢了的请求方得到结果未知。
fn app_command_event(message: TopicMessage) -> Option<smelt_event_protocol::AppCommandRequested> {
    match message {
        TopicMessage::Event { topic, payload }
            if topic == smelt_event_protocol::CORE_TOPIC_APP_COMMAND_REQUESTED =>
        {
            serde_json::from_value(payload)
                .inspect_err(|error| eprintln!("[runtime-events] App 命令请求载荷无效：{error}"))
                .ok()
        }
        TopicMessage::Lagged => {
            eprintln!("[runtime-events] App 命令请求订阅积压，丢失的请求不会重放");
            None
        }
        _ => None,
    }
}

fn appearance_event(message: TopicMessage) -> Option<DaemonStateEvent> {
    match message {
        TopicMessage::Event { topic, payload }
            if topic == smelt_event_protocol::CORE_TOPIC_APPEARANCE_APPLY_REQUESTED =>
        {
            match serde_json::from_value::<smelt_event_protocol::AppearanceApplyRequested>(payload)
            {
                Ok(request) => Some(DaemonStateEvent::AppearanceApplyRequested {
                    request_id: request.request_id,
                    settings: request.settings,
                }),
                Err(error) => {
                    eprintln!("[runtime-events] 外观请求载荷无效：{error}");
                    None
                }
            }
        }
        TopicMessage::Lagged => {
            eprintln!("[runtime-events] 外观请求订阅积压，丢失的请求不会重放");
            None
        }
        TopicMessage::Unavailable(reason) => {
            eprintln!("[runtime-events] 收不到外观请求：{reason}");
            None
        }
        TopicMessage::Event { .. } | TopicMessage::Snapshot { .. } => None,
    }
}

/// 单独订阅停下的原因，给人看。
fn unavailable_reason(error: &smelt_client::daemon_state::DaemonEventClientError) -> String {
    use smelt_client::daemon_state::DaemonEventClientError;
    match error {
        DaemonEventClientError::Server {
            code: smelt_event_protocol::SubscribeErrorCode::Rejected,
            ..
        } => t!("runtime.events_rejected").into_owned(),
        error => t!("runtime.events_failed", error = error).into_owned(),
    }
}

/// 跟着一轮核心订阅的单独订阅：读在自己的线程里，[`Self::close`] 关掉连接并等它退出。
///
/// 连接自己断了（核心订阅还连着）就在线程里退避重订，订上后先交一条
/// [`TopicMessage::Lagged`]：断开期间可能错过了推送，收的一方当作每个话题都推过一次。
/// 运行时拒掉订阅（多半是比这些话题旧）或推来的解不开就交一条
/// [`TopicMessage::Unavailable`] 后停下，等下一轮核心订阅再试。
struct SeparateSubscription {
    shared: Arc<SeparateShared>,
    reader: std::thread::JoinHandle<()>,
}

/// 读线程和 [`SeparateSubscription::close`] 共用。
struct SeparateShared {
    state: std::sync::Mutex<SeparateState>,
    /// 关掉时叫醒退避中的读线程。
    closed: std::sync::Condvar,
}

struct SeparateState {
    closed: bool,
    /// 眼下那条连接；重订后换成新的。
    closer: Option<TopicSubscriptionCloser>,
}

impl SeparateShared {
    fn lock(&self) -> std::sync::MutexGuard<'_, SeparateState> {
        self.state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }
}

/// 连上一条单独订阅，连同关它的把手。
fn open_topic(
    client: &smelt_runtime_client::RuntimeClient,
    topic: &str,
) -> Result<(TopicSubscriptionCloser, TopicSubscription), String> {
    TopicSubscription::open(client, vec![topic])
        .and_then(|subscription| Ok((subscription.closer()?, subscription)))
}

impl SeparateSubscription {
    /// 第一次连不上、起不了线程就不订，记日志；下一轮核心订阅再试。
    fn open(
        runtime: &Runtime,
        topic: String,
        mut emit: impl FnMut(TopicMessage) -> bool + Send + 'static,
    ) -> Option<Self> {
        let key = runtime.key().clone();
        let client = runtime.client().clone();
        let (closer, first) = match open_topic(&client, &topic) {
            Ok(opened) => opened,
            Err(error) => {
                eprintln!("[runtime-events] {key:?} 单独订阅连不上：{error}");
                return None;
            }
        };
        let shared = Arc::new(SeparateShared {
            state: std::sync::Mutex::new(SeparateState {
                closed: false,
                closer: Some(closer),
            }),
            closed: std::sync::Condvar::new(),
        });
        let thread_shared = Arc::clone(&shared);
        let thread_key = key.clone();
        let reader = std::thread::Builder::new()
            .name(format!("smelt-topics-{key:?}"))
            .spawn(move || {
                let shared = thread_shared;
                let mut current = Some(first);
                let mut attempt = 0u32;
                loop {
                    if let Some(subscription) = current.take() {
                        let started = Instant::now();
                        let mut receiver_gone = false;
                        let result = subscription.run(|message| {
                            let kept = emit(message);
                            receiver_gone |= !kept;
                            kept
                        });
                        if let Err(error) = result {
                            eprintln!("[runtime-events] {thread_key:?} 单独订阅停了：{error}");
                            emit(TopicMessage::Unavailable(unavailable_reason(&error)));
                            return;
                        }
                        if receiver_gone {
                            return;
                        }
                        // 连了一阵才断（运行时升级等）从首档退避。
                        if started.elapsed() >= Duration::from_secs(5) {
                            attempt = 0;
                        }
                    }
                    // 轮询例外：只有这条订阅断了时，对端能再连不会通知我们；周期 250ms 起
                    // 翻倍，上限 8s。关掉时由 Condvar 立刻叫醒。
                    let pause = daemon_reconnect_backoff(attempt);
                    attempt = attempt.saturating_add(1);
                    let state = shared.lock();
                    let (state, _) = shared
                        .closed
                        .wait_timeout_while(state, pause, |state| !state.closed)
                        .unwrap_or_else(std::sync::PoisonError::into_inner);
                    if state.closed {
                        return;
                    }
                    drop(state);
                    match open_topic(&client, &topic) {
                        Ok((closer, subscription)) => {
                            let mut state = shared.lock();
                            if state.closed {
                                closer.close();
                                return;
                            }
                            state.closer = Some(closer);
                            drop(state);
                            if !emit(TopicMessage::Lagged) {
                                return;
                            }
                            current = Some(subscription);
                        }
                        Err(error) => {
                            eprintln!("[runtime-events] {thread_key:?} 单独订阅重连失败：{error}");
                        }
                    }
                }
            });
        match reader {
            Ok(reader) => Some(Self { shared, reader }),
            Err(error) => {
                if let Some(closer) = shared.lock().closer.take() {
                    closer.close();
                }
                eprintln!("[runtime-events] {key:?} 起单独订阅线程失败：{error}");
                None
            }
        }
    }

    fn close(self) {
        {
            let mut state = self.shared.lock();
            state.closed = true;
            if let Some(closer) = state.closer.take() {
                closer.close();
            }
        }
        self.shared.closed.notify_all();
        let _ = self.reader.join();
    }
}

pub(crate) fn spawn_subscription(
    runtime: Runtime,
    sender: RuntimeEventSenders,
    status_tx: smol::channel::Sender<(RuntimeKey, RuntimeStatus)>,
    wanted: WantedTopics,
) -> SubscriptionHandle {
    let key = runtime.key().clone();
    let stop = Arc::new(AtomicBool::new(false));
    let (wake, woken) = std::sync::mpsc::channel();
    let round: RoundTopics = Arc::default();
    let handle = SubscriptionHandle {
        stop: Arc::clone(&stop),
        wake,
        round: Arc::clone(&round),
    };
    std::thread::Builder::new()
        .name(format!("smelt-events-{key:?}"))
        .spawn(move || {
            let mut reconnect_attempt = 0u32;
            let mut incompatible_attempt = 0u32;
            let mut upgrade_requested = false;
            while !stop.load(Ordering::Acquire) {
                let mut status = if runtime.is_local() {
                    crate::runtimes::local_status(runtime.client())
                } else {
                    crate::runtimes::status_of(runtime.client())
                };
                if let RuntimeStatus::Incompatible {
                    version,
                    detail,
                    fix,
                } = &status
                    && fix.runtime_outdated()
                {
                    let (version, detail, fix) = (version.clone(), detail.clone(), *fix);
                    if runtime.is_local() {
                        status = upgrade_local_then_probe(
                            &runtime, &version, &detail, fix, &status_tx, &stop,
                        );
                    } else if fix == IncompatibleFix::UpdateSmeltThere {
                        // 请不动它：说清到哪儿去更新，不发注定失败的升级请求。
                        status = RuntimeStatus::Incompatible {
                            version,
                            detail: t!(
                                "upgrade.update_smelt_there",
                                detail = detail,
                                version = crate::runtimes::DESKTOP_VERSION
                            )
                            .into_owned(),
                            fix,
                        };
                    } else if !upgrade_requested && smelt_version::RELEASE.is_some() {
                        // 开发构建没有对应的发版可升，留着不兼容的说明。
                        upgrade_requested = true;
                        status = upgrade_then_probe(&runtime, &version, &detail, &status_tx, &stop);
                    }
                }
                let connected_at = Instant::now();
                let _ = status_tx.try_send((key.clone(), status.clone()));
                let pause = match status {
                    RuntimeStatus::Incompatible { .. } => {
                        reconnect_attempt = 0;
                        let pause = incompatible_backoff(incompatible_attempt);
                        incompatible_attempt = incompatible_attempt.saturating_add(1);
                        pause
                    }
                    // 连不上就不再拨一次订阅，直接退避。
                    RuntimeStatus::Offline(_) => {
                        incompatible_attempt = 0;
                        let pause = if runtime.is_local() {
                            // 轮询例外：本机守护拉不起来时没有「现在能起了」的事件，探一次就是
                            // 再拉一次；周期 250ms 起翻倍，上限 8s。
                            daemon_reconnect_backoff(reconnect_attempt)
                        } else {
                            remote_offline_backoff(reconnect_attempt)
                        };
                        reconnect_attempt = reconnect_attempt.saturating_add(1);
                        pause
                    }
                    _ => {
                        incompatible_attempt = 0;
                        // 插件要的话题分别订阅：旧运行时拒绝新话题时，不能连带其他话题或
                        // 会话状态一起断开。在核心订阅之前订上，插件收到重连时话题已经在推。
                        // 持着锁订，这一轮当中登记变了的增减（[`sync_topics`]）排在后面。
                        {
                            let mut current = lock(&round);
                            let mut topics = OpenTopics::new(&runtime, &sender.topics);
                            topics.reconcile(&wanted.get());
                            *current = Some(topics);
                        }
                        // 外观只由本机桌面处理。
                        let appearance = if runtime.is_local() {
                            let appearance_key = key.clone();
                            let state_sender = sender.states.clone();
                            SeparateSubscription::open(
                                &runtime,
                                smelt_event_protocol::CORE_TOPIC_APPEARANCE_APPLY_REQUESTED
                                    .to_string(),
                                move |message| match appearance_event(message) {
                                    Some(event) => state_sender
                                        .try_send((appearance_key.clone(), event))
                                        .is_ok(),
                                    None => true,
                                },
                            )
                        } else {
                            None
                        };
                        // 命令登记在桌面里，只接本机运行时转来的。
                        let commands = if runtime.is_local() {
                            let command_sender = sender.commands.clone();
                            SeparateSubscription::open(
                                &runtime,
                                smelt_event_protocol::CORE_TOPIC_APP_COMMAND_REQUESTED.to_string(),
                                move |message| match app_command_event(message) {
                                    Some(request) => command_sender.try_send(request).is_ok(),
                                    None => true,
                                },
                            )
                        } else {
                            None
                        };
                        subscribe_daemon_states_blocking(
                            runtime.client(),
                            runtime.is_local(),
                            |event| {
                                !stop.load(Ordering::Acquire)
                                    && sender.states.try_send((key.clone(), event)).is_ok()
                            },
                        );
                        if let Some(topics) = lock(&round).take() {
                            topics.close();
                        }
                        if let Some(appearance) = appearance {
                            appearance.close();
                        }
                        if let Some(commands) = commands {
                            commands.close();
                        }
                        if stop.load(Ordering::Acquire) {
                            return;
                        }
                        // 订阅断了：这会儿算连不上，退避后再探测时连上了会报回在线。本机守护
                        // 是退出了的话，下一轮探测会把它拉起来。
                        let _ = status_tx.try_send((
                            key.clone(),
                            RuntimeStatus::Offline(t!("runtime.disconnected").into_owned()),
                        ));
                        let _ = sender
                            .states
                            .try_send((key.clone(), DaemonStateEvent::Disconnected));
                        // 短暂可用后断开（daemon 升级）从首档开始；连续失败则指数退避并封顶，
                        // 避免运行时没起来时固定节拍反复去连。
                        if connected_at.elapsed() >= Duration::from_secs(5) {
                            reconnect_attempt = 0;
                        }
                        // 轮询例外：订阅断了，对端重新能连时不会通知我们，只能隔一会儿再探；
                        // 周期 250ms 起翻倍，上限 8s（再探仍连不上就转到上面离线的退避）。
                        let pause = daemon_reconnect_backoff(reconnect_attempt);
                        reconnect_attempt = reconnect_attempt.saturating_add(1);
                        pause
                    }
                };
                // 退避等待可被叫醒；把手丢了（运行时被移除）就停。
                let wake = match woken.recv_timeout(pause) {
                    Ok(wake) => wake,
                    Err(std::sync::mpsc::RecvTimeoutError::Timeout) => continue,
                    Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => return,
                };
                // 攒着的几次叫醒并成一次，有人点过「重新连接」就按它算。只是再探一次
                // （回到前台、打开设置）的不动退避，见 [`Wake::Probe`]。
                let reconnect = std::iter::once(wake)
                    .chain(woken.try_iter())
                    .any(|wake| wake == Wake::Reconnect);
                if reconnect {
                    reconnect_attempt = 0;
                    incompatible_attempt = 0;
                    upgrade_requested = false;
                    // 拨号连不上要等到拨号时限才知道，这期间灯还停在「离线」，人会以为没点上。
                    let _ = status_tx.try_send((key.clone(), RuntimeStatus::Connecting));
                }
            }
        })
        .expect("spawn smelt-events 线程");
    handle
}

/// 本机守护版本对不上：停掉旧进程并拉起 App 里的那份，再探一次。
/// 进行中的回合会中断。只试一次，不再因为忙碌反复等。
fn upgrade_local_then_probe(
    runtime: &Runtime,
    version: &str,
    detail: &str,
    fix: IncompatibleFix,
    status_tx: &smol::channel::Sender<(RuntimeKey, RuntimeStatus)>,
    _stop: &AtomicBool,
) -> RuntimeStatus {
    use crate::local_daemon::UpgradeOutcome;

    let _ = status_tx.try_send((
        runtime.key().clone(),
        RuntimeStatus::Upgrading(t!("upgrade.restarting_local").into_owned()),
    ));
    match crate::local_daemon::upgrade_daemon() {
        UpgradeOutcome::Upgraded => crate::runtimes::local_status(runtime.client()),
        UpgradeOutcome::Failed => RuntimeStatus::Incompatible {
            version: version.to_string(),
            detail: t!("upgrade.local_failed", detail = detail).into_owned(),
            fix,
        },
    }
}

/// 请运行时升到这个桌面的版本，等它换好再探一次，返回探到的状态。升级没成就在原来的
/// 不兼容说明后面写上为什么、该怎么办。
fn upgrade_then_probe(
    runtime: &Runtime,
    version: &str,
    detail: &str,
    status_tx: &smol::channel::Sender<(RuntimeKey, RuntimeStatus)>,
    stop: &AtomicBool,
) -> RuntimeStatus {
    use crate::runtimes::{DESKTOP_VERSION, PushUpgrade, push_upgrade, status_of};

    let key = runtime.key().clone();
    let report = |progress: String| {
        let _ = status_tx.try_send((key.clone(), RuntimeStatus::Upgrading(progress)));
    };
    let still_incompatible = |note: String| RuntimeStatus::Incompatible {
        version: version.to_string(),
        detail: t!("upgrade.detail_note", detail = detail, note = note).into_owned(),
        fix: IncompatibleFix::UpgradeRuntime,
    };
    report(t!("upgrade.upgrading_to", version = DESKTOP_VERSION).into_owned());
    match push_upgrade(runtime.client()) {
        PushUpgrade::Restarting => {}
        // 老运行时正忙时会先装好、等空闲再换。
        PushUpgrade::Staged(reason) => {
            return still_incompatible(reason);
        }
        PushUpgrade::Current => {
            return still_incompatible(
                t!("upgrade.still_mismatched_dev", version = DESKTOP_VERSION).into_owned(),
            );
        }
        PushUpgrade::Unsupported => {
            return still_incompatible(
                t!(
                    "upgrade.unsupported",
                    command = crate::runtimes::manual_upgrade_command()
                )
                .into_owned(),
            );
        }
        PushUpgrade::Failed(error) => {
            return still_incompatible(t!("upgrade.auto_failed", error = error).into_owned());
        }
    }
    // 重启时旧进程连同 iroh 端点一起退出，新进程重新上线要几秒。
    const RESTART_GRACE: Duration = Duration::from_secs(120);
    let started = Instant::now();
    loop {
        // 轮询例外：重启中的运行时连同 iroh 端点一起下线，新进程上线不会通知这台桌面，只能
        // 隔一会儿探一次；周期 2s，上限 120s。
        std::thread::sleep(Duration::from_secs(2));
        let status = status_of(runtime.client());
        // 连不上就是还在重启；连上了还是旧版本，就是没换成。
        let pending =
            matches!(status, RuntimeStatus::Offline(_)) && started.elapsed() < RESTART_GRACE;
        if !pending || stop.load(Ordering::Acquire) {
            return match status {
                RuntimeStatus::Incompatible {
                    version,
                    detail,
                    fix,
                } => RuntimeStatus::Incompatible {
                    version,
                    detail: t!(
                        "upgrade.still_mismatched",
                        detail = detail,
                        version = DESKTOP_VERSION
                    )
                    .into_owned(),
                    fix,
                },
                status => status,
            };
        }
    }
}

/// 插件登记的话题变了：记下新的并集，在后台把各条线程这一轮开着的订阅增减到它（订阅要连
/// 运行时，不在主线程做）。不在一轮里的线程不用管，下一轮开始时按新的并集订。
fn sync_topics(cx: &mut App) {
    let Some(subscriptions) = cx.try_global::<RuntimeSubscriptions>() else {
        return;
    };
    subscriptions
        .wanted
        .set(smelt_plugin_api::subscribed_topics(cx));
    let wanted = subscriptions.wanted.clone();
    let rounds: Vec<RoundTopics> = std::iter::once(&subscriptions.local)
        .chain(subscriptions.remotes.values())
        .map(|handle| Arc::clone(&handle.round))
        .collect();
    cx.background_executor()
        .spawn(async move {
            for round in rounds {
                // 先拿这一轮的锁再读并集：几次变化挤在一起时，后跑的按最新的增减。
                if let Some(topics) = lock(&round).as_mut() {
                    topics.reconcile(&wanted.get());
                }
            }
        })
        .detach();
}

/// 远程运行时的订阅跟着 RuntimeHub 走：新添加的起一条，移除的停掉并丢掉它的状态镜像。
/// 版本不兼容的线程不停，它自己隔一阵再探（见 [`spawn_subscription`]）。
fn sync_remote_subscriptions(cx: &mut App) {
    let Some(hub) = cx.try_global::<RuntimeHub>() else {
        return;
    };
    let remotes: Vec<Runtime> = hub
        .remotes
        .iter()
        .map(|remote| remote.runtime.clone())
        .collect();
    let subscriptions = cx.global_mut::<RuntimeSubscriptions>();
    let removed: Vec<RuntimeKey> = subscriptions
        .remotes
        .keys()
        .filter(|key| !remotes.iter().any(|runtime| runtime.key() == *key))
        .cloned()
        .collect();
    for key in &removed {
        if let Some(handle) = subscriptions.remotes.remove(key) {
            handle.stop();
        }
    }
    for runtime in remotes {
        if subscriptions.remotes.contains_key(runtime.key()) {
            continue;
        }
        let key = runtime.key().clone();
        let handle = spawn_subscription(
            runtime,
            subscriptions.sender.clone(),
            subscriptions.status.clone(),
            subscriptions.wanted.clone(),
        );
        subscriptions.remotes.insert(key, handle);
    }
    for key in removed {
        crate::DaemonStates::forget_runtime(&key, cx);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn appearance_topic_delivers_only_valid_requests() {
        let event = appearance_event(TopicMessage::Event {
            topic: smelt_event_protocol::CORE_TOPIC_APPEARANCE_APPLY_REQUESTED.to_string(),
            payload: serde_json::json!({
                "request_id": "request-1",
                "settings": {"theme": "test"},
            }),
        });
        assert!(matches!(
            event,
            Some(DaemonStateEvent::AppearanceApplyRequested { request_id, settings })
                if request_id == "request-1" && settings.get("theme") == Some(&serde_json::json!("test"))
        ));
        assert!(
            appearance_event(TopicMessage::Event {
                topic: smelt_event_protocol::CORE_TOPIC_APPEARANCE_APPLY_REQUESTED.to_string(),
                payload: serde_json::json!({"settings": {}}),
            })
            .is_none()
        );
        assert!(appearance_event(TopicMessage::Lagged).is_none());
        assert!(
            appearance_event(TopicMessage::Event {
                topic: "unrelated".into(),
                payload: serde_json::json!({}),
            })
            .is_none()
        );
    }

    #[test]
    fn topics_are_closed_when_dropped_and_renewed_when_requested_again() {
        let opened: BTreeMap<String, u64> = [("kept", 1), ("dropped", 2), ("renewed", 3)]
            .into_iter()
            .map(|(topic, requested)| (topic.to_string(), requested))
            .collect();
        let wanted: BTreeMap<String, u64> = [("kept", 1), ("renewed", 5), ("new", 6)]
            .into_iter()
            .map(|(topic, requested)| (topic.to_string(), requested))
            .collect();
        let stale = stale_topics(
            opened.iter().map(|(topic, requested)| (topic, *requested)),
            &wanted,
        );
        assert_eq!(stale, ["dropped", "renewed"]);
    }

    #[test]
    fn incompatible_backoff_doubles_up_to_ten_minutes() {
        let pauses: Vec<u64> = (0..8)
            .map(|attempt| incompatible_backoff(attempt).as_secs())
            .collect();
        assert_eq!(pauses, [30, 60, 120, 240, 480, 600, 600, 600]);
    }

    #[test]
    fn remote_offline_backoff_doubles_up_to_five_minutes() {
        let pauses: Vec<u128> = [0, 1, 2, 5, 9, 10, 11, 12, 100, u32::MAX]
            .into_iter()
            .map(|attempt| remote_offline_backoff(attempt).as_millis())
            .collect();
        assert_eq!(
            pauses,
            [
                250, 500, 1_000, 8_000, 128_000, 256_000, 300_000, 300_000, 300_000, 300_000
            ]
        );
    }
}
