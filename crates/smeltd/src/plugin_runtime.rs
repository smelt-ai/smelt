//! Starts the plugin set built for this exact daemon binary.

use smelt_plugin_api::PluginId;
use smelt_plugin_host::{
    HostError, PluginPackage, PluginProcessVerifier, SharedBunHost, SharedBunHostOptions,
    active_plugin_set_root_for_daemon_id, discover_all_plugins,
};
use std::{
    collections::BTreeSet,
    path::PathBuf,
    sync::{
        Arc, Mutex, OnceLock,
        atomic::{AtomicI32, Ordering},
    },
    time::Duration,
};

static RUNTIME: OnceLock<Mutex<RuntimeState>> = OnceLock::new();
static PLUGIN_BOOT: OnceLock<Mutex<Option<PluginBoot>>> = OnceLock::new();
static RUNTIME_LIFECYCLE_GATE: Mutex<()> = Mutex::new(());
/// 共享 Bun 进程组。退出路径即使拿不到 lifecycle 闸门也能 SIGKILL，避免 reload
/// 死锁把 shutdown / predecessor stop 一起钉死。
static HOST_PGID: AtomicI32 = AtomicI32::new(0);

#[derive(Clone)]
struct PluginBoot {
    daemon_fingerprint: Option<String>,
}

fn plugin_boot() -> &'static Mutex<Option<PluginBoot>> {
    PLUGIN_BOOT.get_or_init(|| Mutex::new(None))
}

#[derive(Clone)]
struct RuntimeConfig {
    package_root: PathBuf,
    smelt_root: PathBuf,
    plugin_data_root: PathBuf,
}

#[derive(Default)]
struct RuntimeState {
    config: Option<RuntimeConfig>,
    hosts: RuntimeHosts,
    /// 启停/enable 时发布的贡献快照。open 路径只读这份，不碰 Bun 控制锁。
    contribution_cache: Vec<smelt_plugin_api::PluginContributionSet>,
}

#[derive(Default)]
struct RuntimeHosts {
    shared_bun: Option<Arc<SharedBunHost>>,
}

impl RuntimeState {
    fn take_hosts(&mut self) -> RuntimeHosts {
        std::mem::take(&mut self.hosts)
    }

    fn restart_config(&self) -> Option<RuntimeConfig> {
        if self.hosts.shared_bun.is_none() {
            return self.config.clone();
        }
        None
    }
}

struct PluginVerifier;

impl PluginProcessVerifier for PluginVerifier {
    fn verify(
        &self,
        package: &PluginPackage,
        program: &std::path::Path,
        pid: u32,
    ) -> Result<(), HostError> {
        super::protocol::verify_plugin_process(package, program, pid)
    }
}

pub(crate) fn start(daemon_fingerprint: Option<String>) {
    let Some(smelt_root) = smelt_paths::smelt_home() else {
        log_plugin_host("cannot determine plugin data directory");
        return;
    };
    let plugin_data_root = smelt_root.join("plugin-data");
    // 指纹钉死：调用方（main 启动 / handoff）传钉死值；None（单测直调）则就地
    // 哈希一次并存进 boot。此后 reload 只用 boot 里的钉死值，永不重哈希磁盘——
    // StageDiskOnly 后磁盘是新的、进程还是老的，重哈希会拿错插件集。
    let pinned = daemon_fingerprint.or_else(|| {
        super::daemon_executable_path()
            .ok()
            .and_then(|exe| smelt_plugin_host::executable_fingerprint(&exe).ok())
    });
    *plugin_boot()
        .lock()
        .unwrap_or_else(|error| error.into_inner()) = Some(PluginBoot {
        daemon_fingerprint: pinned.clone(),
    });
    match managed_plugin_root(pinned.as_deref()) {
        Ok(Some(package_root)) => {
            start_with_root(package_root, smelt_root, plugin_data_root);
        }
        Ok(None) => {
            // 映射未写好就先空转。GUI / make install 写完 daemon-sets 后会发
            // plugin_reload，由 reload() 再解析映射并拉起。
            log_plugin_host("plugin set not mapped yet; waiting for plugin_reload");
        }
        Err(error) => {
            log_plugin_host(&format!("cannot resolve managed plugin set: {error}"));
        }
    }
}

