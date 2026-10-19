//! 远程访问：iroh 运行时 endpoint，以及 macOS 空闲防睡眠断言（设计文档 D2、D3）。
//!
//! 开关归运行时自己管：`remote.config.set` 存下配置并照着开关，守护每次启动按落盘
//! 配置自愈。不参与无缝升级交接：exec 前停掉，新进程按配置重新拉起；endpoint id 由
//! 落盘私钥决定，重启不变，已配对设备照常能连。

use super::*;
use smelt_core::remote_config::{RemoteAccessState, RemoteConfig};
use smelt_iroh::runtime::ConnectionRegistry;

/// 远程访问开着时持有的空闲防睡眠断言名称。合盖仍走 Clamshell Sleep。
#[cfg(target_os = "macos")]
pub(crate) const SLEEP_ASSERTION_NAME: &str = "Smelt remote access is enabled";
#[cfg(target_os = "macos")]
pub(crate) const SLEEP_ASSERTION_TYPE: &str = "PreventUserIdleSystemSleep";

/// 当前供电方式。只把明确的电池供电当 Battery；桌面、UPS、读失败都当插电，
/// 避免误判导致插电时手机连不上。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
// 只有 macOS 读得到电源状态；别的平台一律当插电。
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
pub(crate) enum PowerKind {
    Ac,
    Battery,
}

/// 插电：远程开着就握断言，手机随时能连（旧行为）。
/// 电池：绝不阻止任何休眠。远程只能在电脑本身醒着、正在用的时候连；
/// 一旦睡着，远程设备就连不上。
pub(crate) fn sleep_assertion_desired(power: PowerKind) -> bool {
    matches!(power, PowerKind::Ac)
}

#[cfg(target_os = "macos")]
pub(crate) struct SystemSleepAssertion {
    pub(crate) id: u32,
}

#[cfg(target_os = "macos")]
impl SystemSleepAssertion {
    pub(crate) fn acquire() -> Result<Self, i32> {
        use core_foundation::base::TCFType as _;
        use core_foundation::string::{CFString, CFStringRef};

        #[link(name = "IOKit", kind = "framework")]
        unsafe extern "C" {
            fn IOPMAssertionCreateWithName(
                assertion_type: CFStringRef,
                assertion_level: u32,
                assertion_name: CFStringRef,
                assertion_id: *mut u32,
            ) -> i32;
        }

        let assertion_type = CFString::new(SLEEP_ASSERTION_TYPE);
        let reason = CFString::new(SLEEP_ASSERTION_NAME);
        let mut id = 0;
        let result = unsafe {
            IOPMAssertionCreateWithName(
                assertion_type.as_concrete_TypeRef(),
                255,
                reason.as_concrete_TypeRef(),
                &mut id,
            )
        };
        if result == 0 {
            Ok(Self { id })
        } else {
            Err(result)
        }
    }

    pub(crate) fn release_id(id: u32) -> i32 {
        #[link(name = "IOKit", kind = "framework")]
        unsafe extern "C" {
            fn IOPMAssertionRelease(assertion_id: u32) -> i32;
        }
        unsafe { IOPMAssertionRelease(id) }
    }

    /// 清掉本 PID 上同名残留。`exec()` 不跑 Drop，升级后这些断言会跟着 PID 活下来。
    pub(crate) fn release_orphans() {
        let ids = Self::owned_ids();
        if ids.is_empty() {
            return;
        }
        let mut released = 0;
        for id in ids {
            if Self::release_id(id) == 0 {
                released += 1;
            }
        }
        if released > 0 {
            dlog(&format!("已清理 {released} 条残留远程电源断言"));
        }
    }

