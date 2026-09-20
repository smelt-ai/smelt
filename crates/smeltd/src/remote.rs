//! 内嵌远程网关、iroh 隧道、macOS 空闲防睡眠断言。
//!
//! 从 `main.rs` 整块搬出；不参与无缝升级交接，upgrade 后由 autostart 按落盘配置拉起。

use super::*;

/// 内嵌远程网关开着时的状态：token、绑定地址、写权限、喊停用的信号。见文件头
/// 「内嵌远程网关」一节——这条不参与无缝升级交接，`upgrade` 后新进程里初值永远是
/// None，由 `autostart_remote_from_config` 按落盘配置重新拉起。
pub(crate) struct RemoteGateway {
    pub(crate) token: String,
    pub(crate) addr: std::net::SocketAddr,
    pub(crate) write: bool,
    pub(crate) shutdown_tx: tokio::sync::oneshot::Sender<()>,
    pub(crate) _sleep_assertion: Option<SystemSleepAssertion>,
}

/// 远程网关持有的空闲防睡眠断言名称。合盖仍走 Clamshell Sleep。
pub(crate) const SLEEP_ASSERTION_NAME: &str = "Smelt remote access is enabled";
pub(crate) const SLEEP_ASSERTION_TYPE: &str = "PreventUserIdleSystemSleep";

/// 当前供电方式。只把明确的电池供电当 Battery；桌面、UPS、读失败都当插电，
/// 避免误判导致插电时手机连不上。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum PowerKind {
    Ac,
    Battery,
}

/// 插电：远程开着就握断言，手机随时能连（旧行为）。
/// 电池：绝不阻止任何休眠。远程只能在电脑本身醒着、正在用的时候连；
/// 一旦睡着，iroh/网关一起停，手机连不上。
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

/// 按供电方式拿起或放下空闲防睡眠断言。电池上即使有手机连着也不握。
pub(crate) fn sync_remote_sleep_assertion(state: &RemoteState) {
    let mut guard = state.lock().unwrap();
    if guard.stopping {
        return;
    }
    let desired = sleep_assertion_desired(current_power_kind());
    let Some(gateway) = guard.gateway.as_mut() else {
        return;
    };
    let holding = gateway._sleep_assertion.is_some();
    if desired == holding {
        return;
    }
    if desired {
        match SystemSleepAssertion::acquire() {
            Ok(assertion) => {
                gateway._sleep_assertion = Some(assertion);
                dlog("插电，已持有远程电源断言");
            }
            Err(code) => {
                dlog(&format!("远程服务无法阻止系统空闲睡眠：IOKit {code}"));
            }
        }
    } else {
        gateway._sleep_assertion = None;
        dlog("电池供电，已释放远程电源断言，不阻止任何休眠");
    }
}

