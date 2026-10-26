//! 本机 smeltd 的生命周期（设计文档 D3）。
//!
//! 拉起、升级、交接和过期探测。Windows 上升级是重启，不交接。
//! 终端网格、远程会话连接留在 `terminal`。

use std::io::{BufRead, BufReader, Read, Write};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, TryLockError};
use std::thread;
use std::time::{Duration, Instant};

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

/// 守护在跑时只发布新映像并挂上 `smeltd.pending`。启动名留到交接提交之后再拨。
fn stage_managed_daemon_update(src: &std::path::Path) -> std::io::Result<std::path::PathBuf> {
    let dir = managed_daemon_dir();
    std::fs::create_dir_all(&dir)?;
    stage_daemon_update_in(&dir, src)
}

/// `dir` 由调用方给。测试必须用临时目录，不能写真实的 `managed_daemon_path()`。
fn stage_daemon_update_in(
    dir: &std::path::Path,
    src: &std::path::Path,
) -> std::io::Result<std::path::PathBuf> {
    std::fs::create_dir_all(dir)?;
    let image = smelt_sys::daemon_image::publish_image(dir, src)?;
    smelt_sys::daemon_image::retarget_pending_symlink(dir, &image)?;
    reap_managed_daemon_images(dir);
    eprintln!(
        "[workspace] 已暂存守护更新 {} → {}",
        src.display(),
        image.display()
    );
    Ok(image)
}

fn pending_upgrade_exe() -> Option<std::path::PathBuf> {
    smelt_sys::daemon_image::pending_image(&managed_daemon_dir()).or_else(bundled_daemon_path)
}

fn handoff_daemon_to_managed(src: &std::path::Path) -> UpgradeOutcome {
    handoff_daemon_to_managed_once(src)
}

/// App 安装不能在持有安装锁时无限等 ACP 回合；交给 UI 显示等待状态并在安全边界后重试。
fn try_handoff_daemon_to_managed(src: &std::path::Path) -> UpgradeOutcome {
    handoff_daemon_to_managed_once(src)
}