fn start_with_root(package_root: PathBuf, smelt_root: PathBuf, plugin_data_root: PathBuf) {
    replace_runtime(RuntimeConfig {
        package_root,
        smelt_root,
        plugin_data_root,
    });
}

fn log_plugin_host(message: &str) {
    eprintln!("[plugin-host] {message}");
    crate::dlog(&format!("plugin-host: {message}"));
}

pub(crate) fn stop() {
    kill_recorded_host_process();
    if let Ok(_lifecycle) = RUNTIME_LIFECYCLE_GATE.try_lock() {
        drop(retire_current_hosts());
    }
}

fn kill_recorded_host_process() {
    let pid = HOST_PGID.swap(0, Ordering::SeqCst);
    if pid > 1 {
        unsafe {
            libc::kill(-pid, libc::SIGKILL);
        }
    }
}

/// 受管运行时就位后重启插件集。脚本插件在运行时缺席时根本起不来，等它下载完成
/// 必须有人把它们拉起来，否则要等到下一次守护重启才可用。
pub(crate) fn reload_for_runtime_change() {
    reload();
}

/// 用户包安装、更新或卸载完成后重建同一个 supervisor。GUI 只改磁盘并发这个信号，
/// 不自己起插件进程；守护始终是唯一 owner。
pub(crate) fn reload_for_package_change() {
    reload();
}

fn reload() {
    let _lifecycle = RUNTIME_LIFECYCLE_GATE
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    // 必须先 clone 再放锁。`if let Some(config) = runtime().lock()....clone()` 会把
    // MutexGuard 活到 if 函数体结束；随后 replace 再 lock 同一把非可重入锁就死锁，
    // 插件 invoke / contributions / 产品级 Pi 对话会永远停在「启动中」。
    let existing = runtime()
        .lock()
        .unwrap_or_else(|error| error.into_inner())
        .config
        .clone();
    if let Some(config) = existing {
        replace_runtime_while_locked(config);
        return;
    }
    let Some(config) = resolve_boot_config() else {
        log_plugin_host("plugin_reload with no mapping yet; still waiting");
        return;
    };
    replace_runtime_while_locked(config);
}

fn resolve_boot_config() -> Option<RuntimeConfig> {
    let boot = plugin_boot()
        .lock()
        .unwrap_or_else(|error| error.into_inner())
        .clone()?;
    let package_root = match managed_plugin_root(boot.daemon_fingerprint.as_deref()) {
        Ok(Some(root)) => root,
        Ok(None) => return None,
        Err(error) => {
            log_plugin_host(&format!("cannot resolve managed plugin set: {error}"));
            return None;
        }
    };
    let smelt_root = smelt_paths::smelt_home()?;
    Some(RuntimeConfig {
        package_root,
        smelt_root: smelt_root.clone(),
        plugin_data_root: smelt_root.join("plugin-data"),
    })
}

pub(crate) fn restart_after_failed_exec() {
    let _lifecycle = RUNTIME_LIFECYCLE_GATE
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    let config = runtime()
        .lock()
        .unwrap_or_else(|error| error.into_inner())
        .restart_config();
    if let Some(config) = config {
        let hosts = start_hosts(&config);
        install_hosts(hosts, Some(config));
    }
}

/// Replaces every plugin supervisor without letting two generations dispatch concurrently.
fn replace_runtime_while_locked(config: RuntimeConfig) {
    let previous = retire_current_hosts();
    HOST_PGID.store(0, Ordering::SeqCst);
    drop(previous);
    let hosts = start_hosts(&config);
    install_hosts(hosts, Some(config));
}

fn install_hosts(hosts: RuntimeHosts, config: Option<RuntimeConfig>) {
    let pid = hosts
        .shared_bun
        .as_ref()
        .and_then(|host| host.process_id())
        .unwrap_or(0);
    HOST_PGID.store(pid as i32, Ordering::SeqCst);
    let cache = contribution_snapshot(hosts.shared_bun.as_deref());
    let mut state = runtime().lock().unwrap_or_else(|error| error.into_inner());
    state.hosts = hosts;
    state.contribution_cache = cache;
    if let Some(config) = config {
        state.config = Some(config);
    }
}

