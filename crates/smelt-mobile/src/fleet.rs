//! 机队：所有登记过的机器，每台一个节点，同时连着。Dart 入口见 `crate::api::fleet`。
//!
//! ```text
//! Fleet（进程级单例）
//!  ├─ Store        登记与草稿（业务数据，按版本迁移）
//!  ├─ Cache        会话列表、对话尾部（缓存，坏了重建）
//!  ├─ Transport    全进程一个 iroh endpoint，按所有机器的 relay 设置决定形态
//!  ├─ Node × N     每台机器：连接状态机 + 会话目录 + 对话 + 终端（经 NodeSource 共用这台的连接）
//!  └─ PassHub      跨机器投影：单据与灶台
//! ```
//!
//! 每台机器的连接状态机各自重连，互不影响；没有「当前运行时」。endpoint 形态只在配对、
//! 忘记、导入时可能变化：变了就让所有节点重新 connect（旧会话换代退出，各模块按代号丢旧
//! 状态或跟上新连接）。
//!
//! 锁的顺序：`ops` → `registry` → 连接状态机 → 会话目录 → `view` / 指挥台。连接状态机在
//! 自己的锁里回调监听（更新 `view`、通知会话目录），所以持有 `view` 时绝不调连接状态机；
//! 持有 `registry` 时不做网络和 SQLite IO。

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, LazyLock, Mutex, MutexGuard, OnceLock, Weak};

use crate::api::catalog::{CatalogEvent, CatalogEventKind, SessionList};
use crate::api::error::{SmeltError, SmeltErrorKind, error};
use crate::api::fleet::{
    ConnectionState, FleetRuntime, FleetView, RuntimeImport, RuntimeRecord, RuntimeStatus,
};
use crate::cache::Cache;
use crate::catalog::Catalog;
use crate::connection::{
    ConnectionEvent, ConnectionHub, CurrentRuntime, Dialer, Listener, RuntimeInfo, Target,
};
use crate::conversation::Conversations;
use crate::pass::{PassHub, PassListener};
use crate::store::Store;
use crate::terminal::TerminalHub;

/// 设备密钥文件名，和桌面的 `~/.smelt/client-iroh-secret` 同名。
const SECRET_FILE: &str = "client-iroh-secret";

/// 机队用的传输：除了拨号，还要按全部登记机器的 relay 决定 endpoint 形态。生产是
/// `IrohDialer`，测试注入假的。
pub(crate) trait Transport: Dialer {
    /// 按全部登记机器的 relay 调整形态，返回形态是否变了。变了时旧 endpoint 已丢弃，调用方
    /// 要让所有节点重新 connect。relay 不合法时报错、形态不变。
    fn configure(&self, relays: &[Option<String>]) -> Result<bool, SmeltError>;
}

/// 各模块的时限。生产值见 [`Timings::PRODUCTION`]，测试用毫秒级。
#[derive(Clone, Copy, Debug)]
pub(crate) struct Timings {
    pub connection: crate::connection::Timing,
    pub catalog: crate::catalog::Timing,
    pub conversation: crate::conversation::Timing,
    pub terminal: crate::terminal::Timing,
    pub pass: crate::pass::Timing,
}

impl Timings {
    pub(crate) const PRODUCTION: Self = Self {
        connection: crate::connection::Timing::PRODUCTION,
        catalog: crate::catalog::Timing::PRODUCTION,
        conversation: crate::conversation::Timing::PRODUCTION,
        terminal: crate::terminal::Timing::PRODUCTION,
        pass: crate::pass::Timing::PRODUCTION,
    };
}

/// `init_client` 之后才有的东西。
pub(crate) struct Env {
    pub data_dir: PathBuf,
    /// 这台设备的 endpoint id。
    pub device_id: String,
    pub store: Arc<Store>,
    /// 打不开时为空：只是离线时没有旧数据可看。
    pub cache: Option<Arc<Cache>>,
    pub transport: Arc<dyn Transport>,
}

// ---------------------------------------------------------------------------
// 节点
// ---------------------------------------------------------------------------