    pub(crate) fn owned_ids() -> Vec<u32> {
        use core_foundation::array::{CFArrayGetCount, CFArrayGetValueAtIndex, CFArrayRef};
        use core_foundation::base::TCFType as _;
        use core_foundation::dictionary::{CFDictionary, CFDictionaryGetValue, CFDictionaryRef};
        use core_foundation::number::{CFNumber, CFNumberRef};
        use core_foundation::string::{CFString, CFStringRef};
        use std::ffi::c_void;

        #[link(name = "IOKit", kind = "framework")]
        unsafe extern "C" {
            fn IOPMCopyAssertionsByProcess(assertions: *mut CFDictionaryRef) -> i32;
        }

        let mut raw: CFDictionaryRef = std::ptr::null();
        let copy_ok = unsafe { IOPMCopyAssertionsByProcess(&mut raw) == 0 && !raw.is_null() };
        if !copy_ok {
            return Vec::new();
        }
        let dict: CFDictionary = unsafe { CFDictionary::wrap_under_create_rule(raw) };
        let pid = i64::from(std::process::id() as i32);
        let name_key = CFString::new("AssertName");
        let id_key = CFString::new("AssertionId");
        let mut ids = Vec::new();
        let (keys, values) = dict.get_keys_and_values();
        for (key, value) in keys.into_iter().zip(values) {
            if key.is_null() || value.is_null() {
                continue;
            }
            let key_pid = unsafe { CFNumber::wrap_under_get_rule(key as CFNumberRef) }
                .to_i64()
                .unwrap_or(-1);
            if key_pid != pid {
                continue;
            }
            let array = value as CFArrayRef;
            let count = unsafe { CFArrayGetCount(array) };
            for index in 0..count {
                let item = unsafe { CFArrayGetValueAtIndex(array, index) } as CFDictionaryRef;
                if item.is_null() {
                    continue;
                }
                let name_ptr = unsafe {
                    CFDictionaryGetValue(item, name_key.as_concrete_TypeRef() as *const c_void)
                };
                if name_ptr.is_null() {
                    continue;
                }
                let name =
                    unsafe { CFString::wrap_under_get_rule(name_ptr as CFStringRef) }.to_string();
                if name != SLEEP_ASSERTION_NAME {
                    continue;
                }
                let id_ptr = unsafe {
                    CFDictionaryGetValue(item, id_key.as_concrete_TypeRef() as *const c_void)
                };
                if id_ptr.is_null() {
                    continue;
                }
                if let Some(id) =
                    unsafe { CFNumber::wrap_under_get_rule(id_ptr as CFNumberRef) }.to_i64()
                {
                    ids.push(id as u32);
                }
            }
        }
        ids
    }

    #[cfg(test)]
    pub(crate) fn count_owned() -> usize {
        Self::owned_ids().len()
    }
}

#[cfg(target_os = "macos")]
impl Drop for SystemSleepAssertion {
    fn drop(&mut self) {
        let result = Self::release_id(self.id);
        if result != 0 {
            dlog(&format!("释放远程电源断言失败：IOKit {result}"));
        }
    }
}

#[cfg(not(target_os = "macos"))]
pub(crate) struct SystemSleepAssertion;

#[cfg(not(target_os = "macos"))]
impl SystemSleepAssertion {
    pub(crate) fn acquire() -> Result<Self, i32> {
        Ok(Self)
    }
}

#[cfg(test)]
static POWER_KIND_OVERRIDE: Mutex<Option<PowerKind>> = Mutex::new(None);

#[cfg(test)]
pub(crate) fn set_power_kind_override(kind: Option<PowerKind>) -> Option<PowerKind> {
    let mut guard = POWER_KIND_OVERRIDE.lock().unwrap();
    let previous = *guard;
    *guard = kind;
    previous
}

pub(crate) fn current_power_kind() -> PowerKind {
    #[cfg(test)]
    {
        if let Some(kind) = *POWER_KIND_OVERRIDE.lock().unwrap() {
            return kind;
        }
    }
    read_power_kind()
}

#[cfg(target_os = "macos")]
fn read_power_kind() -> PowerKind {
    use core_foundation::base::{CFType, TCFType as _};
    use core_foundation::string::{CFString, CFStringRef};

    #[link(name = "IOKit", kind = "framework")]
    unsafe extern "C" {
        fn IOPSCopyPowerSourcesInfo() -> core_foundation::base::CFTypeRef;
        fn IOPSGetProvidingPowerSourceType(
            snapshot: core_foundation::base::CFTypeRef,
        ) -> CFStringRef;
    }

    unsafe {
        let snapshot = IOPSCopyPowerSourcesInfo();
        if snapshot.is_null() {
            return PowerKind::Ac;
        }
        let _owned = CFType::wrap_under_create_rule(snapshot);
        let kind = IOPSGetProvidingPowerSourceType(snapshot);
        if kind.is_null() {
            return PowerKind::Ac;
        }
        let name = CFString::wrap_under_get_rule(kind).to_string();
        if name.eq_ignore_ascii_case("Battery Power") {
            PowerKind::Battery
        } else {
            PowerKind::Ac
        }
    }
}