fn run_power_source_watch(state: RemoteState) {
    let mut last = None;
    loop {
        thread::sleep(Duration::from_secs(2));
        {
            let guard = state.lock().unwrap();
            if guard.stopping || guard.gateway.is_none() {
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

pub(crate) struct RemoteStateData {
    pub(crate) gateway: Option<RemoteGateway>,
    /// 持久化的设备凭证。`None` 只存在于冷启动尚未开启远程时；第一次启动网关
    /// 会从磁盘读取或创建，之后重启网关、切换写权限都复用它。
    pub(crate) token: Option<String>,
    /// 所有网关代际与已建立 WebSocket 共享的写权限。权限切换只更新这一个原子值，
    /// 不需要断开手机当前 ACP 对话。
    pub(crate) write_enabled: Arc<AtomicBool>,
    /// 交接 COMMIT 后由 `cleanup_sidecar_services` 设为 true，禁止旧代 autostart
    /// 再创建 sidecar；COMMIT 前不会停止 sidecar，失败时无需回滚此状态。
    pub(crate) stopping: bool,
    /// 每次显式停止 iroh 都推进代际。守护启动时捕获的 autostart 只能在原代际重试，
    /// 因而不会越过用户后来的关闭、token 轮换或 upgrade cleanup。
    iroh_autostart_generation: u64,
}

pub(crate) type RemoteState = Arc<Mutex<RemoteStateData>>;

pub(crate) fn new_remote_state(token: Option<String>) -> RemoteState {
    Arc::new(Mutex::new(RemoteStateData {
        gateway: None,
        token,
        write_enabled: Arc::new(AtomicBool::new(false)),
        stopping: false,
        iroh_autostart_generation: 0,
    }))
}

pub(crate) fn remote_token_path() -> Result<std::path::PathBuf, String> {
    smelt_paths::smelt_home()
        .map(|root| root.join("remote-token"))
        .ok_or_else(|| "找不到用户目录，无法保存远程配对 Token".to_string())
}

pub(crate) fn valid_remote_token(token: &str) -> bool {
    token.len() == 32 && token.bytes().all(|byte| byte.is_ascii_hexdigit())
}

pub(crate) fn persist_remote_token(path: &std::path::Path, token: &str) -> Result<(), String> {
    use std::io::Write as _;

    let dir = path
        .parent()
        .ok_or_else(|| "远程配对 Token 路径没有父目录".to_string())?;
    std::fs::create_dir_all(dir)
        .map_err(|error| format!("创建 {} 失败：{error}", dir.display()))?;
    let staged = path.with_extension(format!("tmp-{}", std::process::id()));
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create(true).truncate(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt as _;
        options.mode(0o600);
    }
    let write_result = (|| -> Result<(), String> {
        let mut file = options
            .open(&staged)
            .map_err(|error| format!("写入 {} 失败：{error}", staged.display()))?;
        file.write_all(token.as_bytes())
            .map_err(|error| format!("写入 {} 失败：{error}", staged.display()))?;
        file.sync_all()
            .map_err(|error| format!("同步 {} 失败：{error}", staged.display()))?;
        std::fs::rename(&staged, path)
            .map_err(|error| format!("替换 {} 失败：{error}", path.display()))?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))
                .map_err(|error| format!("收紧 {} 权限失败：{error}", path.display()))?;
        }
        Ok(())
    })();
    if write_result.is_err() {
        let _ = std::fs::remove_file(staged);
    }
    write_result
}

pub(crate) fn load_or_create_remote_token() -> Result<String, String> {
    load_or_create_remote_token_at(&remote_token_path()?)
}

pub(crate) fn load_or_create_remote_token_at(path: &std::path::Path) -> Result<String, String> {
    if let Ok(raw) = std::fs::read_to_string(path) {
        let token = raw.trim();
        if valid_remote_token(token) {
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt as _;
                std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))
                    .map_err(|error| format!("收紧 {} 权限失败：{error}", path.display()))?;
            }
            return Ok(token.to_string());
        }
    }
    let token = uuid::Uuid::new_v4().simple().to_string();
    persist_remote_token(path, &token)?;
    Ok(token)
}

pub(crate) fn rotate_remote_token(state: &RemoteState) -> Result<String, String> {
    rotate_remote_token_at(state, &remote_token_path()?)
}

pub(crate) fn rotate_remote_token_at(
    state: &RemoteState,
    path: &std::path::Path,
) -> Result<String, String> {
    let token = uuid::Uuid::new_v4().simple().to_string();
    let mut guard = state.lock().unwrap();
    if let Some(gateway) = guard.gateway.take() {
        let _ = gateway.shutdown_tx.send(());
    }
    #[cfg(target_os = "macos")]
    SystemSleepAssertion::release_orphans();
    persist_remote_token(path, &token)?;
    guard.token = Some(token.clone());
    Ok(token)
}

