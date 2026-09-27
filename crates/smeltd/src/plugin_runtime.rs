//! Starts the plugin set built for this exact daemon binary.

use smelt_plugin_api::PluginId;
use smelt_plugin_host::{
    HostError, PluginPackage, PluginProcessTracker, PluginProcessVerifier, SharedBunHost,
    SharedBunHostOptions, active_plugin_set_root_for_daemon_id, discover_all_plugins,
};
use std::{
    collections::BTreeSet,
    path::PathBuf,
    sync::{Arc, Condvar, Mutex, OnceLock},
    time::Duration,
};

static RUNTIME: OnceLock<Mutex<RuntimeState>> = OnceLock::new();
static PLUGIN_BOOT: OnceLock<Mutex<Option<PluginBoot>>> = OnceLock::new();
/// Serializes host replacement with every daemon API that can keep a host alive or spawn Bun.
static RUNTIME_LIFECYCLE_GATE: Mutex<()> = Mutex::new(());
static PROCESS_TRACKER: OnceLock<Arc<DaemonPluginProcessTracker>> = OnceLock::new();

#[derive(Default)]
struct TrackedProcessState {
    cancelled: bool,
    spawning: bool,
    pid: Option<u32>,
}

#[derive(Default)]
struct DaemonPluginProcessTracker {
    state: Mutex<TrackedProcessState>,
    changed: Condvar,
}

impl DaemonPluginProcessTracker {
    fn prepare(&self) {
        let mut state = self.state.lock().unwrap_or_else(|error| error.into_inner());
        debug_assert!(!state.spawning && state.pid.is_none());
        state.cancelled = false;
    }

    fn resume(&self) {
        self.state
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .cancelled = false;
    }

    fn is_cancelled(&self) -> bool {
        self.state
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .cancelled
    }

    /// Linearize cancellation with `Command::spawn`: after this returns, no future spawn is
    /// admitted and every process whose spawn already began has received SIGKILL.
    fn cancel_and_kill(&self) {
        let mut state = self.state.lock().unwrap_or_else(|error| error.into_inner());
        state.cancelled = true;
        while state.spawning {
            state = self
                .changed
                .wait(state)
                .unwrap_or_else(|error| error.into_inner());
        }
        if let Some(pid) = state.pid {
            let process_group = pid as i32;
            let result = unsafe { libc::kill(-process_group, libc::SIGKILL) };
            if result != 0 {
                let error = std::io::Error::last_os_error();
                if error.raw_os_error() != Some(libc::ESRCH) {
                    let _ = unsafe { libc::kill(process_group, libc::SIGKILL) };
                }
            }
            wait_for_child_exit(pid);
        }
    }
}

fn wait_for_child_exit(pid: u32) {
    loop {
        let mut info = unsafe { std::mem::zeroed::<libc::siginfo_t>() };
        let result = unsafe {
            libc::waitid(
                libc::P_PID,
                pid as libc::id_t,
                &mut info,
                libc::WEXITED | libc::WNOWAIT,
            )
        };
        if result == 0 {
            return;
        }
        match std::io::Error::last_os_error().raw_os_error() {
            Some(libc::EINTR) => continue,
            // The host's Child owner won the waitpid race; the process has already exited.
            Some(libc::ECHILD) => return,
            _ => return,
        }
    }
}

impl PluginProcessTracker for DaemonPluginProcessTracker {
    fn before_spawn(&self) -> Result<(), HostError> {
        let mut state = self.state.lock().unwrap_or_else(|error| error.into_inner());
        if state.cancelled {
            return Err(HostError::new("plugin runtime startup was cancelled"));
        }
        if state.spawning || state.pid.is_some() {
            return Err(HostError::new(
                "another plugin runtime process is already active",
            ));
        }
        state.spawning = true;
        Ok(())
    }

    fn spawned(&self, pid: u32) {
        let mut state = self.state.lock().unwrap_or_else(|error| error.into_inner());
        state.pid = Some(pid);
        state.spawning = false;
        self.changed.notify_all();
    }