/// 一台机器。模块各自的逻辑不变，只是来源指向这台机器的连接状态机。
pub(crate) struct Node {
    pub runtime_id: String,
    pub target: Target,
    /// 区分同一 `runtime_id` 先后的节点（重新配对换了 endpoint 时会替换）：旧节点晚到的
    /// 状态不能盖掉新节点的。
    serial: u64,
    pub hub: Arc<ConnectionHub>,
    pub catalog: Catalog,
    pub conversations: Conversations,
    pub terminals: TerminalHub,
}

/// 节点上各模块的 `RuntimeSource`：`current()` 读本节点的连接状态机，`subscribe()` 挂本
/// 节点的状态，`cache()` 共用一份缓存。
struct NodeSource {
    hub: Arc<ConnectionHub>,
    cache: Option<Arc<Cache>>,
}

impl crate::catalog::RuntimeSource for NodeSource {
    fn current(&self) -> Option<CurrentRuntime> {
        self.hub.current()
    }

    fn subscribe(&self, listener: Listener) {
        self.hub.subscribe(listener);
    }

    fn cache(&self) -> Option<Arc<Cache>> {
        self.cache.clone()
    }
}

impl crate::conversation::RuntimeSource for NodeSource {
    fn current(&self) -> Option<CurrentRuntime> {
        self.hub.current()
    }

    fn subscribe(&self, listener: Listener) {
        self.hub.subscribe(listener);
    }

    fn cache(&self) -> Option<Arc<Cache>> {
        self.cache.clone()
    }
}

impl crate::terminal::RuntimeSource for NodeSource {
    fn current(&self) -> Option<(u64, smelt_runtime_client::RuntimeClient)> {
        self.hub
            .current()
            .map(|runtime| (runtime.generation, runtime.client))
    }

    fn subscribe(&self, listener: Listener) {
        self.hub.subscribe(listener);
    }
}

impl Node {
    fn new(
        runtime_id: String,
        target: Target,
        serial: u64,
        hub: Arc<ConnectionHub>,
        cache: Option<Arc<Cache>>,
        timings: &Timings,
    ) -> Self {
        let source = Arc::new(NodeSource {
            hub: Arc::clone(&hub),
            cache,
        });
        Self {
            catalog: Catalog::new(runtime_id.clone(), source.clone(), timings.catalog),
            conversations: Conversations::new(source.clone(), timings.conversation),
            terminals: TerminalHub::new(source, timings.terminal),
            runtime_id,
            target,
            serial,
            hub,
        }
    }

    /// 撤掉这台机器：会话目录不再推事件，对话订阅停下且不再写缓存，断开连接（终端随之以
    /// fatal 收尾）。
    fn shutdown(&self) {
        self.catalog.close();
        self.conversations.close();
        self.hub.disconnect();
    }
}

// ---------------------------------------------------------------------------
// 机队
// ---------------------------------------------------------------------------

/// Dart 的机队订阅者：返回 false 表示已取消。在 `view` 锁里调用，不能阻塞。
pub(crate) type FleetListener = Box<dyn Fn(&FleetView) -> bool + Send>;

/// Dart 的会话目录订阅者。要挂到每个节点的会话目录上，所以要能共享。
pub(crate) type CatalogListener = Box<dyn Fn(&CatalogEvent) -> bool + Send + Sync>;

struct CatalogSink {
    alive: AtomicBool,
    listener: CatalogListener,
}

impl CatalogSink {
    fn deliver(&self, event: &CatalogEvent) -> bool {
        if !self.alive.load(Ordering::Acquire) {
            return false;
        }
        let delivered = (self.listener)(event);
        if !delivered {
            self.alive.store(false, Ordering::Release);
        }
        delivered
    }
}

pub(crate) struct Fleet {
    shared: Arc<Shared>,
}

struct Shared {
    timings: Timings,
    env: OnceLock<Env>,
    /// 改登记的操作（初始化、配对、忘记、导入、改名、改色、挪动）串行：它们要读改登记并
    /// 可能换 endpoint 形态。配对要等网络，期间其它改登记的操作排队。
    ops: Mutex<()>,
    registry: Mutex<Registry>,
    view: Mutex<View>,
    pass: PassHub,
    next_serial: AtomicU64,
}

#[derive(Default)]
struct Registry {
    nodes: HashMap<String, Arc<Node>>,
    catalog_sinks: Vec<Arc<CatalogSink>>,
}