/// 幂等：已经开着直接回现有 token/addr/write，不重启、不换 token——包括 `write`
/// 参数；运行中的写权限由 `remote_set_write` 单独热更新，bind/port 等监听参数
/// 仍需重开网关。
/// bind 非法 / 端口绑不上 / 服务线程起不来都走 Err，调用方原样透传给客户端。
///
/// **先等 serve 就绪再写 `RemoteState`**：以前 spawn 后立刻标 running，子线程
/// `Runtime::new`/`from_std` 失败时状态假活，幂等路径永远回死 token。
pub(crate) fn start_remote_gateway(
    state: &RemoteState,
    bind: &str,
    port: u16,
    write: bool,
) -> Result<(String, std::net::SocketAddr, bool), String> {
    let mut guard = state.lock().unwrap();
    if guard.stopping {
        return Err("守护正在升级，远程网关已停止".into());
    }
    if let Some(g) = guard.gateway.as_ref() {
        return Ok((g.token.clone(), g.addr, g.write));
    }

    let token = match guard.token.clone() {
        Some(token) => token,
        None => {
            let token = load_or_create_remote_token()?;
            guard.token = Some(token.clone());
            token
        }
    };

    let ip: std::net::IpAddr = bind
        .parse()
        .map_err(|e| format!("非法绑定地址 {bind}：{e}"))?;
    let std_listener = std::net::TcpListener::bind((ip, port))
        .map_err(|e| format!("绑定 {bind}:{port} 失败：{e}"))?;
    std_listener
        .set_nonblocking(true)
        .map_err(|e| e.to_string())?;
    let addr = std_listener.local_addr().map_err(|e| e.to_string())?;
    guard.write_enabled.store(write, Ordering::SeqCst);
    let write_state_for_thread = Arc::clone(&guard.write_enabled);

    let (shutdown_tx, shutdown_rx) = tokio::sync::oneshot::channel::<()>();
    // 子线程认领 listener / 建 runtime 成功才算 ready；失败则本函数 Err 且不写 state。
    let (ready_tx, ready_rx) = std::sync::mpsc::channel::<Result<(), String>>();

    let token_for_thread = token.clone();
    thread::spawn(move || {
        let rt = match tokio::runtime::Runtime::new() {
            Ok(rt) => rt,
            Err(e) => {
                let msg = format!("远程网关起不了 tokio runtime：{e}");
                eprintln!("{msg}");
                let _ = ready_tx.send(Err(msg));
                return;
            }
        };
        rt.block_on(async move {
            let listener = match tokio::net::TcpListener::from_std(std_listener) {
                Ok(l) => l,
                Err(e) => {
                    let msg = format!("远程网关认领监听 fd 失败：{e}");
                    eprintln!("{msg}");
                    let _ = ready_tx.send(Err(msg));
                    return;
                }
            };
            // listener 已就绪，即将 serve——此时可以对外报 running。
            let _ = ready_tx.send(Ok(()));
            let app = remote_gateway::build_router_with_write_state(
                token_for_thread,
                write_state_for_thread,
            );
            let serve = axum::serve(listener, app).with_graceful_shutdown(async move {
                let _ = shutdown_rx.await;
            });
            if let Err(e) = serve.await {
                eprintln!("远程网关退出：{e}");
            }
        });
    });

    match ready_rx.recv_timeout(Duration::from_secs(5)) {
        Ok(Ok(())) => {
            guard.gateway = Some(RemoteGateway {
                token: token.clone(),
                addr,
                write,
                shutdown_tx,
                _sleep_assertion: None,
            });
            drop(guard);
            sync_remote_sleep_assertion(state);
            if !cfg!(test) {
                let watch_state = Arc::clone(state);
                thread::spawn(move || run_power_source_watch(watch_state));
            }
            Ok((token, addr, write))
        }
        Ok(Err(e)) => Err(e),
        Err(_) => Err("远程网关启动超时（5s）".into()),
    }
}

pub(crate) fn stop_remote_gateway(state: &RemoteState) {
    stop_remote_gateway_inner(state, false);
}

pub(crate) fn stop_remote_gateway_inner(state: &RemoteState, stopping: bool) {
    let mut guard = state.lock().unwrap();
    if stopping {
        guard.stopping = true;
    }
    if let Some(g) = guard.gateway.take() {
        let _ = g.shutdown_tx.send(());
    }
    #[cfg(target_os = "macos")]
    SystemSleepAssertion::release_orphans();
}