fn contribution_snapshot(
    host: Option<&SharedBunHost>,
) -> Vec<smelt_plugin_api::PluginContributionSet> {
    let mut contributions = host.map(SharedBunHost::contributions).unwrap_or_default();
    contributions.sort_by(|left, right| left.plugin_id.cmp(&right.plugin_id));
    contributions
}

fn republish_contribution_cache() {
    let host = shared_bun_host();
    let cache = contribution_snapshot(host.as_deref());
    runtime()
        .lock()
        .unwrap_or_else(|error| error.into_inner())
        .contribution_cache = cache;
}

fn replace_runtime(config: RuntimeConfig) {
    let _lifecycle = RUNTIME_LIFECYCLE_GATE
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    replace_runtime_while_locked(config);
}

fn retire_current_hosts() -> RuntimeHosts {
    runtime()
        .lock()
        .unwrap_or_else(|error| error.into_inner())
        .take_hosts()
}

/// 只在查表时握全局锁；host I/O（invoke / 启停进程）必须在锁外进行。
fn shared_bun_host() -> Option<Arc<SharedBunHost>> {
    runtime()
        .lock()
        .unwrap_or_else(|error| error.into_inner())
        .hosts
        .shared_bun
        .clone()
}

/// 会话打开、贡献查询只读启停时发布的快照，不碰 Bun 控制通道。
pub(crate) fn contributions() -> Vec<smelt_plugin_api::PluginContributionSet> {
    runtime()
        .lock()
        .unwrap_or_else(|error| error.into_inner())
        .contribution_cache
        .clone()
}

/// 当前所有插件的运行状态。设置页据此显示"跑没跑起来、为什么没起来"。
pub(crate) fn statuses() -> Vec<smelt_plugin_host::PluginStatus> {
    let mut statuses = shared_bun_host()
        .map(|host| host.statuses())
        .unwrap_or_default();
    statuses.sort_by(|left, right| left.plugin_id.cmp(&right.plugin_id));
    statuses
}

pub(crate) fn invoke(
    plugin_id: smelt_plugin_api::PluginId,
    request: smelt_plugin_api::InvocationRequest,
    timeout: Duration,
) -> Result<smelt_plugin_api::InvocationResponse, String> {
    let Some(host) = shared_bun_host() else {
        return Err("plugin is not installed".to_string());
    };
    if !has_plugin(host.statuses(), &plugin_id) {
        return Err("plugin is not installed".to_string());
    }
    host.invoke(plugin_id, request, timeout)
        .map_err(|error| error.to_string())
}

pub(crate) fn set_enabled(
    plugin_id: smelt_plugin_api::PluginId,
    enabled: bool,
) -> Result<(), String> {
    let mut enablement = smelt_core::plugin_enablement::PluginEnablement::load();
    enablement.set_enabled(plugin_id.as_str(), enabled);
    if let Err(error) = enablement.save() {
        eprintln!("[plugin-host] persist plugin enablement: {error}");
    }
    let Some(host) = shared_bun_host() else {
        return Ok(());
    };
    if !has_plugin(host.statuses(), &plugin_id) {
        return Ok(());
    }
    let result = host
        .set_enabled(plugin_id, enabled)
        .map_err(|error| error.to_string());
    republish_contribution_cache();
    if let Some(pid) = shared_bun_host().and_then(|host| host.process_id()) {
        HOST_PGID.store(pid as i32, Ordering::SeqCst);
    }
    result
}

fn runtime() -> &'static Mutex<RuntimeState> {
    RUNTIME.get_or_init(|| Mutex::new(RuntimeState::default()))
}

fn has_plugin(statuses: Vec<smelt_plugin_host::PluginStatus>, plugin_id: &PluginId) -> bool {
    statuses.iter().any(|status| status.plugin_id == *plugin_id)
}

