//! 本机 smeltd 的生命周期（设计文档 D3）。
//!
//! 拉起、升级、交接和过期探测。Windows 上升级是重启，不交接。
//! 终端网格、远程会话连接留在 `terminal`。

use std::io::{BufRead, BufReader, Read, Write};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;
#[cfg(unix)]
use std::sync::TryLockError;
use std::thread;
use std::time::Duration;

use smelt_runtime_client::RuntimeStream;
use smelt_runtime_client::protocol::daemon::DaemonOperation;

// ===================== smeltd 守护连接层 =====================

/// 本机运行时 socket 的路径。只用于生命周期管理：拉起后的错误提示、按 socket 反查
/// 守护进程、清理残留文件。连守护一律走 [`local_runtime`]。
fn sock_path() -> std::path::PathBuf {
    smelt_client::daemon_state::smeltd_sock_path()
}

fn local_runtime() -> smelt_runtime_client::RuntimeClient {
    smelt_client::daemon_state::local_runtime()
}

/// 守护常驻路径：与 `.app` 解耦。装 DMG / 覆盖 App 不会 cp 到正在跑的二进制，
/// 避免 macOS 对签名文件覆盖时 SIGKILL smeltd → 会话全灭 → 对话被「重新初始化」。
///
/// **硬约束**：正在跑的 smeltd 必须映射 `~/.smelt/bin/` 里的一份映像，绝不能映射
/// `Smelt.app` 里的那份。否则用户用 Finder 拖 DMG 覆盖 App 时内核会干掉守护。
/// `smeltd` 这个名字只是指向当前映像的符号链接。
fn managed_daemon_dir() -> std::path::PathBuf {
    smelt_paths::smelt_home()
        .unwrap_or_else(|| std::env::temp_dir().join(".smelt"))
        .join("bin")
}

fn bundled_daemon_file_name() -> &'static str {
    if cfg!(windows) { "smeltd.exe" } else { "smeltd" }
}

pub fn managed_daemon_path() -> std::path::PathBuf {
    smelt_sys::daemon_image::launch_executable(&managed_daemon_dir())
}

/// 路径是否落在某个 `.app` 包内（装 DMG 会被覆盖/删除）。
fn path_inside_app_bundle(p: &std::path::Path) -> bool {
    p.components()
        .any(|c| c.as_os_str().to_str().is_some_and(|s| s.ends_with(".app")))
}

/// 规范化路径后比较是否指向同一文件（symlink / 相对路径）。
fn same_daemon_path(a: &std::path::Path, b: &std::path::Path) -> bool {
    if a == b {
        return true;
    }
    match (std::fs::canonicalize(a), std::fs::canonicalize(b)) {
        (Ok(ca), Ok(cb)) => ca == cb,
        _ => false,
    }
}

/// 进程是否已住在 managed 目录。映像文件名是 `smeltd-<指纹>`，启动名是旁边的链接。
/// 不能只认文件名 `smeltd`：交接后的进程报的是映像自己的路径。
fn exe_is_managed(exe: &std::path::Path) -> bool {
    let managed = managed_daemon_path();
    if same_daemon_path(exe, &managed) {
        return true;
    }
    let dir = managed_daemon_dir();
    if exe.starts_with(&dir) {
        return true;
    }
    // 启动名已经拨走，进程仍报着原来的路径：比 inode。
    if let (Ok(em), Ok(mm)) = (std::fs::metadata(exe), std::fs::metadata(&managed)) {
        #[cfg(unix)]
        {
            use std::os::unix::fs::MetadataExt;
            if em.dev() == mm.dev() && em.ino() == mm.ino() {
                return true;
            }
        }
    }
    false
}

/// 等 smeltd 监听 socket 的上限。
///
/// 生产里 daemon 基本已常驻，5s 足够且不宜更久——GUI 卡在这里用户什么也做不了。
/// 测试二进制跑在自己的沙箱里，每个进程都要先把 debug 版 smeltd（数百 MB）复制进去
/// 再冷启动，5s 必然不够。放宽超时，而不是让用例回头去捞开发者真实安装的 daemon。
fn daemon_ready_timeout() -> Duration {
    if smelt_paths::running_under_test() {
        Duration::from_secs(60)
    } else {
        Duration::from_secs(5)
    }
}

/// 查找分发物（smeltd 等）时应当参照的可执行文件位置。
///
/// cargo 把测试二进制放在 `target/<profile>/deps/`，而分发物都在上一级的 profile
/// 目录。不抹掉 `deps` 这一层，用例就只能去捞开发者真实安装的 `~/.smelt`，既污染
/// 个人数据，在 CI 上也必然抓瞎。
fn distribution_exe() -> Option<std::path::PathBuf> {
    let exe = std::env::current_exe().ok()?;
    if smelt_paths::running_under_test()
        && let Some(profile_dir) = exe.parent().and_then(std::path::Path::parent)
    {
        return Some(profile_dir.join(exe.file_name()?));
    }
    Some(exe)
}

/// App 包 / cargo 同目录下的 smeltd（分发物，不是常驻运行路径）。
pub(crate) fn bundled_daemon_path() -> Option<std::path::PathBuf> {
    distribution_exe()
        .map(|e| e.with_file_name(bundled_daemon_file_name()))
        .filter(|p| p.is_file())
}

fn file_mtime_secs(p: &std::path::Path) -> Option<u64> {
    std::fs::metadata(p)
        .ok()?
        .modified()
        .ok()?
        .duration_since(std::time::UNIX_EPOCH)
        .ok()
        .map(|d| d.as_secs())
}

/// 两份 smeltd 是否是同一份构建。
///
/// 只比较文件大小会漏掉“新旧构建恰好同大小”的升级；只比较 mtime 又会把
/// 安装时一次普通的 copy 误判成新版本。这里先做便宜的 metadata 检查，再逐块
/// 比较内容，兼顾正确性和后台探测成本。
fn same_daemon_binary(a: &std::path::Path, b: &std::path::Path) -> bool {
    let (Ok(ma), Ok(mb)) = (std::fs::metadata(a), std::fs::metadata(b)) else {
        return false;
    };
    if ma.len() != mb.len() {
        return false;
    }
    let (Ok(mut fa), Ok(mut fb)) = (std::fs::File::open(a), std::fs::File::open(b)) else {
        return false;
    };
    let mut left = [0u8; 128 * 1024];
    let mut right = [0u8; 128 * 1024];
    loop {
        let (Ok(na), Ok(nb)) = (fa.read(&mut left), fb.read(&mut right)) else {
            return false;
        };
        if na != nb || left[..na] != right[..nb] {
            return false;
        }
        if na == 0 {
            return true;
        }
    }
}

/// 套接字无人应答，并且这份文件没有被任何进程映射，才可以覆盖 `smeltd`。
fn may_replace_managed_daemon_image(probe: &DaemonProbe, executable_mapped: bool) -> bool {
    matches!(probe, DaemonProbe::NotRunning) && !executable_mapped
}