pub(crate) fn set_remote_gateway_write(state: &RemoteState, write: bool) -> Result<(), String> {
    let mut guard = state.lock().unwrap();
    guard.write_enabled.store(write, Ordering::SeqCst);
    let Some(gateway) = guard.gateway.as_mut() else {
        return Err("远程网关未运行".into());
    };
    gateway.write = write;
    Ok(())
}

/// iroh 隧道（见 `crates/smelt-iroh`）：把本机远程网关经 P2P 暴露出去。
///
/// 跟已下线的 Cloudflare 隧道相比的关键差别 —— 也是留下这条路的理由：
/// 1. `endpoint_id` 由落盘私钥决定，**重启不变**，配对二维码可以永久有效。
/// 2. 优先打洞直连，打不通才走中继，不是全程第三方中转。
/// 3. 没有子进程，因此没有孤儿进程风险。
///
/// 私钥落在 `~/.smelt/iroh-secret`，与命令行 `smelt-iroh-host` 共用同一把，
/// 这样两种起法给出的配对码是同一个。
pub(crate) struct IrohTunnel {
    pub(crate) endpoint_id: String,
    pub(crate) relay: smelt_iroh::RelaySettings,
    pub(crate) shutdown_tx: tokio::sync::oneshot::Sender<()>,
    /// 已连接的移动端设备，按隧道代际与 QUIC 连接实例精确跟踪。
    pub(crate) connections: IrohConnections,
    connection_generation: u64,
}

/// 单个已连接设备的信息。
#[derive(Clone, Debug, serde::Serialize)]
pub struct IrohConnection {
    /// iroh 节点 ID（公钥的十六进制表示）。
    pub remote_id: String,
    /// 连接建立的时间戳（Unix 秒）。
    pub connected_at: u64,
}

pub(crate) type IrohState = Arc<Mutex<Option<IrohTunnel>>>;

#[derive(Default)]
pub(crate) struct IrohConnectionsData {
    generation: u64,
    by_remote: HashMap<String, HashMap<usize, u64>>,
}

/// 连接回调可能晚于 tunnel stop；代际用于拒绝旧 tunnel 的迟到事件。
pub(crate) type IrohConnections = Arc<Mutex<IrohConnectionsData>>;

pub(crate) fn new_iroh_connections() -> IrohConnections {
    Arc::new(Mutex::new(IrohConnectionsData::default()))
}

pub(crate) fn begin_iroh_connection_generation(connections: &IrohConnections) -> u64 {
    let mut guard = connections.lock().unwrap();
    guard.generation = guard.generation.wrapping_add(1);
    guard.by_remote.clear();
    guard.generation
}

fn end_iroh_connection_generation(connections: &IrohConnections, generation: u64) {
    let mut guard = connections.lock().unwrap();
    if guard.generation == generation {
        guard.generation = guard.generation.wrapping_add(1);
        guard.by_remote.clear();
    }
}

pub(crate) fn track_iroh_connection_event(
    connections: &IrohConnections,
    generation: u64,
    event: smelt_iroh::ConnectionEvent,
) {
    let mut guard = connections.lock().unwrap();
    if guard.generation != generation {
        return;
    }
    match event {
        smelt_iroh::ConnectionEvent::Connected {
            connection_id,
            remote_id,
            connected_at,
        } => {
            dlog(&format!("iroh 设备已连接：{remote_id}"));
            guard
                .by_remote
                .entry(remote_id)
                .or_default()
                .insert(connection_id, connected_at);
        }
        smelt_iroh::ConnectionEvent::Disconnected {
            connection_id,
            remote_id,
        } => {
            dlog(&format!("iroh 设备已断开：{remote_id}"));
            if let std::collections::hash_map::Entry::Occupied(mut entry) =
                guard.by_remote.entry(remote_id)
            {
                entry.get_mut().remove(&connection_id);
                if entry.get().is_empty() {
                    entry.remove();
                }
            }
        }
    }
}