    fn spawn_failed(&self) {
        let mut state = self.state.lock().unwrap_or_else(|error| error.into_inner());
        state.spawning = false;
        self.changed.notify_all();
    }

    fn exited(&self, pid: u32) {
        let mut state = self.state.lock().unwrap_or_else(|error| error.into_inner());
        if state.pid == Some(pid) {
            state.pid = None;
        }
        self.changed.notify_all();
    }
}

fn process_tracker() -> &'static Arc<DaemonPluginProcessTracker> {
    PROCESS_TRACKER.get_or_init(|| Arc::new(DaemonPluginProcessTracker::default()))
}

#[derive(Clone)]
struct PluginBoot {
    daemon_fingerprint: Option<String>,
    managed_bun: Option<smelt_core::managed_runtime::ManagedBunRuntime>,
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
    /// Supervisor ownership: the generation cannot be collected until the host is reaped.
    managed_bun: Option<smelt_core::managed_runtime::ManagedBunRuntime>,
}

impl Drop for RuntimeHosts {
    fn drop(&mut self) {
        // SharedBunHost::drop performs terminate + wait. Release the supervisor lease only after
        // that direct child has been reaped; child-inherited FDs cover daemon crashes.
        drop(self.shared_bun.take());
        drop(self.managed_bun.take());
    }
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

/// Pin the plugin set identity before any asynchronous bootstrap work starts.
/// Package reloads may arrive while Bun is still being prepared; they must retain this identity
/// but cannot start a host until `start` receives the published generation and its lease.
pub(crate) fn prepare(daemon_fingerprint: Option<String>) {
    // 指纹钉死：调用方（main 启动 / handoff）传钉死值；None（单测直调）则就地
    // 哈希一次并存进 boot。此后 reload 只用 boot 里的钉死值，永不重哈希磁盘——
    // StageDiskOnly 后磁盘是新的、进程还是老的，重哈希会拿错插件集。
    let pinned = daemon_fingerprint.or_else(|| {
        super::daemon_executable_path()
            .ok()
            .and_then(|exe| smelt_plugin_host::executable_fingerprint(&exe).ok())
    });
    process_tracker().prepare();
    *plugin_boot()
        .lock()
        .unwrap_or_else(|error| error.into_inner()) = Some(PluginBoot {
        daemon_fingerprint: pinned,
        managed_bun: None,
    });
}

/// Start the pinned plugin set with the exact runtime generation returned by bootstrap.
/// This path deliberately never probes the global `current` pointer: manager-lock contention is
/// not runtime absence, and the lease already proves this generation is published and alive.
pub(crate) fn start(managed_bun: smelt_core::managed_runtime::ManagedBunRuntime) {
    let _lifecycle = RUNTIME_LIFECYCLE_GATE
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    let pinned = {
        let mut boot = plugin_boot()
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        let Some(boot) = boot.as_mut() else {
            log_plugin_host("plugin runtime was not prepared");
            return;
        };
        boot.managed_bun = Some(managed_bun.clone());
        if process_tracker().is_cancelled() {
            return;
        }
        boot.daemon_fingerprint.clone()
    };
    let Some(smelt_root) = smelt_paths::smelt_home() else {
        log_plugin_host("cannot determine plugin data directory");
        return;
    };
    match managed_plugin_root(pinned.as_deref()) {
        Ok(Some(package_root)) => replace_runtime_while_locked(
            RuntimeConfig {
                package_root,
                plugin_data_root: smelt_root.join("plugin-data"),
                smelt_root,
            },
            managed_bun,
        ),
        Ok(None) => {
            // 映射未写好就先空转。GUI / make install 写完 daemon-sets 后会发
            // plugin_reload，由 reload() 再解析映射并拉起。
            log_plugin_host("plugin set not mapped yet; waiting for plugin_reload");
        }
        Err(error) => log_plugin_host(&format!("cannot resolve managed plugin set: {error}")),
    }
}

fn log_plugin_host(message: &str) {
    eprintln!("[plugin-host] {message}");
    crate::dlog(&format!("plugin-host: {message}"));
}

pub(crate) fn stop() {
    process_tracker().cancel_and_kill();
    if let Ok(_lifecycle) = RUNTIME_LIFECYCLE_GATE.try_lock() {
        drop(retire_current_hosts());
    }
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
    let Some(managed_bun) = prepared_managed_bun() else {
        log_plugin_host("plugin_reload while Bun bootstrap is pending; deferring startup");
        return;
    };
    let existing = configured_runtime();
    if let Some(config) = existing {
        replace_runtime_while_locked(config, managed_bun);
        return;
    }
    let Some(config) = resolve_boot_config() else {
        log_plugin_host("plugin_reload with no mapping yet; still waiting");
        return;
    };
    replace_runtime_while_locked(config, managed_bun);
}

fn prepared_managed_bun() -> Option<smelt_core::managed_runtime::ManagedBunRuntime> {
    let boot = plugin_boot()
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    if process_tracker().is_cancelled() {
        return None;
    }
    boot.as_ref()?.managed_bun.clone()
}

fn configured_runtime() -> Option<RuntimeConfig> {
    runtime()
        .lock()
        .unwrap_or_else(|error| error.into_inner())
        .config
        .clone()
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
    process_tracker().resume();
    let previous = retire_current_hosts();
    drop(previous);
    let config = runtime()
        .lock()
        .unwrap_or_else(|error| error.into_inner())
        .restart_config()
        .or_else(resolve_boot_config);
    if let (Some(config), Some(managed_bun)) = (config, prepared_managed_bun()) {
        let hosts = start_hosts(&config, managed_bun);
        publish_hosts_if_active(hosts, Some(config));
    }
}

/// Replaces every plugin supervisor without letting two generations dispatch concurrently.
fn replace_runtime_while_locked(
    config: RuntimeConfig,
    managed_bun: smelt_core::managed_runtime::ManagedBunRuntime,
) {
    let previous = retire_current_hosts();
    drop(previous);
    let hosts = start_hosts(&config, managed_bun);
    publish_hosts_if_active(hosts, Some(config));
}

/// Never publish a host after lifecycle cancellation. A cancellation racing after this check is
/// still safe: the process tracker registered the child before spawn, and stop kills that exact
/// process group before allowing the predecessor to exit.
fn publish_hosts_if_active(hosts: RuntimeHosts, config: Option<RuntimeConfig>) {
    if process_tracker().is_cancelled() {
        drop(hosts);
        return;
    }
    install_hosts(hosts, config);
}

fn install_hosts(hosts: RuntimeHosts, config: Option<RuntimeConfig>) {
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
    let _lifecycle = RUNTIME_LIFECYCLE_GATE
        .lock()
        .unwrap_or_else(|error| error.into_inner());
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
    let _lifecycle = RUNTIME_LIFECYCLE_GATE
        .lock()
        .unwrap_or_else(|error| error.into_inner());
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
    let _lifecycle = RUNTIME_LIFECYCLE_GATE
        .lock()
        .unwrap_or_else(|error| error.into_inner());
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
    result
}

fn runtime() -> &'static Mutex<RuntimeState> {
    RUNTIME.get_or_init(|| Mutex::new(RuntimeState::default()))
}