/// 这个路径是不是某个活进程的可执行映像。安装方自己没打开它时，命中的就是正在跑的 daemon。
fn path_is_live_executable(path: &std::path::Path) -> bool {
    #[cfg(target_os = "macos")]
    {
        let Ok(c_path) =
            std::ffi::CString::new(std::os::unix::ffi::OsStrExt::as_bytes(path.as_os_str()))
        else {
            return true;
        };
        // 第一次只拿缓冲区大小。这个返回值在没人映射时也是正数，不能拿来判断。
        let hint = unsafe {
            proc_listpidspath(
                PROC_ALL_PIDS,
                0,
                c_path.as_ptr(),
                0,
                std::ptr::null_mut(),
                0,
            )
        };
        if hint <= 0 {
            return false;
        }
        let mut buf = vec![0u8; hint as usize];
        let bytes = unsafe {
            proc_listpidspath(
                PROC_ALL_PIDS,
                0,
                c_path.as_ptr(),
                0,
                buf.as_mut_ptr().cast(),
                hint,
            )
        };
        bytes > 0
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = path;
        false
    }
}

#[cfg(target_os = "macos")]
const PROC_ALL_PIDS: u32 = 1;

#[cfg(target_os = "macos")]
unsafe extern "C" {
    fn proc_listpidspath(
        ty: u32,
        typeinfo: u32,
        path: *const libc::c_char,
        pathflags: u32,
        buffer: *mut libc::c_void,
        buffersize: i32,
    ) -> i32;
}

fn reap_managed_daemon_images(dir: &std::path::Path) {
    if let Err(error) = smelt_sys::daemon_image::reap_unused_images(dir) {
        eprintln!("[workspace] 回收旧守护映像失败：{error}");
    }
}

/// 守护没在跑时发布一份不可变映像，并把启动名拨过去。
/// 启动名这个目录项仍被进程映射时，只挂 `smeltd.pending`，不拨链接。
fn install_managed_daemon_from(src: &std::path::Path) -> std::io::Result<std::path::PathBuf> {
    let dir = managed_daemon_dir();
    std::fs::create_dir_all(&dir)?;
    let managed = managed_daemon_path();
    let image = smelt_sys::daemon_image::publish_image(&dir, src)?;
    let need = !managed.is_file() || !same_daemon_binary(src, &managed);
    if need {
        if managed.is_file() && path_is_live_executable(&managed) {
            smelt_sys::daemon_image::retarget_pending_symlink(&dir, &image)?;
            eprintln!(
                "[workspace] {} 仍被进程映射，不改启动名，已挂上 smeltd.pending",
                managed.display()
            );
            reap_managed_daemon_images(&dir);
            return Ok(managed);
        }
        smelt_sys::daemon_image::retarget_launch_after_commit(&image)?;
        eprintln!(
            "[workspace] 已同步守护 {} → {}",
            src.display(),
            image.display()
        );
    }
    reap_managed_daemon_images(&dir);
    if managed.is_file() {
        Ok(managed)
    } else {
        Err(std::io::Error::new(
            std::io::ErrorKind::NotFound,
            format!("无法安装 smeltd 到 {}", managed.display()),
        ))
    }
}

fn pending_upgrade_exe() -> Option<std::path::PathBuf> {
    smelt_sys::daemon_image::pending_image(&managed_daemon_dir()).or_else(bundled_daemon_path)
}

/// 多 pane 同时 connect 时串行化迁移，避免并发 upgrade 踩踏。
static MANAGED_DAEMON_GATE: Mutex<()> = Mutex::new(());

struct ManagedDaemonFileLock {
    _file: smelt_sys::file_lock::ExclusiveFile,
}

fn acquire_file_lock(path: &std::path::Path) -> std::io::Result<ManagedDaemonFileLock> {
    Ok(ManagedDaemonFileLock {
        _file: smelt_sys::file_lock::ExclusiveFile::lock(path)?,
    })
}

/// 单次更新尝试不能排队等别的进程的 handoff；拿不到锁时交还给 UI 的可取消等待态。
fn try_acquire_file_lock(path: &std::path::Path) -> std::io::Result<Option<ManagedDaemonFileLock>> {
    Ok(smelt_sys::file_lock::ExclusiveFile::try_lock(path)?.map(|file| ManagedDaemonFileLock { _file: file }))
}

fn acquire_managed_daemon_file_lock() -> std::io::Result<ManagedDaemonFileLock> {
    let dir = managed_daemon_dir();
    std::fs::create_dir_all(&dir)?;
    acquire_file_lock(&dir.join("smeltd.install.lock"))
}

fn try_acquire_managed_daemon_file_lock() -> std::io::Result<Option<ManagedDaemonFileLock>> {
    let dir = managed_daemon_dir();
    std::fs::create_dir_all(&dir)?;
    try_acquire_file_lock(&dir.join("smeltd.install.lock"))
}

/// 确保守护跑在 `~/.smelt/bin/smeltd` 且不旧于 App 内分发物。
///
/// GUI 启动 / 连守护 / 装包后 **必须** 调用。返回 managed 路径（可能尚未有进程，
/// 但文件应已就位）。
pub fn ensure_managed_daemon_current() -> std::io::Result<std::path::PathBuf> {
    let _gate = MANAGED_DAEMON_GATE
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let _file_gate = acquire_managed_daemon_file_lock()?;
    remember_managed_daemon_ensure(
        ensure_managed_daemon_current_locked(),
        &CONNECT_MANAGED_ENSURED,
    )
}

fn remember_managed_daemon_ensure<T>(
    result: std::io::Result<T>,
    ensured: &AtomicBool,
) -> std::io::Result<T> {
    if result.is_ok() {
        ensured.store(true, Ordering::Relaxed);
    }
    result
}

fn ensure_managed_daemon_for_connect<F>(
    ensured: &AtomicBool,
    managed: std::path::PathBuf,
    ensure: F,
) -> std::io::Result<std::path::PathBuf>
where
    F: FnOnce() -> std::io::Result<std::path::PathBuf>,
{
    // 这里只能缓存昂贵的版本/迁移检查，不能缓存文件存在性。安装、升级或异常
    // 重启可能在 GUI 生命周期内替换/移走该文件，命中缓存后仍须自愈。
    if ensured.load(Ordering::Relaxed) && managed.is_file() {
        Ok(managed)
    } else {
        remember_managed_daemon_ensure(ensure(), ensured)
    }
}