#[derive(Default)]
struct View {
    /// 按 `position` 排好。
    records: Vec<RuntimeRecord>,
    /// runtime_id → (节点序号, 状态)。
    statuses: HashMap<String, (u64, RuntimeStatus)>,
    listeners: Vec<FleetListener>,
    last: Option<FleetView>,
}

impl View {
    fn snapshot(&self) -> FleetView {
        FleetView {
            runtimes: self
                .records
                .iter()
                .map(|record| FleetRuntime {
                    record: record.clone(),
                    status: self
                        .statuses
                        .get(&record.runtime_id)
                        .map(|(_, status)| status.clone())
                        .unwrap_or_else(idle_status),
                })
                .collect(),
        }
    }

    /// 变了就推。
    fn publish(&mut self) {
        let view = self.snapshot();
        if self.last.as_ref() == Some(&view) {
            return;
        }
        self.listeners.retain(|listener| listener(&view));
        self.last = Some(view);
    }
}

fn idle_status() -> RuntimeStatus {
    RuntimeStatus {
        state: ConnectionState::Disconnected,
        link: None,
        error: None,
    }
}

fn status_of(event: &ConnectionEvent) -> RuntimeStatus {
    RuntimeStatus {
        state: event.state,
        link: event.link,
        error: event.error.clone(),
    }
}

static FLEET: LazyLock<Fleet> = LazyLock::new(|| Fleet::new(Timings::PRODUCTION));

/// 进程级的机队。订阅可以先于 `init_client`。
pub(crate) fn fleet() -> &'static Fleet {
    &FLEET
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(|e| e.into_inner())
}

fn not_initialized() -> SmeltError {
    error(
        SmeltErrorKind::Other,
        "还没有初始化设备身份（先调 init_client）",
    )
}

impl Fleet {
    pub(crate) fn new(timings: Timings) -> Self {
        let shared = Arc::new_cyclic(|weak: &Weak<Shared>| {
            let weak = weak.clone();
            Shared {
                timings,
                env: OnceLock::new(),
                ops: Mutex::new(()),
                registry: Mutex::new(Registry::default()),
                view: Mutex::new(View::default()),
                pass: PassHub::new(
                    Arc::new(move |runtime_id, session_id| {
                        let shared = weak.upgrade().ok_or_else(not_initialized)?;
                        shared
                            .node(runtime_id)?
                            .conversations
                            .pending_actions(session_id)
                    }),
                    timings.pass,
                ),
                next_serial: AtomicU64::new(0),
            }
        });
        Self { shared }
    }

    /// 读取或生成设备密钥（`<data_dir>/client-iroh-secret`），打开本机库和缓存，为每台登记
    /// 的机器建节点并在后台开始连。返回设备的 endpoint id。
    ///
    /// 同一目录重复调用幂等；换目录报错（身份不能在进程里悄悄换掉）。
    pub(crate) fn init_client(&self, data_dir: PathBuf) -> Result<String, SmeltError> {
        let _ops = lock(&self.shared.ops);
        if let Some(env) = self.shared.env.get() {
            if env.data_dir == data_dir {
                return Ok(env.device_id.clone());
            }
            return Err(error(
                SmeltErrorKind::Other,
                format!(
                    "设备身份已从 {} 加载，不能换成 {}",
                    env.data_dir.display(),
                    data_dir.display()
                ),
            ));
        }
        let secret = smelt_iroh::load_or_create_secret(&data_dir.join(SECRET_FILE))
            .map_err(|e| error(SmeltErrorKind::Other, format!("读取设备身份失败：{e:#}")))?;
        let device_id = secret.public().to_string();
        log::info!("设备身份：{device_id}");
        let store = Store::open(&data_dir.join(crate::store::DATABASE_FILE))?;
        // 缓存打不开不影响连接，只是离线时没有旧数据可看。
        let cache = Cache::open(&data_dir.join(crate::cache::DATABASE_FILE))
            .map(Arc::new)
            .map_err(|e| log::warn!("离线缓存打不开：{e}"))
            .ok();
        self.shared.start(Env {
            data_dir,
            device_id: device_id.clone(),
            store: Arc::new(store),
            cache,
            transport: Arc::new(crate::iroh_dialer::IrohDialer::new(secret)),
        })?;
        Ok(device_id)
    }

