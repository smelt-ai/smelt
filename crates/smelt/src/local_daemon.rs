//! 本机 smeltd 的生命周期（设计文档 D3）。
//!
//! 拉起、换版本和过期探测。换版本一律是停掉、装上、再拉起。
//! 终端网格、远程会话连接留在 `terminal`。

use rust_i18n::t;
use std::io::{BufRead, BufReader, Read, Write};
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};
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
/// **硬约束**：正在跑的 smeltd 必须是 `~/.smelt/bin/` 里的那份，绝不能是 `Smelt.app` 里的。
/// 否则用户用 Finder 拖 DMG 覆盖 App 时内核会干掉守护。
fn managed_daemon_dir() -> std::path::PathBuf {
    smelt_paths::smelt_home()
        .unwrap_or_else(|| std::env::temp_dir().join(".smelt"))
        .join("bin")
}

pub(crate) fn bundled_daemon_file_name() -> &'static str {
    smelt_sys::daemon_image::file_name()
}

pub fn managed_daemon_path() -> std::path::PathBuf {
    smelt_sys::daemon_image::installed_path(&managed_daemon_dir())
}

/// 路径是否落在某个 `.app` 包内（装 DMG 会被覆盖/删除）。
fn path_inside_app_bundle(p: &std::path::Path) -> bool {
    p.components()
        .any(|c| c.as_os_str().to_str().is_some_and(|s| s.ends_with(".app")))
}