fn ensure_managed_daemon_current_locked() -> std::io::Result<std::path::PathBuf> {
    let bundled = bundled_daemon_path();
    let managed = managed_daemon_path();

    if let Some(bundled) = &bundled {
        sync_bundled_pi(bundled);
    }
    // 无分发物：仅依赖已有 managed（开发机偶发）。
    let Some(bundled) = bundled else {
        return if managed.is_file() {
            // 仍可能需要把「跑在 .app 里」的旧守护迁走
            let _ = migrate_running_daemon_off_app(&managed);
            Ok(managed)
        } else {
            Err(std::io::Error::new(
                std::io::ErrorKind::NotFound,
                "找不到 smeltd（App 包与 ~/.smelt/bin 皆无）",
            ))
        };
    };

    match probe_daemon_detail() {
        DaemonProbe::NotRunning => {
            install_managed_daemon_from(&bundled)?;
            Ok(managed_daemon_path())
        }
        DaemonProbe::Unresponsive => {
            // 套接字还在，但读不出 version。先停掉旧进程，再装上 App 里的映像并拉起。
            restart_daemon();
            install_managed_daemon_from(&bundled)?;
            let path = managed_daemon_path();
            spawn_managed_daemon_and_connect(path.clone()).map_err(|error| {
                std::io::Error::other(format!("旧守护无响应，重启后仍未就绪：{error}"))
            })?;
            Ok(path)
        }
        DaemonProbe::Running {
            exe_mtime: _,
            exe_path,
            ..
        } => {
            let on_managed = exe_path
                .as_ref()
                .map(|p| exe_is_managed(std::path::Path::new(p)))
                .unwrap_or(false);
            let inside_app = exe_path
                .as_ref()
                .map(|p| path_inside_app_bundle(std::path::Path::new(p)))
                .unwrap_or(!on_managed);
            let content_differs = !managed.is_file() || !same_daemon_binary(&managed, &bundled);
            if inside_app || !on_managed || content_differs {
                eprintln!(
                    "[workspace] 重启守护到 App 内映像（inside_app={inside_app} on_managed={on_managed} content_differs={content_differs} exe={exe_path:?}）"
                );
                match upgrade_daemon_exe(Some(&bundled)) {
                    UpgradeOutcome::Upgraded => Ok(managed_daemon_path()),
                    _ => Err(std::io::Error::other(
                        "未能停掉旧守护并换上与 App 一致的映像",
                    )),
                }
            } else {
                Ok(managed_daemon_path())
            }
        }
    }
}

/// 把 App 里的 smelt-pi 发布到 `~/.smelt/bin` 并拨好 `smelt-pi` 链接，再回收没人用的旧版本。
/// 和守护版本无关：只换了 Pi 时守护不用交接，之后新起的 Pi 就是新的。要读一遍约
/// 120MB 算指纹，每个 GUI 进程只做一次。
fn sync_bundled_pi(bundled_daemon: &std::path::Path) {
    static SYNCED: AtomicBool = AtomicBool::new(false);
    if SYNCED.load(Ordering::Relaxed) {
        return;
    }
    // macOS：守护在 `Contents/MacOS`，Pi 在同一个 `Contents` 下的 Resources 里。
    // Windows：四个 exe 在同一目录，Pi 就在 smeltd.exe 旁边。
    let src = {
        #[cfg(windows)]
        {
            bundled_daemon.parent().map(std::path::Path::to_path_buf)
        }
        #[cfg(not(windows))]
        {
            bundled_daemon
                .parent()
                .and_then(std::path::Path::parent)
                .map(|contents| contents.join(smelt_sys::pi_image::APP_BUNDLE_DIR))
        }
    };
    let Some(src) = src else {
        return;
    };
    let dir = managed_daemon_dir();
    match smelt_sys::pi_image::publish(&dir, &src) {
        Ok(_) => SYNCED.store(true, Ordering::Relaxed),
        Err(error) => eprintln!("[workspace] 发布 smelt-pi 失败：{error}"),
    }
    if let Err(error) = smelt_sys::pi_image::reap_unused(&dir) {
        eprintln!("[workspace] 回收旧 smelt-pi 失败：{error}");
    }
}

/// 守护已在跑但路径未知/在 App 内时，用已有 managed 文件尝试迁出。
fn migrate_running_daemon_off_app(managed: &std::path::Path) -> UpgradeOutcome {
    if !managed.is_file() {
        return UpgradeOutcome::Failed;
    }
    match probe_daemon_detail() {
        DaemonProbe::Running { exe_path, .. } => {
            let on_managed = exe_path
                .as_ref()
                .map(|p| exe_is_managed(std::path::Path::new(p)))
                .unwrap_or(false);
            let inside_app = exe_path
                .as_ref()
                .map(|p| path_inside_app_bundle(std::path::Path::new(p)))
                .unwrap_or(!on_managed);
            if on_managed && !inside_app {
                return UpgradeOutcome::Upgraded;
            }
            // 停掉正在跑的守护，换成 `~/.smelt/bin` 里的这份映像，再拉起同版本新进程。
            upgrade_daemon_exe(Some(managed))
        }
        DaemonProbe::NotRunning | DaemonProbe::Unresponsive => UpgradeOutcome::Failed,
    }
}

/// 进程内 connect 路径是否已 ensure 过（多 pane 只付一次代价）。
/// 冷启动完整 ensure 由 `schedule_session_restore` 串行完成，不与此竞态升级。
static CONNECT_MANAGED_ENSURED: AtomicBool = AtomicBool::new(false);

/// 连接守护。进程内**首次**连上时 ensure 一次 managed 路径；之后只 connect。
/// 连不上则拉起 `~/.smelt/bin/smeltd`（独立进程组）再重试。
///
/// **这里绝不删 sock 文件。**（僵尸 sock 由 smeltd bind 方清理，见 smeltd。）
pub(crate) fn connect_daemon() -> std::io::Result<RuntimeStream> {
    // 已有守护：进程内只 ensure 一次。
    if local_runtime().connect().is_ok() {
        if !CONNECT_MANAGED_ENSURED.load(Ordering::Relaxed) {
            let _ = ensure_managed_daemon_for_connect(
                &CONNECT_MANAGED_ENSURED,
                managed_daemon_path(),
                ensure_managed_daemon_current,
            );
        }
        if let Ok(s) = local_runtime().connect() {
            return Ok(s);
        }
        // handoff 后短暂窗口：下面走拉起/轮询
    }

    // 要拉起了：完整检查一遍，不用进程内缓存。缓存只说明「之前在跑的那个已经核对过」，
    // 它退出后启动名可能还指着旧映像（新版本只挂在 smeltd.pending），照缓存直接拉起会把
    // 旧版本又起一遍。没在跑时完整检查会把启动名拨到 App 里的版本。
    let daemon = match ensure_managed_daemon_current() {
        Ok(p) => p,
        Err(e) => {
            let exe = std::env::current_exe()?;
            let fallback = exe.with_file_name(bundled_daemon_file_name());
            if fallback.is_file() {
                eprintln!(
                    "[daemon] managed daemon sync failed ({e}); falling back to {}",
                    fallback.display()
                );
                fallback
            } else {
                return Err(std::io::Error::new(e.kind(), format!("smeltd 不可用：{e}")));
            }
        }
    };

    spawn_managed_daemon_and_connect(daemon)
}