    /// 用给定的环境启动（测试注入假传输用）。
    #[cfg(test)]
    pub(crate) fn start(&self, env: Env) -> Result<(), SmeltError> {
        let _ops = lock(&self.shared.ops);
        self.shared.start(env)
    }

    pub(crate) fn node(&self, runtime_id: &str) -> Result<Arc<Node>, SmeltError> {
        self.shared.node(runtime_id)
    }

    pub(crate) fn store(&self) -> Result<Arc<Store>, SmeltError> {
        Ok(Arc::clone(&self.shared.env()?.store))
    }

    /// 订阅整份机队：先推一次当前的。
    pub(crate) fn subscribe(&self, listener: FleetListener) {
        let mut view = lock(&self.shared.view);
        let snapshot = view.snapshot();
        if listener(&snapshot) {
            view.listeners.push(listener);
        }
    }

    /// 订阅所有机器的会话目录事件：现有的每台先推一次当前列表，之后加入的机器同样挂上。
    pub(crate) fn subscribe_catalog(&self, listener: CatalogListener) {
        let sink = Arc::new(CatalogSink {
            alive: AtomicBool::new(true),
            listener,
        });
        let mut registry = lock(&self.shared.registry);
        registry
            .catalog_sinks
            .retain(|sink| sink.alive.load(Ordering::Acquire));
        registry.catalog_sinks.push(Arc::clone(&sink));
        for node in registry.nodes.values() {
            attach_catalog_sink(node, &sink);
        }
    }

    pub(crate) fn subscribe_pass(&self, listener: PassListener) {
        self.shared.pass.subscribe(listener);
    }

    /// 配对。见 `crate::api::identity::pair`。
    pub(crate) fn pair(
        &self,
        target: Target,
        code: String,
        device_name: String,
    ) -> Result<RuntimeRecord, SmeltError> {
        let shared = &self.shared;
        let env = shared.env()?;
        let _ops = lock(&shared.ops);
        let records = env.store.runtimes()?;
        let before: Vec<Option<String>> = records.iter().map(|r| r.relay_url.clone()).collect();
        // 配对期间按「这台也登记了」的形态拨号；同一 endpoint 已登记就换掉它原来的 relay。
        let mut during: Vec<Option<String>> = records
            .iter()
            .filter(|record| record.endpoint_id != target.endpoint_id)
            .map(|record| record.relay_url.clone())
            .collect();
        during.push(target.relay_url.clone());
        shared.reshape(env, &during)?;

        // 在单独的状态机上配对：配对码打错、过期都不影响这台机器已有的连接。
        let hub = Arc::new(ConnectionHub::new(shared.timings.connection));
        let paired = hub
            .pair(env.transport.as_ref(), target.clone(), code, device_name)
            .and_then(|info| {
                let paired = env.store.record_pairing(&info, crate::store::now_ms());
                if paired.is_err() {
                    hub.disconnect();
                }
                paired
            });
        let paired = match paired {
            Ok(paired) => paired,
            Err(failure) => {
                if let Err(reshape_error) = shared.reshape(env, &before) {
                    log::warn!(
                        "配对失败后恢复 endpoint 形态失败：{}",
                        reshape_error.message
                    );
                }
                return Err(failure);
            }
        };
        let record = paired.record;
        if let Some(replaced) = paired.replaced {
            log::info!(
                "endpoint {} 现在是 {}，撤掉旧登记 {replaced}",
                target.endpoint_id,
                record.runtime_id
            );
            shared.remove_node(&replaced);
            // 缓存按 endpoint 存：不删的话新登记会把重装前的会话列表和对话当成自己的。
            // 旧节点已停（不再写缓存），新节点还没建。
            if let Some(cache) = &env.cache {
                cache.delete_runtime(&target.endpoint_id);
            }
        }
        let record_target = Target::new(record.endpoint_id.clone(), record.relay_url.clone());
        let existing = lock(&shared.registry)
            .nodes
            .get(&record.runtime_id)
            .cloned();
        match existing {
            // 同一台机器、地址没变（被撤销后重新配对）：保留它的节点（订阅都还在），丢掉配对
            // 用的状态机，让节点自己重新连。
            Some(node) if node.target == record_target => {
                hub.disconnect();
                shared.spawn_connect(&node);
            }
            // 地址变了（或新机器）：用配对时已连上的状态机建节点。
            existing => {
                if existing.is_some() {
                    shared.remove_node(&record.runtime_id);
                }
                shared.add_node(env, record.runtime_id.clone(), record_target, hub);
            }
        }
        shared.reload_records(env)?;
        // 登记以库里为准再算一次形态（被替换的旧登记也算在内）。
        let after = shared.relays();
        shared.reshape(env, &after)?;
        Ok(record)
    }