/// 进程是否跑在 managed 目录里。从旧布局升上来的守护跑的是旁边的 `smeltd-<指纹>`，也算。
fn exe_is_managed(exe: &std::path::Path) -> bool {
    let dir = managed_daemon_dir();
    let dir = std::fs::canonicalize(&dir).unwrap_or(dir);
    let exe = std::fs::canonicalize(exe).unwrap_or_else(|_| exe.to_path_buf());
    exe.parent() == Some(dir.as_path())
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

/// 分发物 smeltd 所在的目录（App 包的 `Contents/MacOS`、cargo 的 profile 目录）。那里
/// 眼下还没有 smeltd 也照样给：盯着它，等它出现。
pub(crate) fn bundled_daemon_dir() -> Option<std::path::PathBuf> {
    distribution_exe()?
        .parent()
        .map(std::path::Path::to_path_buf)
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

/// 把 App 里的 smeltd 和 smelt-agent 装进 `~/.smelt/bin`。守护必须已经停了：Windows 上换不了
/// 正在执行的文件，Unix 上换掉的 smelt-agent 会和还在跑的 Pi 对不上。smelt-agent 要读一遍
/// 约 120MB 算指纹，所以只在要拉起守护时做。
fn install_bundled(bundled_daemon: &std::path::Path) -> std::io::Result<()> {
    let dir = managed_daemon_dir();
    if smelt_sys::daemon_image::install(&dir, bundled_daemon)? {
        eprintln!("[workspace] 已装上守护 {}", bundled_daemon.display());
    }
    if let Some(src) = bundled_agent_dir(bundled_daemon) {
        // smelt-agent 装不上不拦守护：起 Pi 时会报找不到。
        let installed =
            smelt_sys::agent_image::Bundle::read(&src).and_then(|bundle| bundle.install(&dir));
        if let Err(error) = installed {
            eprintln!("[workspace] 装 smelt-agent 失败：{error}");
        }
    }
    Ok(())
}

/// App 里那一整套 smelt-agent。macOS：守护在 `Contents/MacOS`，smelt-agent 在同一个
/// `Contents` 下的 Resources 里。Windows：几个 exe 在同一目录。开发构建里没有就是 `None`。
fn bundled_agent_dir(bundled_daemon: &std::path::Path) -> Option<std::path::PathBuf> {
    let src = if cfg!(windows) {
        bundled_daemon.parent()?.to_path_buf()
    } else {
        bundled_daemon
            .parent()?
            .parent()?
            .join(smelt_sys::agent_image::APP_BUNDLE_DIR)
    };
    smelt_sys::agent_image::bundled(&src).then_some(src)
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

fn acquire_managed_daemon_file_lock() -> std::io::Result<ManagedDaemonFileLock> {
    let dir = managed_daemon_dir();
    std::fs::create_dir_all(&dir)?;
    acquire_file_lock(&dir.join("smeltd.install.lock"))
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

/// 守护没在跑（或不响应）时装上 App 里的版本。在跑就不动：落后了由 `workspace_update` 在没有
/// 进行中的会话时重启，重启后经这里装上新版本。
fn ensure_managed_daemon_current_locked() -> std::io::Result<std::path::PathBuf> {
    let managed = managed_daemon_path();
    // 无分发物（开发机偶发）：只能用已有的 managed。
    let Some(bundled) = bundled_daemon_path() else {
        return if managed.is_file() {
            Ok(managed)
        } else {
            Err(std::io::Error::new(
                std::io::ErrorKind::NotFound,
                t!("daemon.not_found").into_owned(),
            ))
        };
    };
    match probe_daemon_detail() {
        DaemonProbe::NotRunning => install_bundled(&bundled)?,
        DaemonProbe::Unresponsive => {
            // 套接字还在，但读不出 version。停掉旧进程，装上 App 里的版本再拉起。
            restart_daemon();
            install_bundled(&bundled)?;
            spawn_managed_daemon_and_connect(managed.clone()).map_err(|error| {
                std::io::Error::other(t!("daemon.restart_not_ready", error = error).into_owned())
            })?;
        }
        DaemonProbe::Running { .. } => {}
    }
    Ok(managed)
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
        // handoff 后短暂窗口：下面走拉起、等就绪
    }

    // 要拉起了：完整检查一遍，不用进程内缓存。缓存只说明「之前在跑的那个已经核对过」，
    // 它退出后 App 可能已经换了版本，照缓存直接拉起会把旧版本又起一遍。
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
                return Err(std::io::Error::new(
                    e.kind(),
                    t!("daemon.unavailable", error = e).into_owned(),
                ));
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

    // 子进程绑好 socket 时从这条管道报一声；没就绪就退出时管道读到 EOF。
    let mut ready = None;
    let spawn_err: Option<String> = {
        use std::process::Stdio;
        let exe = std::env::current_exe().ok();
        let in_app_bundle = exe.as_ref().is_some_and(|e| path_inside_app_bundle(e));
        let spawn_at = &daemon;
        let mut cmd = std::process::Command::new(spawn_at);
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
        // 把 spawn 目标的指纹传给子进程：它启动时直接钉死，不用再哈希磁盘，也就没有
        // 「拉起与守护启动哈希之间磁盘被替换」的窗口。
        if let Ok(fingerprint) = smelt_sys::daemon_image::content_fingerprint(spawn_at) {
            cmd.env("SMELTD_DAEMON_FINGERPRINT", fingerprint);
        }
        let pipe = smelt_sys::ready_pipe::ReadyPipe::attach(&mut cmd);
        match smelt_sys::process::spawn_detached(&mut cmd) {
            Ok(mut child) => {
                ready = Some(pipe);
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
            Err(e) => {
                Some(t!("daemon.spawn_failed", path = spawn_at.display(), error = e).into_owned())
            }
        }
    };

    // 等子进程报就绪或退出，上限只兜底它卡住不动。读完都再连一次：报了就绪自然连得上；
    // 退出了也可能是别人先起好了守护，它看见就让开了。
    let timeout = daemon_ready_timeout();
    let readiness = match ready {
        Some(Ok(pipe)) => pipe.wait(timeout),
        Some(Err(error)) => Err(error),
        None => Ok(smelt_sys::ready_pipe::Readiness::Exited),
    };
    if let Ok(s) = local_runtime().connect() {
        return Ok(s);
    }
    let secs = timeout.as_secs();
    Err(match (spawn_err, readiness) {
        (Some(why), _) => std::io::Error::other(t!("daemon.not_ready", why = why).into_owned()),
        (None, Ok(smelt_sys::ready_pipe::Readiness::TimedOut)) => std::io::Error::new(
            std::io::ErrorKind::TimedOut,
            t!(
                "daemon.not_listening",
                daemon = daemon.display(),
                secs = secs,
                socket = path.display()
            )
            .into_owned(),
        ),
        (None, Ok(_)) => std::io::Error::other(
            t!(
                "daemon.exited",
                daemon = daemon.display(),
                socket = path.display()
            )
            .into_owned(),
        ),
        (None, Err(error)) => std::io::Error::other(
            t!(
                "daemon.no_ready_signal",
                daemon = daemon.display(),
                error = error
            )
            .into_owned(),
        ),
    })
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

/// 守护自报的身份。换代前后各问一次，比一比就知道新进程有没有接替旧的
/// （见 [`is_successor_daemon`]）。
///
/// 字段都是 `Option`：`pid`/`started_at` 是后加的，老守护只回 `version`/`exe_mtime`，
/// 读不到不能判失败。
#[derive(Clone, Debug, Default)]
pub struct DaemonInfo {
    pub pid: Option<u32>,
    /// 守护进程启动时刻（unix 秒）。无缝升级 exec 后会重置，见 `smeltd::session_state`。
    pub started_at: Option<u64>,
}

/// 查正在跑的守护的身份；守护没起 / 不响应 → None。
///
/// **阻塞 IO，只能在后台跑**。故意不走
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
        pid: v["pid"].as_u64().map(|n| n as u32),
        started_at: v["started_at"].as_u64(),
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
/// 其次 managed）的指纹比。内容不同=旧，不受“cp 造成 mtime+1s”误判；也不会因为
/// 进程跑的那个文件已经被换成新版本而误判不旧。
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
/// 避免误报打扰用户；下一轮再看。
fn outdated_by_fingerprint(pinned: &str, expected: Option<&std::path::Path>) -> bool {
    let Some(path) = expected else {
        return false;
    };
    match expected_fingerprint(path) {
        Ok(disk) => disk != pinned,
        Err(_) => false,
    }
}

/// 一份文件的身份：换了文件或者改了内容，这几项至少变一项。
#[derive(Clone, Debug, PartialEq, Eq)]
struct FileStamp {
    path: std::path::PathBuf,
    len: u64,
    modified: Option<std::time::SystemTime>,
    #[cfg(unix)]
    inode: (u64, u64),
}

fn file_stamp(path: &std::path::Path) -> std::io::Result<FileStamp> {
    let metadata = std::fs::metadata(path)?;
    Ok(FileStamp {
        path: path.to_path_buf(),
        len: metadata.len(),
        modified: metadata.modified().ok(),
        #[cfg(unix)]
        inode: {
            use std::os::unix::fs::MetadataExt as _;
            (metadata.dev(), metadata.ino())
        },
    })
}

/// 期望的那份 smeltd 的指纹，按文件身份缓存。落后检查跟着文件变化、窗口激活、本机运行时重连走，
/// 次数不定；smeltd 几十到几百 MB，文件没变就不重新哈希。
fn expected_fingerprint(path: &std::path::Path) -> std::io::Result<String> {
    static CACHE: Mutex<Option<(FileStamp, String)>> = Mutex::new(None);
    let stamp = file_stamp(path)?;
    let mut cache = CACHE.lock().unwrap_or_else(|e| e.into_inner());
    if let Some((cached, fingerprint)) = cache.as_ref()
        && *cached == stamp
    {
        return Ok(fingerprint.clone());
    }
    let fingerprint = smelt_sys::daemon_image::content_fingerprint(path)?;
    // 哈希期间文件又变了就不记：下次按新身份重算。
    if file_stamp(path).is_ok_and(|after| after == stamp) {
        *cache = Some((stamp, fingerprint.clone()));
    }
    Ok(fingerprint)
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

/// 把守护换成 App 里的版本之后的结果。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum UpgradeOutcome {
    /// 新进程已经起来，或者本来没跑、直接拉起了新版本。
    Upgraded,
    /// 没装上，或者新进程没有接替旧进程。
    Failed,
}

/// 停掉本机守护，装上 App 里的版本再拉起。进行中的回合会中断，历史还在。
///
/// 不向旧守护发 `upgrade`：旧进程可能还是会做 fd 交接的版本，桌面必须自己停掉它。
pub fn upgrade_daemon() -> UpgradeOutcome {
    let Some(bundled) = bundled_daemon_path() else {
        eprintln!("[workspace] 找不到 App 里的 smeltd");
        return UpgradeOutcome::Failed;
    };
    let _gate = MANAGED_DAEMON_GATE
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let _file_gate = match acquire_managed_daemon_file_lock() {
        Ok(lock) => lock,
        Err(error) => {
            eprintln!("[workspace] 拿不到守护安装锁：{error}");
            return UpgradeOutcome::Failed;
        }
    };
    let before = daemon_info();
    let was_up = !matches!(probe_daemon(), DaemonProbe::NotRunning);
    if was_up {
        restart_daemon();
    }
    if let Err(error) = install_bundled(&bundled) {
        eprintln!("[workspace] 装守护失败：{error}");
        return UpgradeOutcome::Failed;
    }
    if let Err(error) = spawn_managed_daemon_and_connect(managed_daemon_path()) {
        eprintln!("[workspace] 拉起新守护失败：{error}");
        return UpgradeOutcome::Failed;
    }
    let running = matches!(probe_daemon(), DaemonProbe::Running { .. });
    let replaced = !was_up || is_successor_daemon(before.as_ref(), daemon_info().as_ref());
    if running && replaced {
        CONNECT_MANAGED_ENSURED.store(true, Ordering::Relaxed);
        UpgradeOutcome::Upgraded
    } else {
        eprintln!("[workspace] 守护没有换成新版本（was_up={was_up}）");
        UpgradeOutcome::Failed
    }
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
    #[cfg(unix)]
    {
        if !stopped && local_runtime().connect().is_ok() {
            force_kill_socket_owner(&path);
        }
        // 残留 sock 文件会让下一次 connect 误判/卡住；尽量清掉。
        if local_runtime().connect().is_err() {
            let _ = std::fs::remove_file(&path);
        }
    }
    // Windows 没有 sock 文件，监听的是命名管道，守护一退出就没了；也没有按监听反查进程
    // 的办法，等下次拉起。
    #[cfg(not(unix))]
    let _ = (stopped, path);
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

/// 确保守护活着，没有就拉起来（复用 connect_daemon 的探测、拉起和等就绪）。
/// 调用方（重启守护后想立刻刷新状态）负责扔到后台线程，避免卡 UI（最坏等 5s）。
pub fn ensure_daemon_running() {
    let _ = connect_daemon();
}

#[cfg(test)]
mod successor_daemon_tests {
    use super::*;

    fn info(pid: Option<u32>, started_at: Option<u64>) -> DaemonInfo {
        DaemonInfo { pid, started_at }
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

    /// 核心回归：磁盘是新的、进程指纹还是老的，必须判旧。比两个磁盘文件会误判「不旧」。
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

    /// 内容一致（仅 cp 造成 mtime 变化）不判旧：重启会打断会话，不能误触。
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