/// 拉起 `daemon` 并等到套接字可连。已经在听就直接返回。
///
/// 不经过 `ensure_managed_daemon_current`：调用方可能正握着安装锁。
fn spawn_managed_daemon_and_connect(daemon: std::path::PathBuf) -> std::io::Result<RuntimeStream> {
    let path = sock_path();
    if let Ok(stream) = local_runtime().connect() {
        return Ok(stream);
    }

    let spawn_err: Option<String> = {
        use std::process::Stdio;
        let exe = std::env::current_exe().ok();
        let in_app_bundle = exe.as_ref().is_some_and(|e| path_inside_app_bundle(e));
        // launch_executable：Unix 是启动名那条符号链接，再 canonicalize 成真正的映像；
        // Windows 直接是 `smeltd-<指纹>.exe`。不要 exec 一个还会被改掉的名字。
        let launch = daemon
            .parent()
            .map(smelt_sys::daemon_image::launch_executable)
            .unwrap_or_else(|| daemon.clone());
        let spawn_at = std::fs::canonicalize(&launch).unwrap_or(launch);
        let mut cmd = std::process::Command::new(&spawn_at);
        smelt_paths::export_to(&mut cmd);
        smelt_sys::tty_color::clear_command(&mut cmd);
        cmd.stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        if let Some(bundle) = exe.as_ref().and_then(|path| path.parent()) {
            cmd.env("SMELT_BUNDLE_DIR", bundle);
        }
        if in_app_bundle {
            cmd.env("SMELT_MENUBAR", "1");
        }
        // 把 spawn 目标的指纹传给子进程：它启动时直接钉死，不用再哈希磁盘——
        // 关闭“spawn 与守护启动哈希之间磁盘被替换”的竞态窗口（与 handoff 经
        // SMELTD_DAEMON_FINGERPRINT 传指纹同一机制）。
        if let Ok(fingerprint) = smelt_sys::daemon_image::content_fingerprint(&spawn_at) {
            cmd.env("SMELTD_DAEMON_FINGERPRINT", fingerprint);
        }
        match smelt_sys::process::spawn_detached(&mut cmd) {
            Ok(mut child) => {
                eprintln!(
                    "[workspace] 已拉起 smeltd pid={} ({}) menubar={}",
                    child.id(),
                    spawn_at.display(),
                    in_app_bundle
                );
                // 守护是本进程的直接子进程。没人 wait，它退出后会留成僵尸，按 pid 看存活
                // 会一直以为它还在：停机要白等到强杀，强杀之后还以为没杀掉。
                let _ = thread::Builder::new()
                    .name("smeltd-reaper".into())
                    .spawn(move || {
                        let _ = child.wait();
                    });
                None
            }
            Err(e) => Some(format!("拉起 smeltd 失败（{}）：{e}", spawn_at.display())),
        }
    };

    let timeout = daemon_ready_timeout();
    let deadline = std::time::Instant::now() + timeout;
    while std::time::Instant::now() < deadline {
        thread::sleep(Duration::from_millis(100));
        if let Ok(s) = local_runtime().connect() {
            return Ok(s);
        }
    }
    let secs = timeout.as_secs();
    Err(std::io::Error::new(
        std::io::ErrorKind::TimedOut,
        match spawn_err {
            Some(why) => format!("smeltd 未就绪：{why}；{secs}s 内未监听 {}", path.display()),
            None => format!(
                "smeltd 未就绪（已拉起 {}，{secs}s 内未监听 {}）",
                daemon.display(),
                path.display()
            ),
        },
    ))
}

/// 探测正在跑的守护：连不上（守护没起，`connect_daemon` 会自己拉起磁盘上最新的）
/// 判 `NotRunning`；连上了但读不出合法的 "version" 响应——老到连这个探测本身都不
/// 认识——判 `Unresponsive`；否则带 mtime + 可选 exe 路径。
enum DaemonProbe {
    NotRunning,
    Unresponsive,
    Running {
        exe_mtime: u64,
        /// 守护自报的 current_exe；老守护无此字段则为 None。
        exe_path: Option<String>,
        /// 守护启动时钉死的进程指纹；老守护无此字段则为 None。
        daemon_fingerprint: Option<String>,
    },
}

fn probe_daemon() -> DaemonProbe {
    probe_daemon_detail()
}

/// 面向守护的短控制请求共用时限。它们全都应在后台运行，但超时仍必须存在：守护
/// 某个连接线程失活时，不能无限占住 executor worker，也不能让重试任务无限堆积。
const DAEMON_CONTROL_TIMEOUT: Duration = Duration::from_secs(5);

fn connect_daemon_control() -> std::io::Result<RuntimeStream> {
    local_runtime().connect_with_timeout(DAEMON_CONTROL_TIMEOUT)
}

fn probe_daemon_detail() -> DaemonProbe {
    let Ok(mut s) = connect_daemon_control() else {
        return DaemonProbe::NotRunning;
    };
    let parsed = (|| -> Option<(u64, Option<String>, Option<String>)> {
        writeln!(
            s,
            "{}",
            serde_json::json!({ "op": DaemonOperation::Version })
        )
        .ok()?;
        let mut resp = String::new();
        BufReader::new(s).read_line(&mut resp).ok()?;
        let v: serde_json::Value = serde_json::from_str(resp.trim()).ok()?;
        let mtime = v["exe_mtime"].as_u64()?;
        let exe = v["exe"].as_str().map(str::to_string);
        let fingerprint = v["daemon_fingerprint"].as_str().map(str::to_string);
        Some((mtime, exe, fingerprint))
    })();
    match parsed {
        Some((m, exe, fingerprint)) => DaemonProbe::Running {
            exe_mtime: m,
            exe_path: exe,
            daemon_fingerprint: fingerprint,
        },
        None => DaemonProbe::Unresponsive,
    }
}

/// 守护自报的运行信息，设置页展示用。
///
/// 字段全是 `Option`：`pid`/`started_at`/`session_count` 是后加的，老守护（还没换代的
/// 那个进程）只回 `version`/`exe_mtime`，读不到就显示不出来，不能因此判失败。
#[derive(Clone, Debug, Default)]
pub struct DaemonInfo {
    pub version: Option<String>,
    pub pid: Option<u32>,
    /// 守护进程启动时刻（unix 秒）。无缝升级 exec 后会重置，见 `smeltd::session_state`。
    pub started_at: Option<u64>,
    pub session_count: Option<u64>,
}

/// 查正在跑的守护的运行信息；守护没起 / 不响应 → None。
///
/// **阻塞 IO，只能在后台跑**（见 main.rs::refresh_daemon_status）。故意不走
/// `connect_daemon`：那个连不上会顺手拉起守护，而这里只是"看一眼现状"，
/// 不该有副作用——没起就是没起。
pub fn daemon_info() -> Option<DaemonInfo> {
    let mut s = connect_daemon_control().ok()?;
    writeln!(
        s,
        "{}",
        serde_json::json!({ "op": DaemonOperation::Version })
    )
    .ok()?;
    let mut resp = String::new();
    BufReader::new(s).read_line(&mut resp).ok()?;
    let v: serde_json::Value = serde_json::from_str(&resp).ok()?;
    Some(DaemonInfo {
        version: v["version"].as_str().map(str::to_string),
        pid: v["pid"].as_u64().map(|n| n as u32),
        started_at: v["started_at"].as_u64(),
        session_count: v["session_count"].as_u64(),
    })
}