pub(crate) fn snapshot_iroh_connections(
    connections: &IrohConnections,
    generation: u64,
) -> Vec<IrohConnection> {
    let guard = connections.lock().unwrap();
    if guard.generation != generation {
        return Vec::new();
    }
    let mut snapshot = guard
        .by_remote
        .iter()
        .filter_map(|(remote_id, instances)| {
            instances
                .values()
                .copied()
                .min()
                .map(|connected_at| IrohConnection {
                    remote_id: remote_id.clone(),
                    connected_at,
                })
        })
        .collect::<Vec<_>>();
    snapshot.sort_by(|left, right| left.remote_id.cmp(&right.remote_id));
    snapshot
}

pub(crate) fn iroh_autostart_generation(state: &RemoteState) -> u64 {
    state.lock().unwrap().iroh_autostart_generation
}

pub(crate) fn iroh_autostart_is_current(state: &RemoteState, generation: u64) -> bool {
    let guard = state.lock().unwrap();
    !guard.stopping && guard.iroh_autostart_generation == generation
}

/// 串行化 `start_iroh`：绑定要联网、最长 30s，期间不能一直攥着 `IrohState`
/// （`iroh_status` 等只读路径会被一起堵死），可一旦放开，两个并发调用就会各自
/// 绑一个 endpoint，后写入的顶掉先写入的。守护自愈与 GUI 补发正好可能同时发生，
/// 所以这里单独用一把「启动锁」，把幂等检查和绑定圈在同一段临界区里。
pub(crate) static IROH_START_LOCK: Mutex<()> = Mutex::new(());

/// 幂等：已经开着直接回现有 endpoint_id。会先确保远程网关按 `write` 开着
/// （隧道要转发给它），语义与 `start_tunnel` 一致。
///
/// 与网关同样「先等就绪再写 state」：iroh 绑定要联网发现中继，失败率不低，
/// 抢先标 running 会让幂等路径永远回一个连不上的 endpoint_id。
pub(crate) fn start_iroh(
    iroh_state: &IrohState,
    remote_state: &RemoteState,
    write: bool,
    relay_address: &str,
    connections: IrohConnections,
) -> Result<(String, String, std::net::SocketAddr, bool, String), String> {
    let relay = smelt_iroh::RelaySettings::parse(relay_address)
        .map_err(|e| format!("iroh relay 配置无效：{e:#}"))?;
    // 锁中毒（某次启动 panic 过）不该让远程从此再也起不来：拿回内层的 () 继续。
    let _start_guard = IROH_START_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    if let Some(t) = iroh_state.lock().unwrap().as_ref() {
        if t.relay != relay {
            return Err("iroh relay 配置已变化，请先停止旧隧道再重试".into());
        }
        let (token, addr, effective_write) = {
            let guard = remote_state.lock().unwrap();
            match guard.gateway.as_ref() {
                Some(g) => (g.token.clone(), g.addr, g.write),
                // 网关被单独停掉了：报错而不是回一个通往虚空的配对码。
                None => return Err("iroh 隧道开着但本机网关已停，请先 iroh_stop".into()),
            }
        };
        return Ok((
            t.endpoint_id.clone(),
            token,
            addr,
            effective_write,
            t.relay.url.to_string(),
        ));
    }

    let (token, addr, effective_write) = ensure_remote_gateway_with_write(remote_state, write)?;
    let connection_generation = begin_iroh_connection_generation(&connections);

    let (shutdown_tx, shutdown_rx) = tokio::sync::oneshot::channel::<()>();
    let (ready_tx, ready_rx) = std::sync::mpsc::channel::<Result<String, String>>();

    let tunnel_relay = relay.clone();
    let conn_tracker = Arc::clone(&connections);
    thread::spawn(move || {
        let rt = match tokio::runtime::Runtime::new() {
            Ok(rt) => rt,
            Err(e) => {
                let _ = ready_tx.send(Err(format!("iroh 起不了 tokio runtime：{e}")));
                return;
            }
        };
        rt.block_on(async move {
            let secret = match smelt_iroh::default_secret_path()
                .and_then(|p| smelt_iroh::load_or_create_secret(&p))
            {
                Ok(s) => s,
                Err(e) => {
                    let _ = ready_tx.send(Err(format!("iroh 密钥不可用：{e:#}")));
                    return;
                }
            };
            let endpoint = match smelt_iroh::bind_endpoint(
                secret,
                vec![smelt_iroh::ALPN.to_vec()],
                &tunnel_relay,
            )
            .await
            {
                Ok(ep) => ep,
                Err(e) => {
                    let _ = ready_tx.send(Err(format!("iroh 绑定失败：{e:#}")));
                    return;
                }
            };
            let _ = ready_tx.send(Ok(endpoint.id().to_string()));
            let path_observer = std::sync::Arc::new(|status: smelt_iroh::PathStatus| {
                dlog(&format!(
                    "iroh path remote={} kind={} address={} rtt_ms={}",
                    status.remote,
                    status.kind,
                    status.address,
                    status.rtt.as_millis()
                ));
            });
            let conn_observer = std::sync::Arc::new(move |event| {
                track_iroh_connection_event(&conn_tracker, connection_generation, event)
            });
            smelt_iroh::serve_tunnel_with_observers(
                endpoint,
                addr,
                async move {
                    let _ = shutdown_rx.await;
                },
                path_observer,
                conn_observer,
            )
            .await;
        });
    });

    // 30s：绑定要连接用户配置的 relay，比本地绑端口慢得多，5s 在弱网下会误判失败。
    match ready_rx.recv_timeout(Duration::from_secs(30)) {
        Ok(Ok(endpoint_id)) => {
            *iroh_state.lock().unwrap() = Some(IrohTunnel {
                endpoint_id: endpoint_id.clone(),
                relay: relay.clone(),
                shutdown_tx,
                connections,
                connection_generation,
            });
            Ok((
                endpoint_id,
                token,
                addr,
                effective_write,
                relay.url.to_string(),
            ))
        }
        Ok(Err(e)) => {
            end_iroh_connection_generation(&connections, connection_generation);
            Err(e)
        }
        Err(_) => {
            end_iroh_connection_generation(&connections, connection_generation);
            Err("iroh 隧道启动超时（30s）".into())
        }
    }
}