pub(crate) fn bootstrap_retry_delay(attempt: u32) -> Duration {
    let exponent = attempt.saturating_sub(1).min(5);
    Duration::from_secs((1_u64 << exponent).min(30))
}

fn has_plugin(statuses: Vec<smelt_plugin_host::PluginStatus>, plugin_id: &PluginId) -> bool {
    statuses.iter().any(|status| status.plugin_id == *plugin_id)
}

fn start_hosts(
    config: &RuntimeConfig,
    managed_bun: smelt_core::managed_runtime::ManagedBunRuntime,
) -> RuntimeHosts {
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
        bun: Some(managed_bun.path.clone()),
        inherited_fds: vec![managed_bun.inherited_fd()],
        process_tracker: Some(process_tracker().clone()),
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
                hosts.shared_bun = Some(Arc::new(host));
                hosts.managed_bun = Some(managed_bun);
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
        let guard = TEST_RUNTIME_GATE
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        process_tracker().resume();
        guard
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
            if let Ok(host) = hold_rx.recv_timeout(Duration::from_secs(1)) {
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
    fn runtime_config_snapshot_releases_lock_before_replacement() {
        use std::sync::mpsc;
        use std::time::Duration;

        let _gate = lock_test_runtime();
        let root = std::env::temp_dir().join(format!(
            "smeltd-plugin-reload-{}-{}",
            std::process::id(),
            uuid::Uuid::new_v4()
        ));
        std::fs::create_dir_all(&root).unwrap();
        runtime()
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .config = Some(RuntimeConfig {
            package_root: root.clone(),
            smelt_root: root.join("smelt"),
            plugin_data_root: root.join("data"),
        });

        let (tx, rx) = mpsc::channel();
        std::thread::spawn(move || {
            let config = configured_runtime();
            let _ = tx.send(config.is_some());
        });
        assert!(rx.recv_timeout(Duration::from_secs(3)).expect(
            "cloning plugin config must not retain runtime lock and deadlock the replacement"
        ));
        drop(
            runtime()
                .try_lock()
                .expect("runtime config snapshot must release the runtime lock"),
        );
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn bootstrap_cancellation_blocks_and_drains_an_in_flight_spawn() {
        let tracker = Arc::new(DaemonPluginProcessTracker::default());
        tracker.prepare();
        tracker.before_spawn().unwrap();

        let stopping = Arc::clone(&tracker);
        let stop_thread = std::thread::spawn(move || stopping.cancel_and_kill());
        while !tracker.is_cancelled() {
            std::thread::yield_now();
        }
        assert!(
            tracker.before_spawn().is_err(),
            "取消后不得再开始新的插件进程"
        );
        tracker.spawn_failed();
        stop_thread.join().unwrap();
        assert!(tracker.is_cancelled());
        tracker.resume();
        assert!(
            !tracker.is_cancelled(),
            "exec 回滚后必须允许 supervisor 恢复"
        );
    }

    #[test]
    fn stop_waits_until_the_registered_child_has_exited() {
        use std::os::unix::process::CommandExt;

        let tracker = DaemonPluginProcessTracker::default();
        tracker.prepare();
        tracker.before_spawn().unwrap();
        let child = std::process::Command::new("/bin/sleep")
            .arg("30")
            .process_group(0)
            .spawn()
            .unwrap();
        let pid = child.id();
        tracker.spawned(pid);
        tracker.cancel_and_kill();
        let mut child = child;
        assert!(child.try_wait().unwrap().is_some());
        tracker.exited(pid);
        assert!(tracker.state.lock().unwrap().pid.is_none());
    }

    #[test]
    fn managed_runtime_bootstrap_backoff_is_bounded() {
        assert_eq!(bootstrap_retry_delay(1), Duration::from_secs(1));
        assert_eq!(bootstrap_retry_delay(2), Duration::from_secs(2));
        assert_eq!(bootstrap_retry_delay(5), Duration::from_secs(16));
        assert_eq!(bootstrap_retry_delay(u32::MAX), Duration::from_secs(30));
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
            hosts: RuntimeHosts::default(),
            config: Some(config),
            contribution_cache: Vec::new(),
        };

        let stopped = state.take_hosts();
        drop(stopped);
        assert!(
            state.restart_config().is_some(),
            "停止 host 后必须保留可用于 exec 回滚的配置"
        );
        std::fs::remove_dir_all(root).unwrap();
    }
}