#[cfg(not(target_os = "macos"))]
fn read_power_kind() -> PowerKind {
    PowerKind::Ac
}

/// 按供电方式拿起或放下空闲防睡眠断言。电池上即使有设备连着也不握。
pub(crate) fn sync_remote_sleep_assertion(state: &RemoteState) {
    let mut guard = state.lock().unwrap();
    if guard.stopping {
        return;
    }
    let desired = sleep_assertion_desired(current_power_kind());
    let Some(endpoint) = guard.endpoint.as_mut() else {
        return;
    };
    let holding = endpoint.sleep_assertion.is_some();
    if desired == holding {
        return;
    }
    if desired {
        match SystemSleepAssertion::acquire() {
            Ok(assertion) => {
                endpoint.sleep_assertion = Some(assertion);
                dlog("插电，已持有远程电源断言");
            }
            Err(code) => {
                dlog(&format!("远程访问无法阻止系统空闲睡眠：IOKit {code}"));
            }
        }
    } else {
        endpoint.sleep_assertion = None;
        dlog("电池供电，已释放远程电源断言，不阻止任何休眠");
    }
}

fn run_power_source_watch(state: RemoteState, generation: u64) {
    let mut last = None;
    loop {
        thread::sleep(Duration::from_secs(2));
        {
            let guard = state.lock().unwrap();
            if guard.stopping || guard.generation != generation || guard.endpoint.is_none() {
                return;
            }
        }
        let now = current_power_kind();
        if last == Some(now) {
            continue;
        }
        last = Some(now);
        sync_remote_sleep_assertion(&state);
    }
}

/// 在跑的 endpoint。
pub(crate) struct RemoteEndpoint {
    pub(crate) endpoint_id: String,
    pub(crate) relay: Option<smelt_iroh::RelaySettings>,
    pub(crate) registry: ConnectionRegistry,
    shutdown_tx: Option<tokio::sync::oneshot::Sender<()>>,
    pub(crate) sleep_assertion: Option<SystemSleepAssertion>,
}