pub(crate) fn stop_iroh(state: &IrohState, remote_state: &RemoteState) {
    {
        let mut remote = remote_state.lock().unwrap();
        remote.iroh_autostart_generation = remote.iroh_autostart_generation.wrapping_add(1);
    }
    // 与完整 start 临界区线性化：返回成功时，之前开始的 bind 不可能再发布新 tunnel。
    let _start_guard = IROH_START_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    if let Some(t) = state.lock().unwrap().take() {
        end_iroh_connection_generation(&t.connections, t.connection_generation);
        let _ = t.shutdown_tx.send(());
    }
}

pub(crate) fn iroh_status(state: &IrohState) -> Option<(String, smelt_iroh::RelaySettings)> {
    state
        .lock()
        .unwrap()
        .as_ref()
        .map(|t| (t.endpoint_id.clone(), t.relay.clone()))
}

/// 查询当前已连接的移动端设备列表。
pub(crate) fn get_iroh_connections(state: &IrohState) -> Vec<IrohConnection> {
    state
        .lock()
        .unwrap()
        .as_ref()
        .map(|t| snapshot_iroh_connections(&t.connections, t.connection_generation))
        .unwrap_or_default()
}

/// 守护启动时按落盘配置自动恢复远程访问。
///
/// 为什么必须由守护自己做：网关和隧道的运行态**不**参与无缝升级交接，每个新进程
/// 起来都是空的。而「远程开着」这个意愿只记在主库 `collab` 作用域里，以前只有
/// GUI 冷启动那一次会去 `remote_start`/`iroh_start`。于是只要守护单独重启过
/// （设置页「重启守护进程」、无缝升级 exec、崩溃后被 `ensure_daemon_running` 拉起），
/// 就没有任何人再把它们拉回来——手机侧表现为「连不上，得去设置页把远程关掉再打开」。
/// 关掉再打开之所以有效，只是因为那条路径重新发了这两条 op。
///
/// 走后台线程：iroh 绑定要联网发现 relay，最坏 30s，绝不能挡住 accept 循环。
/// 登录后网络还没就绪很常见，因此失败要退避重试，而不是一次失败就放弃到下次重启。
pub(crate) fn autostart_remote_from_config(
    remote_state: RemoteState,
    iroh_state: IrohState,
    iroh_connections: IrohConnections,
) {
    // 逃生阀：跑测试 / 排障时不希望守护自作主张连网。
    if std::env::var_os("SMELT_NO_REMOTE_AUTOSTART").is_some() {
        return;
    }
    spawn_remote_autostart(
        smelt_core::remote_config::load(),
        remote_state,
        iroh_state,
        iroh_connections,
    );
}