/// 把正在跑的守护用一次 handoff 迁到 `~/.smelt/bin` 里的一份新映像（会话 PTY 保留）。
///
/// 无论现在跑的是不是 managed 路径，都先发布不可变映像再 exec 那份文件。
/// 启动名要等升级成功才拨；失败时链接保持原样，候选留在 `smeltd.pending`。
///
/// 一次尝试：Busy 原样返回，由 GUI 在回合结束后再发一次 upgrade。
fn handoff_daemon_to_managed_once(src: &std::path::Path) -> UpgradeOutcome {
    let managed = managed_daemon_path();
    let dir = managed_daemon_dir();
    if let Err(e) = std::fs::create_dir_all(&dir) {
        eprintln!("[workspace] 创建 managed 目录失败：{e}");
        return UpgradeOutcome::Failed;
    }

    let image = match smelt_sys::daemon_image::publish_image(&dir, src) {
        Ok(image) => image,
        Err(e) => {
            eprintln!("[workspace] 发布守护映像失败：{e}");
            reap_managed_daemon_images(&dir);
            return UpgradeOutcome::Failed;
        }
    };
    if let Err(e) = smelt_sys::daemon_image::retarget_pending_symlink(&dir, &image) {
        eprintln!("[workspace] 挂上 smeltd.pending 失败：{e}");
        reap_managed_daemon_images(&dir);
        return UpgradeOutcome::Failed;
    }

    let outcome = upgrade_daemon_exe(Some(&image));
    match outcome {
        UpgradeOutcome::Upgraded => {
            if let Err(e) = smelt_sys::daemon_image::retarget_launch_after_commit(&image) {
                eprintln!(
                    "[workspace] 完成 managed 安装失败：{e}（目标：{}）",
                    managed.display()
                );
            }
            reap_managed_daemon_images(&dir);
            eprintln!("[workspace] 守护已迁入 managed：{}", managed.display());
            UpgradeOutcome::Upgraded
        }
        UpgradeOutcome::Busy => {
            reap_managed_daemon_images(&dir);
            UpgradeOutcome::Busy
        }
        other => {
            // 失败时启动名不动。候选留在 pending，下次再 exec 同一份映像。
            reap_managed_daemon_images(&dir);
            eprintln!(
                "[workspace] handoff 到 managed 失败（{other:?}），候选：{}",
                image.display()
            );
            other
        }
    }
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
            // 连得上但无 version：进程可能还活着，只 stage 下一份，不覆盖正在跑的文件。
            let _ = stage_managed_daemon_update(&bundled);
            Ok(managed_daemon_path())
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
                .unwrap_or(!on_managed); // 老守护无 exe：若已在 managed 目录则别瞎迁
            // 路径不对（仍在 .app / 非 managed）→ 必须迁。
            // 二进制升级：仅当「跑的比 App 旧」且**文件内容实质不同**——
            // 禁止仅因 cp 造成 mtime+1s 就 handoff（会清空 Term → 对话像被重初始化）。
            let content_differs = !managed.is_file() || !same_daemon_binary(&managed, &bundled);
            let must_relocate = inside_app || !on_managed;
            if must_relocate {
                // **安全关键**：守护还住在 .app 里 / 不在 managed 目录，随时可能被
                // Finder 拖 DMG 覆盖 App 时误杀，必须立即迁出——这条不能等空闲。
                eprintln!(
                    "[workspace] 迁移守护 → managed（inside_app={inside_app} on_managed={on_managed} exe={exe_path:?}）"
                );
                let outcome = handoff_daemon_to_managed(&bundled);
                if !matches!(outcome, UpgradeOutcome::Upgraded) {
                    let _ = stage_managed_daemon_update(&bundled);
                }
            } else if content_differs {
                // 守护已在 managed（含版本旧）：只发布新映像并挂 pending，**不拨
                // 启动名，也不 exec**。连接路径偷偷换代正是「用着用着终端全卡」的
                // 根源。真正的换代交给 GUI 空闲升级或用户手动点升级。
                let _ = stage_managed_daemon_update(&bundled);
            }
            Ok(managed_daemon_path())
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
            handoff_daemon_to_managed(managed)
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
    let path = sock_path();

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

    // 二次确认：若 sock 已因 handoff 起来，直接连
    if let Ok(s) = local_runtime().connect() {
        return Ok(s);
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
            Ok(child) => {
                eprintln!(
                    "[workspace] 已拉起 smeltd pid={} ({}) menubar={}",
                    child.id(),
                    spawn_at.display(),
                    in_app_bundle
                );
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

/// 旧守护写完升级回执就会退出，并关掉这条连接。
/// 读到 EOF、对端重置，都是它已经走了。到 `timeout` 还没关，返回 false。
fn read_until_peer_closes(reader: &mut BufReader<RuntimeStream>, timeout: Duration) -> bool {
    let _ = reader.get_ref().set_read_timeout(Some(timeout));
    let mut line = String::new();
    match reader.read_line(&mut line) {
        Ok(_) => true,
        Err(error)
            if error.kind() == std::io::ErrorKind::TimedOut
                || error.kind() == std::io::ErrorKind::WouldBlock =>
        {
            false
        }
        Err(_) => true,
    }
}

/// 无缝升级的结果。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum UpgradeOutcome {
    /// 交接完成，会话全部保留；或守护本来没跑、直接拉起了最新版。
    Upgraded,
    /// 正在跑的守护太旧，不认识 "upgrade" op（静默断连），只能走硬重启。
    Unsupported,
    /// ACP 仍有未完成 RPC；守护未执行 exec，现有连接完全未受影响。
    Busy,
    /// 守护接了单但升级没生效（exec 失败等），版本还是旧的。
    Failed,
}

/// 单次 App 安装尝试的结果。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AppInstallOutcome {
    Installed,
    /// 当前 ACP 回合或另一条 handoff 正在占用安全边界；App 包尚未替换。
    WaitingForSafeHandoff,
}

/// 无缝升级守护：发 "upgrade" op，守护 exec 磁盘上的新二进制、PTY fd 原地交接，
/// **所有会话不中断**（协议与流程见 smeltd 升级设计）。调用方在成功后应对每个
/// 终端调 reconnect()——会话 id 都还在，走的是正常 reattach + 重放恢复。
///
/// `read_line` 前设了读超时：守护万一卡住（比如某个 out 锁被冻结客户端占住），不能
/// 让这次调用永久挂起——上层 `daemon_upgrading` 标志会跟着卡死，整个功能失效。
pub fn upgrade_daemon() -> UpgradeOutcome {
    upgrade_daemon_exe(pending_upgrade_exe().as_deref())
}

/// 无缝升级守护。`new_exe` 为 `Some` 时让守护 **exec 指定路径**（装 DMG：先 exec
/// 暂存包里的 smeltd，会话不丢，再替换 .app）；`None` 则 exec `current_exe`。
pub fn upgrade_daemon_exe(new_exe: Option<&std::path::Path>) -> UpgradeOutcome {
    let predecessor = daemon_info();
    let Ok(mut s) = local_runtime().connect() else {
        // 守护没跑：拉起磁盘上最新的等于升级完成，但要探测确认它真的起来了再报
        // 成功——ensure_daemon_running 的失败是静默的，不确认就报 Upgraded 会让
        // UI 显示"已升级"而守护其实没起来。
        ensure_daemon_running();
        return if matches!(probe_daemon(), DaemonProbe::Running { .. }) {
            UpgradeOutcome::Upgraded
        } else {
            UpgradeOutcome::Failed
        };
    };
    let msg = match new_exe {
        Some(p) => serde_json::json!({
            "op": DaemonOperation::Upgrade,
            "exe": p.to_string_lossy()
        }),
        None => serde_json::json!({ "op": DaemonOperation::Upgrade }),
    };
    if writeln!(s, "{msg}").is_err() {
        return UpgradeOutcome::Failed;
    }
    // 守护 upgrade 在回 ok 前要逐会话拿 out 锁；每个冻结 client 最多卡
    // CLIENT_WRITE_TIMEOUT(3s)。会话多时 5s 不够——会误报 Failed 而守护稍后仍 exec。
    let _ = s.set_read_timeout(Some(Duration::from_secs(30)));
    // 三种读结果分开判断，不能混为一谈：
    // - Ok(0)（EOF，没读到任何字节）＝老守护完全不认识这个 op，直接断连 → Unsupported；
    // - Err（超时/IO 错误）＝守护接了但迟迟不回（可能卡住），不代表版本问题 → Failed；
    // - Ok(n>0) 但解析不出 JSON，或解析出来 ok!=true（比如 current_exe/写交接文件
    //   失败的显式回执）＝守护是新版本、只是这次没成功，同样是 Failed，不能引导
    //   用户去做「版本过旧只能硬重启」这种更破坏性的操作。
    let mut resp = String::new();
    let mut reader = BufReader::new(s);
    match reader.read_line(&mut resp) {
        Ok(0) => return UpgradeOutcome::Unsupported,
        Ok(_) => {}
        Err(_) => return UpgradeOutcome::Failed,
    }
    let parsed = serde_json::from_str::<serde_json::Value>(resp.trim()).ok();
    if parsed
        .as_ref()
        .is_some_and(|v| v["busy"].as_bool() == Some(true))
    {
        return UpgradeOutcome::Busy;
    }
    let acked = parsed.is_some_and(|v| v["ok"].as_bool() == Some(true));
    if !acked {
        return UpgradeOutcome::Failed;
    }
    // 回执已经到手。接班进程在 COMMIT 前就拿着监听 fd，旧进程退出时关掉这条连接。
    // 等到那个 EOF 再问一次版本，pid 就是新的。中间不再隔几十毫秒连一次。
    if !read_until_peer_closes(&mut reader, Duration::from_secs(10)) {
        return UpgradeOutcome::Failed;
    }
    // Windows 没有 fd 交接：旧进程退出后，它拉起的接班进程才绑定监听。这里等接班进程
    // 起来，不自己拉。自己拉会和接班进程抢监听，抢到的是启动名指着的旧映像。
    #[cfg(windows)]
    {
        let deadline = std::time::Instant::now() + Duration::from_secs(30);
        while std::time::Instant::now() < deadline {
            if is_successor_daemon(predecessor.as_ref(), daemon_info().as_ref()) {
                return UpgradeOutcome::Upgraded;
            }
            std::thread::sleep(Duration::from_millis(200));
        }
        return UpgradeOutcome::Failed;
    }
    #[cfg(unix)]
    if is_successor_daemon(predecessor.as_ref(), daemon_info().as_ref()) {
        UpgradeOutcome::Upgraded
    } else {
        UpgradeOutcome::Failed
    }
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
    let pi_bundle = candidate_app.join("Contents").join(smelt_sys::pi_image::APP_BUNDLE_DIR);
    smelt_sys::pi_image::publish(&managed_daemon_dir(), &pi_bundle)
        .map_err(|error| anyhow::anyhow!("发布 smelt-pi 失败：{error}"))?;
    Ok(daemon)
}

/// 安装时对运行中守护的处置决策。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[cfg(unix)]
enum InstallCommitAction {
    /// 守护已在 managed 路径：只发布新映像并挂 pending，不拨启动名，不 handoff。
    /// 版本升级交给空闲无缝升级；安装永不因此阻塞。
    StageDiskOnly,
    /// 守护仍在 .app 里（或路径未知）：必须 handoff 迁出，否则换 App 会 SIGKILL 它。
    RelocateHandoff,
}

/// 安装处置决策（纯函数，可单测）：managed 不变量已成立就只 stage 磁盘。
///
/// 历史上这里无条件 handoff，把"换 App"和"换守护版本"绑成一次原子事务——
/// ACP 忙就 exit 75 半安装（映射写了、App 没换）。解耦后安装只保证 managed
/// 不变量；版本升级由空闲无缝升级接管（restore 后检查 + 60s watch + 回合结束
/// flush），skew 窗口的新 GUI + 老守护由启动路径的 op 容忍覆盖。
#[cfg(unix)]
fn install_commit_action_for_running_daemon(
    exe_path: Option<&std::path::Path>,
) -> InstallCommitAction {
    let managed = exe_path.map(exe_is_managed).unwrap_or(false);
    // 老守护无 exe 字段：无法证明已迁出，保守迁出（一次性迁移，此后永久跳过）。
    let inside_app = exe_path.map(path_inside_app_bundle).unwrap_or(true);
    if managed && !inside_app {
        InstallCommitAction::StageDiskOnly
    } else {
        InstallCommitAction::RelocateHandoff
    }
}

/// 安装提交：守护没在跑就只落盘 managed；在跑且已在 managed 路径同样只 stage
/// 磁盘（版本升级交给空闲无缝升级，安装不等 ACP）；只在守护仍住在 .app 里时
/// 才强制 handoff 迁出——此时 ACP 忙仍返回 WaitingForSafeHandoff、不替换 App。
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
        DaemonProbe::NotRunning | DaemonProbe::Unresponsive => {
            stage_managed_daemon_update(candidate_smeltd)?;
            Ok(AppInstallOutcome::Installed)
        }
        DaemonProbe::Running { exe_path, .. } => {
            let exe = exe_path.as_deref().map(std::path::Path::new);
            match install_commit_action_for_running_daemon(exe) {
                InstallCommitAction::StageDiskOnly => {
                    stage_managed_daemon_update(candidate_smeltd)?;
                    Ok(AppInstallOutcome::Installed)
                }
                InstallCommitAction::RelocateHandoff => {
                    match try_handoff_daemon_to_managed(candidate_smeltd) {
                        UpgradeOutcome::Upgraded => Ok(AppInstallOutcome::Installed),
                        UpgradeOutcome::Busy => Ok(AppInstallOutcome::WaitingForSafeHandoff),
                        UpgradeOutcome::Unsupported | UpgradeOutcome::Failed => {
                            anyhow::bail!(
                                "装包前 handoff→managed 未成功，已停止替换 App 以保护现有会话"
                            )
                        }
                    }
                }
            }
        }
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
        Err(TryLockError::WouldBlock) => return Ok(AppInstallOutcome::WaitingForSafeHandoff),
        Err(TryLockError::Poisoned(poisoned)) => poisoned.into_inner(),
    };
    let Some(_file_gate) = try_acquire_managed_daemon_file_lock()? else {
        return Ok(AppInstallOutcome::WaitingForSafeHandoff);
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
            let dir = managed_daemon_dir();
            if let Some(pending) = smelt_sys::daemon_image::pending_image(&dir) {
                match upgrade_daemon_exe(Some(&pending)) {
                    UpgradeOutcome::Upgraded => {
                        if let Err(error) =
                            smelt_sys::daemon_image::retarget_launch_after_commit(&pending)
                        {
                            eprintln!("[workspace] 升级后改写启动名失败：{error}");
                        }
                        eprintln!("✅ 守护进程已无缝升级到最新版本");
                    }
                    UpgradeOutcome::Busy => {
                        eprintln!("⏸ 守护进程忙碌中，新版本已暂存，将在空闲时自升级");
                    }
                    UpgradeOutcome::Unsupported | UpgradeOutcome::Failed => {
                        eprintln!("ℹ️ 守护进程无需或无法直接无缝升级（已暂存，下次启动生效）");
                    }
                }
            } else if let DaemonProbe::Running { exe_path, .. } = probe_daemon_detail()
                && let Ok(image) = std::fs::canonicalize(managed_daemon_path())
            {
                let already = exe_path
                    .as_deref()
                    .is_some_and(|exe| same_daemon_path(std::path::Path::new(exe), &image));
                if !already {
                    match upgrade_daemon_exe(Some(&image)) {
                        UpgradeOutcome::Upgraded => {
                            if let Err(error) =
                                smelt_sys::daemon_image::retarget_launch_after_commit(&image)
                            {
                                eprintln!("[workspace] 升级后改写启动名失败：{error}");
                            }
                            eprintln!("✅ 守护进程已无缝升级到最新版本");
                        }
                        UpgradeOutcome::Busy => {
                            eprintln!("⏸ 守护进程忙碌中，新版本已暂存，将在空闲时自升级");
                        }
                        UpgradeOutcome::Unsupported | UpgradeOutcome::Failed => {}
                    }
                }
            }
            reap_managed_daemon_images(&dir);
            eprintln!(
                "✅ 已安装 {}（守护交接与插件映射与在线更新相同）",
                target.display()
            );
            Some(0)
        }
        Ok(AppInstallOutcome::WaitingForSafeHandoff) => {
            eprintln!(
                "⏸ ACP 会话仍在运行，未替换 App（与在线更新一致）。回合结束后再 make install。"
            );
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
    let daemon_pid = daemon_pid_with_timeout();
    // 硬重启后必须重新验证托管文件；此前的成功缓存不能跨 daemon 生命周期。
    CONNECT_MANAGED_ENSURED.store(false, Ordering::Relaxed);
    // 无超时 read_line 会在守护卡死/不回包时永久挂起（设置页「重启守护」假死根因）。
    if let Ok(mut s) = local_runtime().connect_with_timeout(Duration::from_secs(2)) {
        let _ = writeln!(
            s,
            "{}",
            serde_json::json!({ "op": DaemonOperation::Shutdown })
        );
        let mut resp = String::new();
        let _ = BufReader::new(s).read_line(&mut resp);
    }
    // 等守护退出（最多 ~1s）；socket 仍能连上再强杀，**不要**提前 return 漏掉残留 sock。
    for _ in 0..10 {
        if local_runtime().connect().is_err() {
            break;
        }
        thread::sleep(Duration::from_millis(100));
    }
    // shutdown 会先回包再清理 sidecar；若清理卡住，另一个启动者可能已经删掉
    // socket 目录项，此时 lsof(path) 再也找不到旧守护。按重启前记录的 PID 兜底。
    if let Some(pid) = daemon_pid {
        if process_is_alive(pid) {
            let _ = smelt_sys::process::signal_tree(pid, smelt_sys::process::TreeSignal::Kill);
            let _ = smelt_sys::process::terminate_pid(pid);
            for _ in 0..10 {
                if !process_is_alive(pid) {
                    break;
                }
                thread::sleep(Duration::from_millis(50));
            }
        }
    } else if local_runtime().connect().is_ok() {
        // 老守护的 version 响应没有 pid。Unix 用 lsof 按 sock 反查。
        // Windows 没有这条路：没有 pid 就只删残留 sock，等下次拉起。
        #[cfg(unix)]
        {
            force_kill_socket_owner(&path);
            thread::sleep(Duration::from_millis(150));
        }
    }
    // 残留 sock 文件会让下一次 connect 误判/卡住；尽量清掉。
    if local_runtime().connect().is_err() {
        let _ = std::fs::remove_file(&path);
    }
    CONNECT_MANAGED_ENSURED.store(false, Ordering::Relaxed);
}