    pub(crate) fn rename(&self, runtime_id: &str, name: &str) -> Result<(), SmeltError> {
        let env = self.shared.env()?;
        let _ops = lock(&self.shared.ops);
        env.store.rename(runtime_id, name)?;
        self.shared.reload_records(env)
    }

    pub(crate) fn set_accent(&self, runtime_id: &str, accent: u32) -> Result<(), SmeltError> {
        let env = self.shared.env()?;
        let _ops = lock(&self.shared.ops);
        env.store.set_accent(runtime_id, accent)?;
        self.shared.reload_records(env)
    }

    pub(crate) fn move_runtime(&self, runtime_id: &str, to_index: u32) -> Result<(), SmeltError> {
        let env = self.shared.env()?;
        let _ops = lock(&self.shared.ops);
        env.store.move_runtime(runtime_id, to_index)?;
        self.shared.reload_records(env)
    }

    pub(crate) fn forget(&self, runtime_id: &str) -> Result<(), SmeltError> {
        let shared = &self.shared;
        let env = shared.env()?;
        let _ops = lock(&shared.ops);
        let endpoint = lock(&shared.view)
            .records
            .iter()
            .find(|record| record.runtime_id == runtime_id)
            .map(|record| record.endpoint_id.clone());
        if !env.store.delete_runtime(runtime_id)? {
            return Err(crate::store::unknown_runtime(runtime_id));
        }
        shared.remove_node(runtime_id);
        // 节点已停（对话不再写缓存）之后再删缓存。
        if let (Some(cache), Some(endpoint)) = (&env.cache, endpoint) {
            cache.delete_runtime(&endpoint);
        }
        shared.reload_records(env)?;
        let relays = shared.relays();
        shared.reshape(env, &relays)?;
        Ok(())
    }

    /// 立刻重新连一台机器，阻塞到这次握手有结果。
    pub(crate) fn reconnect(&self, runtime_id: &str) -> Result<(), SmeltError> {
        let env = self.shared.env()?;
        let node = self.shared.node(runtime_id)?;
        self.shared.connect(env, &node)
    }

    pub(crate) fn verify(&self) {
        for node in self.shared.nodes() {
            node.hub.verify();
        }
    }

    pub(crate) fn detach_all_terminals(&self) {
        for node in self.shared.nodes() {
            node.terminals.detach_all();
        }
    }

    pub(crate) fn import(
        &self,
        runtimes: Vec<RuntimeImport>,
    ) -> Result<Vec<RuntimeRecord>, SmeltError> {
        let shared = &self.shared;
        let env = shared.env()?;
        let _ops = lock(&shared.ops);
        let mut imported = Vec::new();
        for entry in runtimes {
            let runtime_id = entry.runtime_id.trim();
            let target = Target::new(entry.endpoint_id, entry.relay_url);
            if runtime_id.is_empty() {
                continue;
            }
            if let Err(invalid) =
                crate::iroh_dialer::validate(&target.endpoint_id, target.relay_url.as_deref())
            {
                log::warn!("跳过不合法的旧登记 {runtime_id}：{}", invalid.message);
                continue;
            }
            if let Some(record) = env.store.import_runtime(
                runtime_id,
                &target.endpoint_id,
                target.relay_url.as_deref(),
                &entry.name,
                crate::store::now_ms(),
            )? {
                imported.push(record);
            }
        }
        if imported.is_empty() {
            return Ok(imported);
        }
        let mut nodes = Vec::new();
        for record in &imported {
            let target = Target::new(record.endpoint_id.clone(), record.relay_url.clone());
            let hub = Arc::new(ConnectionHub::new(shared.timings.connection));
            nodes.push(shared.add_node(env, record.runtime_id.clone(), target, hub));
        }
        shared.reload_records(env)?;
        let relays = shared.relays();
        // 形态变了会让所有节点（含新的）重新连；没变就只连新的。
        if !shared.reshape(env, &relays).unwrap_or_else(|e| {
            log::warn!("导入后调整 endpoint 形态失败：{}", e.message);
            false
        }) {
            for node in &nodes {
                shared.spawn_connect(node);
            }
        }
        Ok(imported)
    }
}