/// 磁盘上「期望」的 smeltd mtime：优先 App/cargo 同目录分发物，其次 managed。
/// 用于判断正在跑的守护是否落后（装 DMG 后 App 更新了，但 ~/.smelt/bin 还旧）。
fn disk_smeltd_mtime() -> Option<u64> {
    bundled_daemon_path()
        .and_then(|p| file_mtime_secs(&p))
        .or_else(|| file_mtime_secs(&managed_daemon_path()))
}

/// 守护是否落后于磁盘上的 smeltd 二进制（重装/重编译后常见：旧守护不会自动重启，
/// 新代码要等手动重启守护才生效）。守护没起 → false（没什么可重启的，交给
/// `connect_daemon` 按需拉起最新的）；守护活着但连 "version" op 都不认识 → true
/// （老到必然过期）；查得到 mtime 但磁盘那份查不到 → false（避免误报打扰用户）。
pub fn daemon_outdated() -> bool {
    match probe_daemon() {
        DaemonProbe::NotRunning => false,
        DaemonProbe::Unresponsive => true,
        DaemonProbe::Running {
            exe_mtime,
            exe_path,
            daemon_fingerprint,
        } => daemon_outdated_from_probe(
            exe_mtime,
            exe_path.as_deref(),
            daemon_fingerprint.as_deref(),
        ),
    }
}

/// outdated 纯判定（可单测）：指纹主键 + 老守护回退。
///
/// 主键是指纹：守护自报启动时钉死的进程指纹，与磁盘期望二进制（bundled 优先，
/// 其次 managed）的指纹比。内容不同=旧，不受“cp 造成 mtime+1s”误判，也不被
/// StageDiskOnly 骗（磁盘新了、进程指纹还是老的——之前 `same_daemon_binary(
/// running_exe_path, bundled)` 比的是两个磁盘新文件，直接误判“不旧”，升级永不触发）。
fn daemon_outdated_from_probe(
    exe_mtime: u64,
    exe_path: Option<&str>,
    daemon_fingerprint: Option<&str>,
) -> bool {
    // 仍住在 .app 内 / 不在 managed = 必须迁（装 DMG 会死）
    let inside_app = exe_path
        .map(|p| path_inside_app_bundle(std::path::Path::new(p)))
        .unwrap_or(false);
    let on_managed = exe_path
        .map(|p| exe_is_managed(std::path::Path::new(p)))
        .unwrap_or(false);
    if inside_app || !on_managed {
        return true;
    }
    // 主键：钉死指纹 vs 磁盘期望。
    if let Some(pinned) = daemon_fingerprint {
        let expected = bundled_daemon_path().or_else(|| {
            let managed = managed_daemon_path();
            managed.is_file().then_some(managed)
        });
        return outdated_by_fingerprint(pinned, expected.as_deref());
    }
    // 老守护无指纹：退回旧 heuristic（exe 路径内容比对 + mtime）。
    // 注意这条在 StageDiskOnly 后会误判“不旧”，但老守护熬过一次升级就换成新
    // 守护了——一次性窗口，可接受。
    if let (Some(exe), Some(bundled)) = (exe_path, bundled_daemon_path()) {
        let running_exe = std::path::Path::new(exe);
        let path_was_renamed =
            !running_exe.is_file() && same_daemon_binary(&managed_daemon_path(), &bundled);
        if same_daemon_binary(running_exe, &bundled) || path_was_renamed {
            return false;
        }
    }
    disk_smeltd_mtime().is_some_and(|disk| disk > exe_mtime)
}

/// 指纹主键判定（纯函数，可单测）：内容不同=旧。期望文件缺失/读不出→保守不旧，
/// 避免误报打扰用户；下一轮（或 headless 自升级）再看。
fn outdated_by_fingerprint(pinned: &str, expected: Option<&std::path::Path>) -> bool {
    let Some(path) = expected else {
        return false;
    };
    match smelt_sys::daemon_image::content_fingerprint(path) {
        Ok(disk) => disk != pinned,
        Err(_) => false,
    }
}

/// 新守护是否已经用 version 握手接替了旧进程。pid / started_at 在交接后都会变。
fn is_successor_daemon(before: Option<&DaemonInfo>, now: Option<&DaemonInfo>) -> bool {
    let Some(now) = now else {
        return false;
    };
    let Some(before) = before else {
        return true;
    };
    match (before.pid, now.pid) {
        (Some(old), Some(new)) if old != new => return true,
        _ => {}
    }
    matches!(
        (before.started_at, now.started_at),
        (Some(old), Some(new)) if old != new
    )
}

/// 把守护换到另一份映像之后的结果。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum UpgradeOutcome {
    /// 新进程已经起来，或者本来没跑、直接拉起了目标映像。
    Upgraded,
    /// 映像没发布成功，或者新进程没有接替旧进程。
    Failed,
}

/// 单次 App 安装尝试的结果。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AppInstallOutcome {
    Installed,
    /// 另一条安装正占着安装锁；App 包尚未替换。
    WaitingForInstallLock,
}

/// 停掉本机守护并拉起受管目录里的目标映像。进行中的回合会中断，历史还在。
pub fn upgrade_daemon() -> UpgradeOutcome {
    upgrade_daemon_exe(pending_upgrade_exe().as_deref())
}

/// 发布 `new_exe`（或待装映像）到 `~/.smelt/bin`，拨好启动名，停掉旧进程再拉起。
///
/// 不向旧守护发 `upgrade`。旧进程可能还是会做 fd 交接的版本，桌面必须自己停掉它。
/// `new_exe` 为 `None` 时用 `smeltd.pending`，没有就用 App 里的那份。
pub fn upgrade_daemon_exe(new_exe: Option<&std::path::Path>) -> UpgradeOutcome {
    let image = match resolve_restart_image(new_exe) {
        Ok(image) => image,
        Err(error) => {
            eprintln!("[workspace] 准备守护映像失败：{error}");
            return UpgradeOutcome::Failed;
        }
    };
    if let Err(error) = smelt_sys::daemon_image::retarget_launch_after_commit(&image) {
        eprintln!("[workspace] 设置启动名失败：{error}");
        return UpgradeOutcome::Failed;
    }
    let before = daemon_info();
    let was_up = !matches!(probe_daemon(), DaemonProbe::NotRunning);
    if was_up {
        restart_daemon();
    }
    if local_runtime()
        .connect_with_timeout(Duration::from_millis(200))
        .is_err()
    {
        if let Err(error) = spawn_managed_daemon_and_connect(managed_daemon_path()) {
            eprintln!("[workspace] 拉起新守护失败：{error}");
            return UpgradeOutcome::Failed;
        }
    }
    let running = matches!(probe_daemon(), DaemonProbe::Running { .. });
    let replaced = !was_up || is_successor_daemon(before.as_ref(), daemon_info().as_ref());
    if running && replaced {
        reap_managed_daemon_images(&managed_daemon_dir());
        UpgradeOutcome::Upgraded
    } else {
        eprintln!("[workspace] 守护没有换成新映像（was_up={was_up}）");
        UpgradeOutcome::Failed
    }
}