fn daemon_pid_with_timeout() -> Option<u32> {
    let mut s = local_runtime()
        .connect_with_timeout(Duration::from_millis(500))
        .ok()?;
    writeln!(
        s,
        "{}",
        serde_json::json!({ "op": DaemonOperation::Version })
    )
    .ok()?;
    let mut resp = String::new();
    BufReader::new(s).read_line(&mut resp).ok()?;
    let value: serde_json::Value = serde_json::from_str(resp.trim()).ok()?;
    value["pid"]
        .as_u64()
        .and_then(|pid| u32::try_from(pid).ok())
}

fn process_is_alive(pid: u32) -> bool {
    smelt_sys::process::pid_is_alive(pid)
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

    #[test]
    fn peer_close_is_observed_before_the_deadline() {
        let (ours, theirs) = RuntimeStream::pair().expect("socketpair");
        let mut reader = BufReader::new(ours);
        thread::spawn(move || {
            thread::sleep(Duration::from_millis(200));
            drop(theirs);
        });
        let started = Instant::now();
        assert!(read_until_peer_closes(&mut reader, Duration::from_secs(5)));
        assert!(
            started.elapsed() < Duration::from_secs(2),
            "对端关掉连接后应马上返回，实际 {:?}",
            started.elapsed()
        );
    }

    #[test]
    fn open_peer_waits_out_the_deadline() {
        let (ours, theirs) = RuntimeStream::pair().expect("socketpair");
        let mut reader = BufReader::new(ours);
        let started = Instant::now();
        assert!(!read_until_peer_closes(
            &mut reader,
            Duration::from_millis(200)
        ));
        let elapsed = started.elapsed();
        assert!(elapsed >= Duration::from_millis(150), "实际 {elapsed:?}");
        assert!(elapsed < Duration::from_secs(2), "实际 {elapsed:?}");
        drop(theirs);
    }
}