fn start_hosts(config: &RuntimeConfig) -> RuntimeHosts {
    let disabled = smelt_core::plugin_enablement::PluginEnablement::load().disabled_plugin_ids();
    let packages = discover_all_plugins(Some(&config.package_root), &config.smelt_root)
        .into_iter()
        .filter_map(|result| match result {
            Ok(package) => Some(package),
            Err(error) => {
                log_plugin_host(&format!("plugin discovery rejected: {error}"));
                None
            }
        })
        .collect::<Vec<_>>();
    let mut seen = BTreeSet::new();
    if let Some(duplicate) = packages
        .iter()
        .map(|package| package.manifest().id.clone())
        .find(|id| !seen.insert(id.clone()))
    {
        log_plugin_host(&format!(
            "duplicate plugin id rejected across runtime hosts: {duplicate}"
        ));
        return RuntimeHosts::default();
    }

    let spawn = smelt_plugin_host::SpawnOptions {
        // 每次起插件集都重新解析一次：受管 bun 可能是守护启动之后才下载完的，
        // 捕获一份旧快照会让脚本插件一直起不来。
        bun: smelt_core::acp_conn::managed_bun_if_ready(),
        ..Default::default()
    };
    let verifier = Arc::new(PluginVerifier);
    let mut hosts = RuntimeHosts::default();
    if !packages.is_empty() {
        match SharedBunHost::start_packages(
            packages,
            config.plugin_data_root.clone(),
            verifier,
            SharedBunHostOptions { spawn, disabled },
        ) {
            Ok(host) => {
                if let Some(pid) = host.process_id() {
                    HOST_PGID.store(pid as i32, Ordering::SeqCst);
                }
                hosts.shared_bun = Some(Arc::new(host));
            }
            Err(error) => log_plugin_host(&format!("cannot start shared bun host: {error}")),
        }
    }
    log_plugin_host(&format!(
        "supervising bundled plugins from {} and user plugins from {}",
        config.package_root.display(),
        smelt_plugin_host::user_plugin_root(&config.smelt_root).display()
    ));
    hosts
}