/// 目标已经是受管目录里的不可变映像时直接用它，否则发布一份进去。
/// 启动名不能指向 `.app` 里面的文件。
fn resolve_restart_image(new_exe: Option<&std::path::Path>) -> std::io::Result<std::path::PathBuf> {
    let dir = managed_daemon_dir();
    std::fs::create_dir_all(&dir)?;
    let src = match new_exe {
        Some(path) if path.is_file() => path.to_path_buf(),
        Some(path) => {
            return Err(std::io::Error::new(
                std::io::ErrorKind::NotFound,
                format!("映像不存在：{}", path.display()),
            ));
        }
        None => pending_upgrade_exe()
            .filter(|path| path.is_file())
            .or_else(bundled_daemon_path)
            .ok_or_else(|| {
                std::io::Error::new(std::io::ErrorKind::NotFound, "没有可切换的 smeltd 映像")
            })?,
    };
    if image_already_in_managed_dir(&src, &dir) {
        return Ok(std::fs::canonicalize(&src).unwrap_or(src));
    }
    smelt_sys::daemon_image::publish_image(&dir, &src)
}

fn image_already_in_managed_dir(path: &std::path::Path, dir: &std::path::Path) -> bool {
    let Some(name) = path.file_name().and_then(|name| name.to_str()) else {
        return false;
    };
    if !smelt_sys::daemon_image::is_immutable_image_name(name) {
        return false;
    }
    let Ok(canon) = path.canonicalize() else {
        return false;
    };
    let Ok(dir) = dir.canonicalize() else {
        return false;
    };
    canon.parent() == Some(dir.as_path())
}

/// 候选 `.app` 安装的公共前半段：取出候选包里的守护程序，再交到 `~/.smelt/bin`。
#[cfg(unix)]
fn prepare_candidate_daemon(candidate_app: &std::path::Path) -> anyhow::Result<std::path::PathBuf> {
    let daemon = candidate_app.join("Contents/MacOS/smeltd");
    if !daemon.is_file() {
        anyhow::bail!("候选 App 缺少守护程序：{}", daemon.display());
    }
    // smelt-pi 和守护分开发布。安装会当场交接守护，交接上来的新守护起 Pi 时链接必须已经
    // 就位；发布不上就不换 App。
    let pi_bundle = candidate_app
        .join("Contents")
        .join(smelt_sys::pi_image::APP_BUNDLE_DIR);
    smelt_sys::pi_image::publish(&managed_daemon_dir(), &pi_bundle)
        .map_err(|error| anyhow::anyhow!("发布 smelt-pi 失败：{error}"))?;
    Ok(daemon)
}

/// 安装提交：没在跑就只落盘；无响应先杀掉再装；在跑则停掉旧进程并换上候选映像。
/// 安装锁被占用时由调用方返回 `WaitingForInstallLock`，这里不再因会话忙碌推迟。
#[cfg(unix)]
fn commit_candidate_daemon(
    candidate_smeltd: &std::path::Path,
) -> anyhow::Result<AppInstallOutcome> {
    let probe = probe_daemon_detail();
    let managed = managed_daemon_path();
    let mapped = managed.is_file() && path_is_live_executable(&managed);
    if may_replace_managed_daemon_image(&probe, mapped) {
        install_managed_daemon_from(candidate_smeltd)?;
        return Ok(AppInstallOutcome::Installed);
    }
    match probe {
        DaemonProbe::NotRunning => {
            install_managed_daemon_from(candidate_smeltd)?;
            Ok(AppInstallOutcome::Installed)
        }
        DaemonProbe::Unresponsive => {
            restart_daemon();
            install_managed_daemon_from(candidate_smeltd)?;
            spawn_managed_daemon_and_connect(managed_daemon_path())
                .map_err(|error| anyhow::anyhow!("旧守护无响应，重启后仍未就绪：{error}"))?;
            Ok(AppInstallOutcome::Installed)
        }
        // 守护住在哪都一样：停掉旧进程，换上候选映像，再拉起。
        DaemonProbe::Running { .. } => match upgrade_daemon_exe(Some(candidate_smeltd)) {
            UpgradeOutcome::Upgraded => Ok(AppInstallOutcome::Installed),
            UpgradeOutcome::Failed => anyhow::bail!("装包前未能停掉旧守护并换上新映像"),
        },
    }
}

#[cfg(unix)]
fn prepare_and_commit_candidate_app(
    candidate_app: &std::path::Path,
) -> anyhow::Result<AppInstallOutcome> {
    let candidate_smeltd = prepare_candidate_daemon(candidate_app)?;
    commit_candidate_daemon(&candidate_smeltd)
}

/// 本地 `make install` 入口：与在线更新同一套「插件映射 → 守护交接 → 换 App」。
#[cfg(unix)]
pub fn install_local_app_bundle(
    source: &std::path::Path,
    target: &std::path::Path,
) -> anyhow::Result<AppInstallOutcome> {
    let _gate = match MANAGED_DAEMON_GATE.try_lock() {
        Ok(gate) => gate,
        Err(TryLockError::WouldBlock) => return Ok(AppInstallOutcome::WaitingForInstallLock),
        Err(TryLockError::Poisoned(poisoned)) => poisoned.into_inner(),
    };
    let Some(_file_gate) = try_acquire_managed_daemon_file_lock()? else {
        return Ok(AppInstallOutcome::WaitingForInstallLock);
    };

    match prepare_and_commit_candidate_app(source)? {
        AppInstallOutcome::Installed => {}
        other => return Ok(other),
    }

    atomic_install_app_bundle(source, target)?;
    sync_managed_helpers_from_app(source)?;
    Ok(AppInstallOutcome::Installed)
}

/// `smelt --install-app <src.app> [dst.app]`：给 Makefile 用，避免再写一套 shell 交接。
#[cfg(target_os = "macos")]
pub fn maybe_run_install_app<I, S>(args: I) -> Option<i32>
where
    I: IntoIterator<Item = S>,
    S: AsRef<str>,
{
    let args: Vec<String> = args
        .into_iter()
        .map(|arg| arg.as_ref().to_string())
        .collect();
    if args.get(1).map(String::as_str) != Some("--install-app") {
        return None;
    }
    let Some(source) = args.get(2).map(std::path::PathBuf::from) else {
        eprintln!("用法：smelt --install-app <Smelt.app> [/Applications/Smelt.app]");
        return Some(2);
    };
    let target = args
        .get(3)
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| std::path::PathBuf::from("/Applications/Smelt.app"));
    match install_local_app_bundle(&source, &target) {
        Ok(AppInstallOutcome::Installed) => {
            reap_managed_daemon_images(&managed_daemon_dir());
            eprintln!(
                "✅ 已安装 {}。守护已换成同一版本；进行中的回合已中断，历史记录还在。",
                target.display()
            );
            Some(0)
        }
        Ok(AppInstallOutcome::WaitingForInstallLock) => {
            eprintln!("⏸ 另一条安装正在进行，未替换 App。等它结束后再 make install。");
            Some(75)
        }
        Err(error) => {
            eprintln!("✗ 安装失败：{error:#}");
            Some(1)
        }
    }
}