#[cfg(all(test, unix))]
mod install_commit_tests {
    use super::*;

    /// 核心回归：守护已在 managed 路径时安装只发布新映像，不 handoff——
    /// 此前无条件 handoff，ACP 忙就 exit 75 半安装（映射写了、App 没换）。
    #[test]
    fn managed_daemon_stages_disk_only() {
        let managed = managed_daemon_path();
        assert_eq!(
            install_commit_action_for_running_daemon(Some(&managed)),
            InstallCommitAction::StageDiskOnly
        );
        // 不可变映像仍在 managed 目录内：同样不得在安装时 handoff。
        let staged = managed_daemon_dir()
            .join("smeltd-0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef");
        assert_eq!(
            install_commit_action_for_running_daemon(Some(&staged)),
            InstallCommitAction::StageDiskOnly
        );
    }

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

    /// 守护仍住在 .app 里：必须 handoff 迁出，否则换 App 会被 SIGKILL。
    #[test]
    fn app_resident_daemon_must_relocate() {
        let in_app = std::path::PathBuf::from("/Applications/Smelt.app/Contents/MacOS/smeltd");
        assert_eq!(
            install_commit_action_for_running_daemon(Some(&in_app)),
            InstallCommitAction::RelocateHandoff
        );
        // 非 managed 的其它路径同样迁出。
        let elsewhere = std::path::PathBuf::from("/tmp/smeltd");
        assert_eq!(
            install_commit_action_for_running_daemon(Some(&elsewhere)),
            InstallCommitAction::RelocateHandoff
        );
    }