/// 网关自启的 AddrInUse 判定：Rust io 错误文案不随 locale 变化，
/// macOS `os error 48` / Linux `os error 98` 双保险。
fn is_addr_in_use_message(message: &str) -> bool {
    message.contains("Address already in use")
        || message.contains("os error 48")
        || message.contains("os error 98")
}

/// `autostart_remote_from_config` 里除「读配置」以外的部分。拆出来是为了能测
/// 「配置说关就一动不动」这条——读配置那步依赖 `$HOME`，改环境变量的测试跨线程不可靠。
///
/// 返回是否真的起了后台恢复线程。
pub(crate) fn spawn_remote_autostart(
    config: smelt_core::remote_config::RemoteConfig,
    remote_state: RemoteState,
    iroh_state: IrohState,
    iroh_connections: IrohConnections,
) -> bool {
    if !config.enabled {
        return false;
    }
    let autostart_generation = iroh_autostart_generation(&remote_state);

    thread::spawn(move || {
        if !iroh_autostart_is_current(&remote_state, autostart_generation) {
            dlog("远程意愿已变化，跳过远程网关自动恢复");
            return;
        }
        // 网关只绑回环、不联网，先起它：即使 relay 没配好，GUI 侧「本机链接」
        // 和后续的 iroh_start 幂等路径也有东西可用。
        //
        // AddrInUse 有界重试：交接 v2 里 successor 在 predecessor 退出中的毫秒
        // 窗口里自启会撞端口（stop 在 COMMIT 发送之后，见 predecessor::run_transaction）。
        // 只重试"地址被占"（io 文案不随 locale 变，可匹配；48=macOS，98=Linux），
        // 其它错误（非法地址/权限）一次失败就停，不空转。
        let mut gateway_addr = None;
        for attempt in 0..5 {
            match ensure_remote_gateway_with_write(&remote_state, config.write_enabled) {
                Ok((_, addr, _)) => {
                    gateway_addr = Some(addr);
                    break;
                }
                Err(error) if is_addr_in_use_message(&error) && attempt < 4 => {
                    dlog(&format!(
                        "远程网关端口被占（多半是对端退出中），1s 后重试（第 {} 次）：{error}",
                        attempt + 1
                    ));
                    thread::sleep(Duration::from_secs(1));
                }
                Err(error) => {
                    dlog(&format!("自动恢复远程网关失败：{error}"));
                    return;
                }
            }
        }
        match gateway_addr {
            Some(addr) => dlog(&format!("按配置自动恢复远程网关：{addr}")),
            None => return,
        }

        if config.iroh_relay.trim().is_empty() {
            dlog("未配置 iroh relay，跳过隧道自动恢复");
            return;
        }

        // 退避重试：绑定失败几乎都是「网络还没好」，隔一会儿就能成。
        const BACKOFF: [u64; 5] = [0, 3, 10, 30, 60];
        for (attempt, delay) in BACKOFF.iter().enumerate() {
            if *delay > 0 {
                thread::sleep(Duration::from_secs(*delay));
            }
            if !iroh_autostart_is_current(&remote_state, autostart_generation) {
                dlog("远程意愿已变化，取消 iroh 隧道自动恢复");
                return;
            }
            // 期间用户可能已经手动开好了（GUI 冷启动那条路），幂等直接认账。
            if iroh_state.lock().unwrap().is_some() {
                return;
            }
            match start_iroh(
                &iroh_state,
                &remote_state,
                config.write_enabled,
                &config.iroh_relay,
                Arc::clone(&iroh_connections),
            ) {
                Ok((endpoint_id, _, _, _, _)) => {
                    dlog(&format!("按配置自动恢复 iroh 隧道：{endpoint_id}"));
                    return;
                }
                Err(e) => dlog(&format!(
                    "自动恢复 iroh 隧道失败（第 {} 次）：{e}",
                    attempt + 1
                )),
            }
        }
        dlog("iroh 隧道自动恢复重试用尽，等待 GUI 或用户手动重试");
    });
    true
}