fn managed_plugin_root(daemon_fingerprint: Option<&str>) -> Result<Option<PathBuf>, String> {
    #[cfg(debug_assertions)]
    if let Some(root) = std::env::var_os("SMELT_PLUGIN_ROOT") {
        return Ok(Some(PathBuf::from(root)));
    }
    let smelt_root =
        smelt_paths::smelt_home().ok_or_else(|| "cannot determine home directory".to_string())?;
    match daemon_fingerprint {
        Some(fingerprint) => active_plugin_set_root_for_daemon_id(&smelt_root, fingerprint)
            .map_err(|error| error.to_string()),
        // None=启动时钉死失败（文件已消失的孤儿进程）：宁可空转等 reload，
        // 也不对磁盘现哈希——StageDiskOnly 后现哈希拿到的是新二进制的映射，
        // 老进程会加载错插件集。
        None => Ok(None),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    static TEST_RUNTIME_GATE: Mutex<()> = Mutex::new(());

    fn lock_test_runtime() -> std::sync::MutexGuard<'static, ()> {
        TEST_RUNTIME_GATE
            .lock()
            .unwrap_or_else(|error| error.into_inner())
    }

    #[test]
    fn contributions_are_a_published_snapshot() {
        use smelt_plugin_api::{PluginContributionSet, PluginId};

        let _gate = lock_test_runtime();
        let expected = PluginContributionSet {
            plugin_id: PluginId::new("com.example.cached").unwrap(),
            name: "cached".into(),
            version: "1".into(),
            contributions: Vec::new(),
        };
        {
            let mut state = runtime().lock().unwrap_or_else(|error| error.into_inner());
            state.hosts = RuntimeHosts::default();
            state.contribution_cache = vec![expected.clone()];
        }
        let got = contributions();
        assert_eq!(got, vec![expected]);
        drop(runtime().try_lock().expect("读贡献快照不能握住 runtime 锁"));
        runtime()
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .contribution_cache
            .clear();
    }

    #[test]
    fn stop_does_not_wait_for_lifecycle_gate() {
        let _gate = lock_test_runtime();
        let (started_tx, started_rx) = std::sync::mpsc::channel();
        let holder = std::thread::spawn(move || {
            let _lifecycle = RUNTIME_LIFECYCLE_GATE
                .lock()
                .unwrap_or_else(|error| error.into_inner());
            let _ = started_tx.send(());
            std::thread::sleep(Duration::from_secs(2));
        });
        started_rx.recv().expect("lifecycle 闸门应已被占用");
        let started = std::time::Instant::now();
        stop();
        assert!(
            started.elapsed() < Duration::from_millis(500),
            "stop 在 reload 持有 lifecycle 时必须立刻 SIGKILL 记录的进程组，不能再排队等闸门"
        );
        holder.join().unwrap();
    }

    #[test]
    fn host_dispatch_releases_runtime_lock_before_returning() {
        let _gate = lock_test_runtime();
        let _ = contributions();
        let _ = statuses();
        drop(
            runtime()
                .try_lock()
                .expect("contributions/statuses 返回后 runtime 锁必须已释放"),
        );
    }

    #[test]
    fn contributions_can_run_while_another_thread_keeps_a_cloned_host() {
        use std::sync::mpsc;
        use std::time::Duration;

        let _gate = lock_test_runtime();
        let (hold_tx, hold_rx) = mpsc::channel::<Arc<SharedBunHost>>();
        let (release_tx, release_rx) = mpsc::channel::<()>();
        let thread = std::thread::spawn(move || {
            if let Some(host) = hold_rx.recv_timeout(Duration::from_secs(1)).ok() {
                let _ = host.statuses();
                let _ = release_rx.recv_timeout(Duration::from_secs(1));
                drop(host);
            }
        });

        if let Some(host) = shared_bun_host() {
            hold_tx.send(host).unwrap();
        }
        let _ = contributions();
        drop(
            runtime()
                .try_lock()
                .expect("clone 出 host 之后 runtime 锁必须空闲，不能把插件 I/O 堵在全局锁上"),
        );
        let _ = release_tx.send(());
        thread.join().unwrap();
    }

    #[test]
    fn reload_releases_runtime_lock_before_replacing_hosts() {
        use std::sync::mpsc;
        use std::time::Duration;

        let _gate = lock_test_runtime();
        let root = std::env::temp_dir().join(format!(
            "smeltd-plugin-reload-{}-{}",
            std::process::id(),
            uuid::Uuid::new_v4()
        ));
        std::fs::create_dir_all(&root).unwrap();
        {
            let mut state = runtime().lock().unwrap_or_else(|error| error.into_inner());
            state.config = Some(RuntimeConfig {
                package_root: root.clone(),
                smelt_root: root.join("smelt"),
                plugin_data_root: root.join("data"),
            });
        }

        let (tx, rx) = mpsc::channel();
        std::thread::spawn(move || {
            reload();
            let _ = tx.send(());
        });
        rx.recv_timeout(Duration::from_secs(3)).expect(
            "reload 在已有 config 时死锁：if let 仍握着 runtime 锁又进入 replace_runtime_while_locked",
        );
        drop(
            runtime()
                .try_lock()
                .expect("reload 返回后 runtime 锁必须已释放"),
        );
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn runtime_state_retains_restartable_config_after_hosts_stop() {
        let root = std::env::temp_dir().join(format!(
            "smeltd-plugin-runtime-{}-{}",
            std::process::id(),
            uuid::Uuid::new_v4()
        ));
        std::fs::create_dir_all(&root).unwrap();
        let config = RuntimeConfig {
            package_root: root.clone(),
            smelt_root: root.join("smelt"),
            plugin_data_root: root.join("data"),
        };
        let mut state = RuntimeState {
            hosts: start_hosts(&config),
            config: Some(config),
            contribution_cache: Vec::new(),
        };
        assert!(state.hosts.shared_bun.is_none());
        let stopped = state.take_hosts();
        assert!(state.hosts.shared_bun.is_none());
        drop(stopped);

        let config = state
            .restart_config()
            .expect("stopped runtime should retain its configuration");
        state.hosts = start_hosts(&config);
        state.config = Some(config);
        assert!(state.hosts.shared_bun.is_none());
        drop(state.take_hosts());
        std::fs::remove_dir_all(root).unwrap();
    }
}