    /// 老守护无 exe 字段：无法证明已迁出，保守迁出（一次性，此后永久跳过）。
    #[test]
    fn unknown_exe_relocates_conservatively() {
        assert_eq!(
            install_commit_action_for_running_daemon(None),
            InstallCommitAction::RelocateHandoff
        );
    }

    #[test]
    fn staging_an_update_does_not_replace_the_live_launch_file() {
        use std::os::unix::fs::MetadataExt;
        let dir = std::env::temp_dir().join(format!(
            "smelt-stage-{}-{}",
            std::process::id(),
            uuid::Uuid::new_v4().simple()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let launch = dir.join("smeltd");
        std::fs::write(&launch, b"running-image").unwrap();
        let launch_inode = std::fs::metadata(&launch).unwrap().ino();
        let src = dir.join("src");
        std::fs::write(&src, b"pending-image").unwrap();

        let dest = stage_daemon_update_in(&dir, &src).unwrap();
        assert_eq!(std::fs::read(&launch).unwrap(), b"running-image");
        assert_eq!(std::fs::metadata(&launch).unwrap().ino(), launch_inode);
        assert_eq!(std::fs::read(&dest).unwrap(), b"pending-image");
        assert_eq!(
            smelt_sys::daemon_image::pending_image(&dir).as_deref(),
            Some(std::fs::canonicalize(&dest).unwrap().as_path()),
            "空闲升级必须 exec 新映像，不能再 exec 正在跑的旧文件"
        );

        let image_inode = std::fs::metadata(&dest).unwrap().ino();
        let again = stage_daemon_update_in(&dir, &src).unwrap();
        assert_eq!(again, dest);
        assert_eq!(std::fs::metadata(&again).unwrap().ino(), image_inode);
        std::fs::remove_dir_all(dir).unwrap();
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