impl Shared {
    fn env(&self) -> Result<&Env, SmeltError> {
        self.env.get().ok_or_else(not_initialized)
    }

    fn node(&self, runtime_id: &str) -> Result<Arc<Node>, SmeltError> {
        self.env()?;
        lock(&self.registry)
            .nodes
            .get(runtime_id)
            .cloned()
            .ok_or_else(|| crate::store::unknown_runtime(runtime_id))
    }

    fn nodes(&self) -> Vec<Arc<Node>> {
        lock(&self.registry).nodes.values().cloned().collect()
    }

    fn relays(&self) -> Vec<Option<String>> {
        lock(&self.view)
            .records
            .iter()
            .map(|record| record.relay_url.clone())
            .collect()
    }

    /// 装上环境、为每台登记的机器建节点并在后台开始连。调用方持有 `ops`。
    fn start(self: &Arc<Self>, env: Env) -> Result<(), SmeltError> {
        let records = env.store.runtimes()?;
        let relays: Vec<Option<String>> = records.iter().map(|r| r.relay_url.clone()).collect();
        env.transport.configure(&relays)?;
        if self.env.set(env).is_err() {
            return Err(error(SmeltErrorKind::Other, "设备身份已经初始化过"));
        }
        let env = self.env()?;
        let nodes: Vec<Arc<Node>> = records
            .iter()
            .map(|record| {
                let target = Target::new(record.endpoint_id.clone(), record.relay_url.clone());
                let hub = Arc::new(ConnectionHub::new(self.timings.connection));
                self.add_node(env, record.runtime_id.clone(), target, hub)
            })
            .collect();
        self.reload_records(env)?;
        for node in &nodes {
            self.spawn_connect(node);
        }
        Ok(())
    }

    /// 建节点并登记：状态挂到机队视图，会话目录挂上所有订阅者和指挥台。
    fn add_node(
        self: &Arc<Self>,
        env: &Env,
        runtime_id: String,
        target: Target,
        hub: Arc<ConnectionHub>,
    ) -> Arc<Node> {
        let serial = self.next_serial.fetch_add(1, Ordering::Relaxed);
        let node = Arc::new(Node::new(
            runtime_id.clone(),
            target,
            serial,
            hub,
            env.cache.clone(),
            &self.timings,
        ));
        lock(&self.view)
            .statuses
            .insert(runtime_id.clone(), (serial, idle_status()));
        let weak = Arc::downgrade(self);
        let status_id = runtime_id.clone();
        node.hub.subscribe(Box::new(move |_generation, event| {
            let Some(shared) = weak.upgrade() else {
                return false;
            };
            let mut view = lock(&shared.view);
            match view.statuses.get_mut(&status_id) {
                Some((current, status)) if *current == serial => {
                    *status = status_of(event);
                    view.publish();
                    true
                }
                // 节点已撤掉或被替换。
                _ => false,
            }
        }));

        let mut registry = lock(&self.registry);
        let pass_weak = Arc::downgrade(self);
        let pass_id = runtime_id.clone();
        node.catalog.subscribe(Box::new(move |event| {
            let Some(shared) = pass_weak.upgrade() else {
                return false;
            };
            if let Some(list) = &event.sessions {
                shared.pass.update(&pass_id, list.clone());
            }
            true
        }));
        registry
            .catalog_sinks
            .retain(|sink| sink.alive.load(Ordering::Acquire));
        for sink in &registry.catalog_sinks {
            attach_catalog_sink(&node, sink);
        }
        registry.nodes.insert(runtime_id, Arc::clone(&node));
        node
    }