impl RemoteEndpoint {
    fn shutdown(mut self) {
        if let Some(tx) = self.shutdown_tx.take() {
            let _ = tx.send(());
        }
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
enum Phase {
    #[default]
    Idle,
    Starting,
    Failed(String),
}

#[derive(Default)]
pub(crate) struct RemoteStateData {
    pub(crate) endpoint: Option<RemoteEndpoint>,
    phase: Phase,
    /// 每次配置变化都推进。后台绑定线程发现代际变了，就不再发布结果、不再重试。
    generation: u64,
    /// 升级 exec 前由 `cleanup_sidecar_services` 置位，之后不再拉起任何东西。
    pub(crate) stopping: bool,
}

pub(crate) type RemoteState = Arc<Mutex<RemoteStateData>>;

pub(crate) fn new_remote_state() -> RemoteState {
    Arc::new(Mutex::new(RemoteStateData::default()))
}

pub(crate) fn remote_status(state: &RemoteState) -> RemoteAccessState {
    let guard = state.lock().unwrap();
    if let Some(endpoint) = guard.endpoint.as_ref() {
        return RemoteAccessState::Running {
            endpoint_id: endpoint.endpoint_id.clone(),
            relay: endpoint.relay.as_ref().map(|relay| relay.url.to_string()),
        };
    }
    match &guard.phase {
        Phase::Idle => RemoteAccessState::Off,
        Phase::Starting => RemoteAccessState::Starting,
        Phase::Failed(error) => RemoteAccessState::Failed {
            error: error.clone(),
        },
    }
}

/// 照配置开关 endpoint，立刻返回。绑定在后台线程，失败就退避重试，直到成功、配置
/// 再变或守护停止。
pub(crate) fn apply_remote_config(state: &RemoteState, config: &RemoteConfig) {
    let relay = match config.iroh_relay.trim() {
        "" => Ok(None),
        address => smelt_iroh::RelaySettings::parse(address)
            .map(Some)
            .map_err(|error| format!("iroh relay 配置无效：{error:#}")),
    };
    let mut guard = state.lock().unwrap();
    if guard.stopping {
        return;
    }
    guard.generation = guard.generation.wrapping_add(1);
    let generation = guard.generation;
    let relay = match relay {
        Ok(relay) if config.enabled => relay,
        Ok(_) => {
            if let Some(endpoint) = guard.endpoint.take() {
                endpoint.shutdown();
            }
            guard.phase = Phase::Idle;
            drop(guard);
            #[cfg(target_os = "macos")]
            SystemSleepAssertion::release_orphans();
            dlog("远程访问已关闭");
            return;
        }
        Err(error) => {
            if let Some(endpoint) = guard.endpoint.take() {
                endpoint.shutdown();
            }
            guard.phase = Phase::Failed(error);
            return;
        }
    };
    if guard
        .endpoint
        .as_ref()
        .is_some_and(|endpoint| endpoint.relay == relay)
    {
        return;
    }
    if let Some(endpoint) = guard.endpoint.take() {
        endpoint.shutdown();
    }
    guard.phase = Phase::Starting;
    drop(guard);
    let state = Arc::clone(state);
    thread::spawn(move || bind_with_backoff(state, relay, generation));
}

fn generation_is_current(state: &RemoteState, generation: u64) -> bool {
    let guard = state.lock().unwrap();
    !guard.stopping && guard.generation == generation
}

/// 绑定失败几乎都是「网络还没好」。无头机器没人会来点重试，所以一直退避重试下去。
fn bind_with_backoff(
    state: RemoteState,
    relay: Option<smelt_iroh::RelaySettings>,
    generation: u64,
) {
    const BACKOFF_SECS: [u64; 5] = [0, 3, 10, 30, 60];
    for attempt in 0usize.. {
        let delay = BACKOFF_SECS[attempt.min(BACKOFF_SECS.len() - 1)];
        if delay > 0 {
            thread::sleep(Duration::from_secs(delay));
        }
        if !generation_is_current(&state, generation) {
            return;
        }
        match bind_endpoint(relay.clone()) {
            Ok(endpoint) => {
                let mut guard = state.lock().unwrap();
                if guard.stopping || guard.generation != generation {
                    drop(guard);
                    endpoint.shutdown();
                    return;
                }
                dlog(&format!("远程访问已开启：{}", endpoint.endpoint_id));
                guard.endpoint = Some(endpoint);
                guard.phase = Phase::Idle;
                drop(guard);
                sync_remote_sleep_assertion(&state);
                if !cfg!(test) {
                    let watch_state = Arc::clone(&state);
                    thread::spawn(move || run_power_source_watch(watch_state, generation));
                }
                return;
            }
            Err(error) => {
                dlog(&format!(
                    "远程访问启动失败（第 {} 次）：{error}",
                    attempt + 1
                ));
                let mut guard = state.lock().unwrap();
                if guard.generation == generation {
                    guard.phase = Phase::Failed(error);
                }
            }
        }
    }
}

/// 起一个 iroh endpoint，每条进来的流交给 [`crate::devices::accept_remote_stream`]。
fn bind_endpoint(relay: Option<smelt_iroh::RelaySettings>) -> Result<RemoteEndpoint, String> {
    let (shutdown_tx, shutdown_rx) = tokio::sync::oneshot::channel::<()>();
    let (ready_tx, ready_rx) = std::sync::mpsc::channel::<Result<String, String>>();
    let registry = ConnectionRegistry::default();
    let serve_registry = registry.clone();
    let bind_relay = relay.clone();
    thread::spawn(move || {
        let runtime = match tokio::runtime::Runtime::new() {
            Ok(runtime) => runtime,
            Err(error) => {
                let _ = ready_tx.send(Err(format!("起不了 tokio runtime：{error}")));
                return;
            }
        };
        runtime.block_on(async move {
            let secret = match smelt_iroh::default_secret_path()
                .and_then(|path| smelt_iroh::load_or_create_secret(&path))
            {
                Ok(secret) => secret,
                Err(error) => {
                    let _ = ready_tx.send(Err(format!("iroh 密钥不可用：{error:#}")));
                    return;
                }
            };
            let endpoint = match smelt_iroh::runtime::bind_runtime_endpoint(
                secret,
                vec![smelt_iroh::runtime::RUNTIME_ALPN.to_vec()],
                bind_relay.as_ref(),
            )
            .await
            {
                Ok(endpoint) => endpoint,
                Err(error) => {
                    let _ = ready_tx.send(Err(format!("{error:#}")));
                    return;
                }
            };
            let _ = ready_tx.send(Ok(endpoint.id().to_string()));
            let path_observer: smelt_iroh::PathObserver = Arc::new(|status| {
                dlog(&format!(
                    "iroh path remote={} kind={} address={} rtt_ms={}",
                    status.remote,
                    status.kind,
                    status.address,
                    status.rtt.as_millis()
                ));
            });
            let conn_observer: smelt_iroh::ConnectionObserver = Arc::new(|event| match event {
                smelt_iroh::ConnectionEvent::Connected { remote_id, .. } => {
                    dlog(&format!("iroh 设备已连接：{remote_id}"))
                }
                smelt_iroh::ConnectionEvent::Disconnected { remote_id, .. } => {
                    dlog(&format!("iroh 设备已断开：{remote_id}"))
                }
            });
            smelt_iroh::runtime::serve_runtime(
                endpoint,
                Arc::new(crate::devices::RemoteStreams),
                serve_registry,
                async move {
                    let _ = shutdown_rx.await;
                },
                Some(path_observer),
                Some(conn_observer),
            )
            .await;
        });
    });
    // 绑定要连 relay，弱网下比本地绑端口慢得多。
    match ready_rx.recv_timeout(Duration::from_secs(30)) {
        Ok(Ok(endpoint_id)) => Ok(RemoteEndpoint {
            endpoint_id,
            relay,
            registry,
            shutdown_tx: Some(shutdown_tx),
            sleep_assertion: None,
        }),
        Ok(Err(error)) => Err(error),
        Err(_) => {
            let _ = shutdown_tx.send(());
            Err("iroh endpoint 绑定超时（30s）".to_string())
        }
    }
}

/// 守护启动时按落盘配置拉起远程访问，不等任何客户端。
pub(crate) fn autostart_remote_from_config(state: &RemoteState) {
    // 跑测试 / 排障时不希望守护自作主张连网。
    if std::env::var_os("SMELT_NO_REMOTE_AUTOSTART").is_some() {
        return;
    }
    apply_remote_config(state, &smelt_core::remote_config::load());
}

/// 进程退出、upgrade exec 前停掉远程访问；控制方法和设备流也从这里找到远程状态。
pub(crate) static LIFECYCLE: std::sync::OnceLock<RemoteState> = std::sync::OnceLock::new();

pub(crate) fn register_lifecycle(remote: RemoteState) {
    let _ = LIFECYCLE.set(remote);
}

/// 本进程的远程状态。测试里没有注册时当远程关着。
pub(crate) fn lifecycle_remote() -> Option<&'static RemoteState> {
    LIFECYCLE.get()
}

/// exit / exec 前必须调：exec 不跑 Drop，之后再拿的断言会泄漏进下一份映像。
pub(crate) fn cleanup_sidecar_services() {
    if let Some(remote) = LIFECYCLE.get() {
        let endpoint = {
            let mut guard = remote.lock().unwrap();
            guard.stopping = true;
            guard.endpoint.take()
        };
        if let Some(endpoint) = endpoint {
            endpoint.shutdown();
        }
    }
    #[cfg(target_os = "macos")]
    SystemSleepAssertion::release_orphans();
}

#[cfg(test)]
pub(crate) struct RemoteTestLock {
    _lock: std::sync::MutexGuard<'static, ()>,
    previous_power: Option<PowerKind>,
}

#[cfg(test)]
impl Drop for RemoteTestLock {
    fn drop(&mut self) {
        set_power_kind_override(self.previous_power);
    }
}

/// 电源断言是进程全局的，相关测试串行跑。默认按插电算。
#[cfg(test)]
pub(crate) fn lock_remote_tests() -> RemoteTestLock {
    static LOCK: Mutex<()> = Mutex::new(());
    let _lock = LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let previous_power = set_power_kind_override(Some(PowerKind::Ac));
    RemoteTestLock {
        _lock,
        previous_power,
    }
}

/// 测试用：不联网，直接装上一个「在跑」的 endpoint。
#[cfg(test)]
pub(crate) fn install_test_endpoint(state: &RemoteState, endpoint_id: &str) {
    let mut guard = state.lock().unwrap();
    guard.generation = guard.generation.wrapping_add(1);
    guard.endpoint = Some(RemoteEndpoint {
        endpoint_id: endpoint_id.to_string(),
        relay: None,
        registry: ConnectionRegistry::default(),
        shutdown_tx: None,
        sleep_assertion: None,
    });
    drop(guard);
    sync_remote_sleep_assertion(state);
}