#[cfg(test)]
pub(crate) struct RemoteGatewayTestLock {
    _lock: std::sync::MutexGuard<'static, ()>,
    previous_power: Option<PowerKind>,
}

#[cfg(test)]
impl Drop for RemoteGatewayTestLock {
    fn drop(&mut self) {
        set_power_kind_override(self.previous_power);
    }
}

#[cfg(test)]
pub(crate) fn lock_remote_gateway_tests() -> RemoteGatewayTestLock {
    static LOCK: Mutex<()> = Mutex::new(());
    let _lock = LOCK.lock().unwrap_or_else(|e| e.into_inner());
    // 远程网关测试默认按插电算，避免开发机在电池上跑时断言策略抖动。
    let previous_power = set_power_kind_override(Some(PowerKind::Ac));
    RemoteGatewayTestLock {
        _lock,
        previous_power,
    }
}

/// 进程退出 / upgrade exec 前清理远程网关与 iroh 隧道。菜单栏 quit 与 accept 线程
/// 不同线程，靠这份 OnceLock 共享 Arc（main 启动时 register）。
pub(crate) static LIFECYCLE: std::sync::OnceLock<(RemoteState, IrohState)> =
    std::sync::OnceLock::new();

pub(crate) fn register_lifecycle(remote: RemoteState, iroh: IrohState) {
    let _ = LIFECYCLE.set((remote, iroh));
}

/// 关内嵌网关与 iroh 隧道。exit/exec 前必须调——否则 exec 后端口还被占着，
/// 新进程再开网关会撞上「address already in use」。
pub(crate) fn cleanup_sidecar_services() {
    if let Some((remote, iroh)) = LIFECYCLE.get() {
        // 先禁止 autostart：exec 不跑 Drop，cleanup 之后、exec 之前再 Create
        // 的断言会泄漏进下一份映像。iroh 要赶在网关之前停，否则正在转发的流
        // 会先撞上已经死掉的网关端口。
        remote.lock().unwrap().stopping = true;
        stop_iroh(iroh, remote);
        stop_remote_gateway_inner(remote, true);
    } else {
        #[cfg(target_os = "macos")]
        SystemSleepAssertion::release_orphans();
    }
}

pub(crate) fn ensure_remote_gateway_with_write(
    state: &RemoteState,
    write: bool,
) -> Result<(String, std::net::SocketAddr, bool), String> {
    {
        let guard = state.lock().unwrap();
        if let Some(g) = guard.gateway.as_ref()
            && g.write == write
        {
            return Ok((g.token.clone(), g.addr, g.write));
        }
    }
    // 先更新共享门闩，让尚未结束的旧 WebSocket 立即看到新策略；随后重开网关
    // 只是为了让新连接也拿到相同的共享状态，不再依赖连接断开来刷新权限。
    state
        .lock()
        .unwrap()
        .write_enabled
        .store(write, Ordering::SeqCst);
    stop_remote_gateway(state);
    start_remote_gateway(state, "127.0.0.1", 0, write)
}