    /// 撤掉一台机器的节点：从视图和指挥台拿掉，停掉它，告诉会话目录的订阅者它的列表空了。
    fn remove_node(&self, runtime_id: &str) {
        let removed = lock(&self.registry).nodes.remove(runtime_id);
        let Some(node) = removed else {
            return;
        };
        {
            let mut view = lock(&self.view);
            if view
                .statuses
                .get(runtime_id)
                .is_some_and(|(serial, _)| *serial == node.serial)
            {
                view.statuses.remove(runtime_id);
            }
        }
        node.shutdown();
        let event = CatalogEvent {
            runtime_id: runtime_id.to_string(),
            kind: CatalogEventKind::Sessions,
            sessions: Some(SessionList {
                sessions: Vec::new(),
                cached: true,
                saved_at_ms: None,
            }),
            attention: None,
            session_id: None,
            message: None,
        };
        let sinks = lock(&self.registry).catalog_sinks.clone();
        for sink in sinks {
            sink.deliver(&event);
        }
    }

    /// 从库里重读登记，推机队视图，更新指挥台的机器顺序。
    fn reload_records(&self, env: &Env) -> Result<(), SmeltError> {
        let records = env.store.runtimes()?;
        let order: Vec<String> = records.iter().map(|r| r.runtime_id.clone()).collect();
        {
            let mut view = lock(&self.view);
            view.records = records;
            view.publish();
        }
        self.pass.set_order(order);
        Ok(())
    }

    /// 按 `relays` 调整 endpoint 形态；变了就让所有节点在后台重新连。返回是否变了。
    fn reshape(self: &Arc<Self>, env: &Env, relays: &[Option<String>]) -> Result<bool, SmeltError> {
        let changed = env.transport.configure(relays)?;
        if changed {
            for node in self.nodes() {
                self.spawn_connect(&node);
            }
        }
        Ok(changed)
    }

    /// 连一台机器，阻塞。连上后用描述刷新登记里的主机名、系统和版本。
    fn connect(&self, env: &Env, node: &Node) -> Result<(), SmeltError> {
        let info = node
            .hub
            .connect(env.transport.as_ref(), node.target.clone())?;
        self.record_describe(env, node, &info);
        Ok(())
    }

    fn record_describe(&self, env: &Env, node: &Node, info: &RuntimeInfo) {
        if info.runtime_id != node.runtime_id {
            // endpoint 换了主人（那台机器重装后换了身份）：要重新配对才认。
            log::warn!(
                "endpoint {} 现在报告的是 {}，不是登记的 {}",
                node.target.endpoint_id,
                info.runtime_id,
                node.runtime_id
            );
            return;
        }
        match env.store.record_describe(info) {
            Ok(Some(record)) => {
                let mut view = lock(&self.view);
                if let Some(slot) = view
                    .records
                    .iter_mut()
                    .find(|slot| slot.runtime_id == record.runtime_id)
                {
                    *slot = record;
                    view.publish();
                }
            }
            Ok(None) => {}
            Err(failure) => log::warn!("刷新机器描述失败：{}", failure.message),
        }
    }

    /// 在后台线程上连。离线的机器进入 `Reconnecting` 自己退避重试；结果从机队视图推出。
    fn spawn_connect(self: &Arc<Self>, node: &Arc<Node>) {
        let shared = Arc::clone(self);
        let node = Arc::clone(node);
        let spawned = std::thread::Builder::new()
            .name("smelt-fleet-connect".to_string())
            .spawn(move || {
                let Ok(env) = shared.env() else {
                    return;
                };
                if let Err(failure) = shared.connect(env, &node) {
                    log::info!("连 {} 失败：{}", node.runtime_id, failure.message);
                }
            });
        if let Err(spawn_error) = spawned {
            log::error!("起不了连接线程：{spawn_error}");
        }
    }
}

fn attach_catalog_sink(node: &Node, sink: &Arc<CatalogSink>) {
    let sink = Arc::clone(sink);
    node.catalog
        .subscribe(Box::new(move |event| sink.deliver(event)));
}

#[cfg(test)]
mod tests;