#[cfg(unix)]
fn atomic_install_app_bundle(
    source: &std::path::Path,
    target: &std::path::Path,
) -> anyhow::Result<()> {
    let script = std::env::current_dir()
        .map_err(anyhow::Error::from)?
        .join("scripts/install-mac-app-atomically.sh");
    if !script.is_file() {
        anyhow::bail!(
            "找不到 {}（请在仓库根目录执行 make install）",
            script.display()
        );
    }
    let status = std::process::Command::new(&script)
        .arg(source)
        .arg(target)
        .status()
        .map_err(anyhow::Error::from)?;
    if !status.success() {
        anyhow::bail!("原子安装 App 失败，退出码 {}", status.code().unwrap_or(-1));
    }
    Ok(())
}

#[cfg(unix)]
fn sync_managed_helpers_from_app(app: &std::path::Path) -> std::io::Result<()> {
    let macos = app.join("Contents/MacOS");
    let dest_dir = managed_daemon_dir();
    std::fs::create_dir_all(&dest_dir)?;
    for name in ["smelt-notify"] {
        let src = macos.join(name);
        if !src.is_file() {
            continue;
        }
        let staged = dest_dir.join(format!("{name}.next"));
        let dest = dest_dir.join(name);
        let _ = std::fs::remove_file(&staged);
        std::fs::copy(&src, &staged)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mut permissions = std::fs::metadata(&staged)?.permissions();
            permissions.set_mode(0o755);
            std::fs::set_permissions(&staged, permissions)?;
        }
        std::fs::rename(&staged, dest)?;
    }
    Ok(())
}

/// 让守护自己退出。**会杀掉它托管的所有 PTY 会话**（进程一死子进程 EOF/SIGHUP），
/// 调用前必须先让用户明确知情确认。退出后调 ensure_daemon_running() 拉起磁盘上
/// 最新的 smeltd 二进制。
///
/// **必须在后台线程调用**：内部有 sleep / 可能跑 lsof，禁止在 UI 线程直接跑。
///
/// "shutdown" op 本身也是新加的——老到连它都不认识的守护会照单全收地忽略这条消息，
/// 连接照旧开着，优雅关闭形同没发生。等一小段时间探测它是否真的死了，没死就按
/// 监听 socket 的进程直接 SIGKILL，这条路径不依赖守护认不认识任何协议。
pub fn restart_daemon() {
    let path = sock_path();
    // 硬重启后必须重新验证托管文件；此前的成功缓存不能跨 daemon 生命周期。
    CONNECT_MANAGED_ENSURED.store(false, Ordering::Relaxed);
    let stopped = smelt_client::daemon_stop::stop_local_daemon();
    // 老守护的 version 响应没有 pid，停不停得掉只能看 socket。Unix 用 lsof 按 sock 反查。
    // Windows 没有这条路：只删残留 sock，等下次拉起。
    #[cfg(unix)]
    if !stopped && local_runtime().connect().is_ok() {
        force_kill_socket_owner(&path);
    }
    #[cfg(not(unix))]
    let _ = stopped;
    // 残留 sock 文件会让下一次 connect 误判/卡住；尽量清掉。
    if local_runtime().connect().is_err() {
        let _ = std::fs::remove_file(&path);
    }
    CONNECT_MANAGED_ENSURED.store(false, Ordering::Relaxed);
}

/// 兜底：找到正监听着 `path` 的进程并 SIGKILL，再清掉残留 socket 文件。用于优雅
/// shutdown 对老守护不生效的情况——`lsof -t` 直接按 socket 文件反查 pid，不经过
/// 应用层协议，多老的守护都杀得掉。
///
/// `lsof` 在 macOS 上偶发长时间无响应。stdout 必须有人读，否则管道写满后它
/// 自己也不会退出。两秒还没结束就杀掉，半截输出不当成 pid。
#[cfg(unix)]
fn force_kill_socket_owner(path: &std::path::Path) {
    let child = match std::process::Command::new("lsof")
        .arg("-t")
        .arg(path)
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .spawn()
    {
        Ok(child) => child,
        Err(_) => {
            let _ = std::fs::remove_file(path);
            return;
        }
    };
    if let Ok(Some(output)) =
        smelt_sys::process_wait::wait_child_stdout(child, Duration::from_secs(2))
    {
        for pid in String::from_utf8_lossy(&output).split_whitespace() {
            let _ = std::process::Command::new("kill")
                .arg("-9")
                .arg(pid)
                .status();
        }
    }
    let _ = std::fs::remove_file(path);
}

/// 确保守护活着，没有就拉起来（复用 connect_daemon 的探测+拉起+轮询逻辑）。
/// 调用方（重启守护后想立刻刷新状态）负责扔到后台线程，避免卡 UI（最坏等 5s）。
pub fn ensure_daemon_running() {
    let _ = connect_daemon();
}

#[cfg(test)]
mod successor_daemon_tests {
    use super::*;
    use std::time::Instant;

    fn info(pid: Option<u32>, started_at: Option<u64>) -> DaemonInfo {
        DaemonInfo {
            pid,
            started_at,
            ..DaemonInfo::default()
        }
    }

    #[test]
    fn successor_is_a_new_pid_or_started_at() {
        let old = info(Some(10), Some(100));
        assert!(is_successor_daemon(
            Some(&old),
            Some(&info(Some(11), Some(100)))
        ));
        assert!(is_successor_daemon(
            Some(&old),
            Some(&info(Some(10), Some(200)))
        ));
        assert!(!is_successor_daemon(Some(&old), Some(&old)));
        assert!(!is_successor_daemon(Some(&old), None));
        assert!(is_successor_daemon(None, Some(&old)));
    }
}

#[cfg(all(test, unix))]
mod install_commit_tests {
    use super::*;

    #[test]
    fn only_a_down_and_unmapped_daemon_may_be_replaced() {
        assert!(may_replace_managed_daemon_image(
            &DaemonProbe::NotRunning,
            false
        ));
        assert!(!may_replace_managed_daemon_image(
            &DaemonProbe::NotRunning,
            true
        ));
        assert!(!may_replace_managed_daemon_image(
            &DaemonProbe::Unresponsive,
            false
        ));
        assert!(!may_replace_managed_daemon_image(
            &DaemonProbe::Unresponsive,
            true
        ));
    }

    #[test]
    fn mapped_executable_is_live_and_a_fresh_file_is_not() {
        let exe = std::env::current_exe().unwrap();
        assert!(
            path_is_live_executable(&exe),
            "正在跑的测试二进制必须被看成活映像"
        );
        let fresh = std::env::temp_dir().join(format!(
            "smelt-unmapped-{}-{}",
            std::process::id(),
            uuid::Uuid::new_v4()
        ));
        std::fs::write(&fresh, b"not-mapped").unwrap();
        assert!(
            !path_is_live_executable(&fresh),
            "没人映射的文件必须允许替换"
        );
        std::fs::remove_file(fresh).unwrap();
    }
}

#[cfg(all(test, unix))]
mod outdated_fingerprint_tests {
    use super::*;

    fn fixture(content: &[u8]) -> std::path::PathBuf {
        let path = std::env::temp_dir().join(format!(
            "smelt-outdated-fp-{}-{}.bin",
            std::process::id(),
            uuid::Uuid::new_v4().simple()
        ));
        std::fs::write(&path, content).unwrap();
        path
    }

    /// 核心回归：StageDiskOnly 后磁盘是新的、进程指纹还是老的，必须判旧。
    /// 之前 `same_daemon_binary(running_exe_path, bundled)` 比的是两个磁盘新文件，
    /// 误判“不旧”，升级永不触发。
    #[test]
    fn staged_disk_with_old_process_is_outdated() {
        let disk_new = fixture(b"new-binary-bytes");
        let disk_old = fixture(b"old-binary-bytes");
        let pinned_old = smelt_sys::daemon_image::content_fingerprint(&disk_old).unwrap();
        assert!(
            outdated_by_fingerprint(&pinned_old, Some(&disk_new)),
            "磁盘新了、进程还是老的，必须判旧"
        );
        std::fs::remove_file(disk_new).ok();
        std::fs::remove_file(disk_old).ok();
    }

    /// 内容一致（仅 cp 造成 mtime 变化）不判旧：handoff 会闪断终端，不能误触。
    #[test]
    fn same_content_is_not_outdated() {
        let disk = fixture(b"same-binary-bytes");
        let pinned = smelt_sys::daemon_image::content_fingerprint(&disk).unwrap();
        assert!(!outdated_by_fingerprint(&pinned, Some(&disk)));
        std::fs::remove_file(disk).ok();
    }

    /// 期望文件缺失/不可读→保守不旧，避免误报打扰用户。
    #[test]
    fn missing_expected_file_is_not_outdated() {
        assert!(!outdated_by_fingerprint("abc123", None));
        assert!(!outdated_by_fingerprint(
            "abc123",
            Some(std::path::Path::new("/nonexistent/smeltd-xyz"))
        ));
    }
}

#[cfg(all(test, unix))]
mod managed_daemon_ensure_tests {
    use super::*;
    use std::cell::Cell;
    use std::sync::Arc;
    use std::sync::atomic::AtomicUsize;

    fn managed_test_file(name: &str) -> std::path::PathBuf {
        let nonce = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        std::env::temp_dir().join(format!(
            "smelt-managed-{name}-{}-{nonce}",
            std::process::id()
        ))
    }

    #[test]
    fn explicit_ensure_then_connect_fallback_only_ensures_once() {
        let ensured = AtomicBool::new(false);
        let calls = Cell::new(0);
        let managed = managed_test_file("cached");
        std::fs::write(&managed, b"daemon").unwrap();

        let first = remember_managed_daemon_ensure(
            {
                calls.set(calls.get() + 1);
                Ok(managed.clone())
            },
            &ensured,
        );
        let fallback = ensure_managed_daemon_for_connect(&ensured, managed.clone(), || {
            calls.set(calls.get() + 1);
            Ok(managed.clone())
        });

        assert_eq!(first.unwrap(), managed);
        assert_eq!(fallback.unwrap(), managed);
        assert_eq!(calls.get(), 1);
        assert!(ensured.load(Ordering::Relaxed));
        let _ = std::fs::remove_file(managed);
    }

    #[test]
    fn cached_ensure_runs_again_when_managed_binary_disappears() {
        let ensured = AtomicBool::new(true);
        let calls = Cell::new(0);
        let managed = managed_test_file("missing");

        let result = ensure_managed_daemon_for_connect(&ensured, managed.clone(), || {
            calls.set(calls.get() + 1);
            std::fs::write(&managed, b"restored")?;
            Ok(managed.clone())
        });

        assert_eq!(result.unwrap(), managed);
        assert_eq!(calls.get(), 1, "文件消失后不能继续盲信 ensure 缓存");
        let _ = std::fs::remove_file(managed);
    }

    #[test]
    fn file_lock_serializes_managed_daemon_installers() {
        const INSTALLERS: usize = 8;
        let lock_path = managed_test_file("install-lock");
        let barrier = Arc::new(std::sync::Barrier::new(INSTALLERS));
        let active = Arc::new(AtomicUsize::new(0));
        let max_active = Arc::new(AtomicUsize::new(0));
        let mut installers = Vec::new();

        for _ in 0..INSTALLERS {
            let lock_path = lock_path.clone();
            let barrier = Arc::clone(&barrier);
            let active = Arc::clone(&active);
            let max_active = Arc::clone(&max_active);
            installers.push(thread::spawn(move || {
                barrier.wait();
                let _lock = acquire_file_lock(&lock_path).expect("安装锁获取失败");
                let now = active.fetch_add(1, Ordering::SeqCst) + 1;
                max_active.fetch_max(now, Ordering::SeqCst);
                thread::sleep(Duration::from_millis(5));
                active.fetch_sub(1, Ordering::SeqCst);
            }));
        }

        for installer in installers {
            installer.join().unwrap();
        }
        assert_eq!(
            max_active.load(Ordering::SeqCst),
            1,
            "跨执行上下文的 managed 安装必须完全串行"
        );
        let _ = std::fs::remove_file(lock_path);
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn install_app_cli_is_opt_in() {
        assert!(maybe_run_install_app(["smelt"]).is_none());
        assert!(maybe_run_install_app(["smelt", "--help"]).is_none());
        assert_eq!(smelt_cli::maybe_run(["smelt", "--help"]), Some(0));
        assert_eq!(maybe_run_install_app(["smelt", "--install-app"]), Some(2));
    }

    #[test]
    fn daemon_spawn_strips_host_color_suppression() {
        let mut cmd = std::process::Command::new("/bin/true");
        cmd.env("NO_COLOR", "1");
        cmd.env("FORCE_COLOR", "0");
        smelt_paths::export_to(&mut cmd);
        smelt_sys::tty_color::clear_command(&mut cmd);
        let present: Vec<String> = cmd
            .get_envs()
            .filter_map(|(key, value)| {
                value?;
                key.to_str().map(str::to_string)
            })
            .collect();
        for key in smelt_sys::tty_color::SUPPRESSION_VARS {
            assert!(
                !present.iter().any(|k| k == key),
                "拉起 smeltd 不得带上 {key}，实际: {present:?}"
            );
        }
    }
}

