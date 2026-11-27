//! macOS 的更新：静默下载新版 zip，校验签名身份和版本号后放在缓存目录里等着装；装的时候
//! 拉起一次性的安装程序（`smelt-installer --install`），等 App 退出后把 `.app` 原子换成新
//! 版本，需要时再打开。`make install` 装本地构建的包也走同一条换包的路（[`replace_app`]）。
//!
//! 不记任何状态：
//! - 装的是哪版看程序自己编进去的版本号（[`installed_version`]），不另外记。
//! - 下好的包只在这次运行里等着装，重启后重新检查、重新下载。缓存目录随时可以整个删掉。
//! - 换包用 `renamex_np` 一步换完：任何时刻被打断，原位置上都是一个完整的 App（旧的或新的），
//!   交换位置上的另一份都是可以删的东西，下次启动删掉（[`clean_up_after_update`]）。
//!
//! 哪一步失败都只算这一次：旧 App 原样留着，下次检查照常进行。

use super::*;

use std::fs::{File, OpenOptions};
use std::io::Write;
use std::os::fd::AsRawFd;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

const BUNDLE_ID: &str = "com.zzfn.smelt";
const APP_NAME: &str = "Smelt";
/// 自更新用的 App 压缩包，`release.yml` 按架构各传一份。arm64 沿用不带架构的老名字，
/// 已装好的旧版只认这个名字。
#[cfg(target_arch = "aarch64")]
const UPDATE_ASSET: &str = "Smelt.app.zip";
#[cfg(target_arch = "x86_64")]
const UPDATE_ASSET: &str = "Smelt-x86_64.app.zip";
/// GUI 和安装程序互相等的两把锁放在 `~/.smelt`。
const LOCK_DIR: &str = ".smelt";
const APP_LAUNCH_GATE_FILE: &str = "app-launch-gate.lock";
const APP_RUNTIME_LOCK_FILE: &str = "app-runtime.lock";
const INSTALLER_NAME: &str = "smelt-installer";
/// 安装程序最多等 App 的窗口全部退出多久。
const INSTALLER_PARENT_WAIT_TIMEOUT: Duration = Duration::from_secs(120);

/// 查指定通道最新的发布。macOS 的更新包下下来再校验签名，查的时候不用多确认什么。
pub async fn fetch_latest(channel: UpdateChannel) -> anyhow::Result<UpdateCandidate> {
    fetch_candidate(channel).await
}

/// Release 里的自更新包。
pub(super) fn update_asset(release: &GithubRelease) -> Option<&GithubAsset> {
    release.asset(UPDATE_ASSET)
}

/// 下好、校验过的新版 App，在缓存目录的 `<随机 id>/Smelt.app`。只在这次运行里等着装。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StagedUpdate {
    pub version: String,
    pub url: String,
    app: PathBuf,
}

/// 已经拉起的安装程序。它的进度和错误写在 app.log。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct InstallerTicket {
    pid: u32,
}

impl std::fmt::Display for InstallerTicket {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "pid={}", self.pid)
    }
}

/// GUI 整个生命周期持共享锁；installer 提交交换前持独占锁。新 GUI 在独占锁期间
/// fail-fast，已启动 GUI 则让 installer 等待，因此检查与 rename 之间没有 TOCTOU。
pub struct AppRuntimeLease {
    _runtime_file: File,
    /// installer 持有独占 launch gate 直到交换完成；GUI 只在获取 runtime lease
    /// 期间短暂持共享 gate，因此等待中的 installer 不会被新 reader 饿死。
    _launch_gate_file: Option<File>,
}

fn open_app_lock_at(dir: &Path, name: &str) -> anyhow::Result<File> {
    std::fs::create_dir_all(dir)?;
    OpenOptions::new()
        .create(true)
        .read(true)
        .write(true)
        .truncate(false)
        .open(dir.join(name))
        .with_context(|| format!("couldn't open the app lock: {name}"))
}

fn try_flock(file: &File, operation: libc::c_int, context: &str) -> anyhow::Result<bool> {
    if unsafe { libc::flock(file.as_raw_fd(), operation | libc::LOCK_NB) } == 0 {
        return Ok(true);
    }
    let error = std::io::Error::last_os_error();
    if error.kind() == std::io::ErrorKind::WouldBlock {
        return Ok(false);
    }
    Err(error).with_context(|| context.to_string())
}

fn acquire_app_runtime_lease_at(dir: &Path) -> anyhow::Result<AppRuntimeLease> {
    let launch_gate = open_app_lock_at(dir, APP_LAUNCH_GATE_FILE)?;
    if !try_flock(
        &launch_gate,
        libc::LOCK_SH,
        "couldn't take the app launch gate",
    )? {
        anyhow::bail!("The installer is updating the app; reopen it in a moment");
    }
    let runtime = open_app_lock_at(dir, APP_RUNTIME_LOCK_FILE)?;
    if !try_flock(
        &runtime,
        libc::LOCK_SH,
        "couldn't take the app runtime shared lock",
    )? {
        anyhow::bail!("The installer is updating the app; reopen it in a moment");
    }
    // runtime lease 已建立，installer 即使随后拿到 launch gate，也必须等本 GUI 退出。
    drop(launch_gate);
    Ok(AppRuntimeLease {
        _runtime_file: runtime,
        _launch_gate_file: None,
    })
}

pub fn acquire_app_runtime_lease() -> anyhow::Result<AppRuntimeLease> {
    acquire_app_runtime_lease_at(&lock_dir()?)
}

enum ExclusiveLockWait {
    TimedOut,
    Failed(anyhow::Error),
}

/// 在拥有这个文件的线程上阻塞 `flock`。到点只丢掉接收端：锁由那条线程在
/// `flock` 返回后自己放开，别的线程不能关这个 fd。
fn wait_exclusive_flock(
    file: File,
    timeout: Duration,
    context: &str,
) -> Result<File, ExclusiveLockWait> {
    if timeout.is_zero() {
        return Err(ExclusiveLockWait::TimedOut);
    }
    let (tx, rx) = std::sync::mpsc::channel();
    let started = std::thread::Builder::new()
        .name("smelt-flock".to_string())
        .spawn(move || {
            let error = loop {
                if unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX) } == 0 {
                    break None;
                }
                let error = std::io::Error::last_os_error();
                if error.raw_os_error() == Some(libc::EINTR) {
                    continue;
                }
                break Some(error);
            };
            let _ = tx.send((file, error));
        });
    if started.is_err() {
        return Err(ExclusiveLockWait::Failed(anyhow::anyhow!(
            "{context}: couldn't start the wait thread"
        )));
    }
    match rx.recv_timeout(timeout) {
        Ok((file, None)) => Ok(file),
        Ok((_file, Some(error))) => Err(ExclusiveLockWait::Failed(
            anyhow::Error::from(error).context(context.to_string()),
        )),
        Err(std::sync::mpsc::RecvTimeoutError::Timeout) => Err(ExclusiveLockWait::TimedOut),
        Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => Err(ExclusiveLockWait::Failed(
            anyhow::anyhow!("{context}: the wait thread exited before taking the lock"),
        )),
    }
}

fn acquire_installer_runtime_lease_at(
    dir: &Path,
    timeout: Duration,
) -> anyhow::Result<AppRuntimeLease> {
    let started = Instant::now();
    let launch_gate = open_app_lock_at(dir, APP_LAUNCH_GATE_FILE)?;
    let launch_gate = match wait_exclusive_flock(
        launch_gate,
        timeout,
        "couldn't take the app launch gate exclusively",
    ) {
        Ok(file) => file,
        Err(ExclusiveLockWait::TimedOut) => {
            anyhow::bail!("timed out waiting for the app launch gate; the update wasn't committed");
        }
        Err(ExclusiveLockWait::Failed(error)) => return Err(error),
    };
    // 从这里起新 GUI 无法进入；只需等待已经持有 runtime lease 的 GUI 全部退出。
    let remaining = timeout.saturating_sub(started.elapsed());
    if remaining.is_zero() {
        anyhow::bail!(
            "timed out waiting for other Smelt windows to quit; the update wasn't committed"
        );
    }
    let runtime = open_app_lock_at(dir, APP_RUNTIME_LOCK_FILE)?;
    match wait_exclusive_flock(
        runtime,
        remaining,
        "couldn't take the app runtime lock exclusively",
    ) {
        Ok(runtime) => Ok(AppRuntimeLease {
            _runtime_file: runtime,
            _launch_gate_file: Some(launch_gate),
        }),
        Err(ExclusiveLockWait::TimedOut) => {
            anyhow::bail!(
                "timed out waiting for other Smelt windows to quit; the update wasn't committed"
            );
        }
        Err(ExclusiveLockWait::Failed(error)) => Err(error),
    }
}

fn acquire_installer_runtime_lease() -> anyhow::Result<AppRuntimeLease> {
    acquire_installer_runtime_lease_at(&lock_dir()?, INSTALLER_PARENT_WAIT_TIMEOUT)
}

fn lock_dir() -> anyhow::Result<PathBuf> {
    dirs::home_dir()
        .map(|home| home.join(LOCK_DIR))
        .ok_or_else(|| anyhow::anyhow!("couldn't find the home directory"))
}

/// 这一份能不能自更新：发版构建（有版本号可比），而且在 `.app` 里跑（有能整个换掉的
/// App）。开发构建、直接跑可执行文件的不检查、不下载。
pub fn can_self_update() -> bool {
    smelt_version::RELEASE.is_some() && current_app_bundle().is_ok()
}

/// 从当前可执行文件路径反推 `Smelt.app` 的位置：
/// `<App>.app/Contents/MacOS/smelt` 往上 3 层就是 `<App>.app`。
/// 非 `.app` 环境（比如 `cargo run`）直接报错，不做任何文件操作。
pub(crate) fn current_app_bundle() -> anyhow::Result<PathBuf> {
    app_bundle_for_exe(&smelt_paths::current_exe()?)
}

fn app_bundle_for_exe(exe: &Path) -> anyhow::Result<PathBuf> {
    let bundle = exe
        .parent()
        .and_then(Path::parent)
        .and_then(Path::parent)
        .ok_or_else(|| anyhow::anyhow!("the executable path is too shallow: {}", exe.display()))?
        .to_path_buf();
    if bundle.extension().and_then(|e| e.to_str()) != Some("app") {
        anyhow::bail!(
            "not running from a .app bundle ({}); skipping self-update",
            bundle.display()
        );
    }
    Ok(bundle)
}

/// 下载的包放在 `~/Library/Caches/com.zzfn.smelt/update`。
fn cache_root() -> anyhow::Result<PathBuf> {
    Ok(dirs::cache_dir()
        .ok_or_else(|| anyhow::anyhow!("couldn't find the system cache directory"))?
        .join(BUNDLE_ID)
        .join("update"))
}

fn remove_dir_if_exists(path: &Path) -> std::io::Result<()> {
    match std::fs::remove_dir_all(path) {
        Err(error) if error.kind() != std::io::ErrorKind::NotFound => Err(error),
        _ => Ok(()),
    }
}

/// 下载 ZIP → 解压 → 校验签名身份和版本号，放进缓存目录等着装。
///
/// ZIP 有上百 MB，一次性读完既吃内存又让界面整段时间没有进度，所以按 chunk 流式落盘，
/// 边下边经 `on_progress` 报字节数。缓存目录里原有的东西（上次没装的包、中断的下载）先
/// 清掉：同一时间只等着装一个包。
pub async fn download_and_stage(
    url: &str,
    version: &str,
    on_progress: impl Fn(DownloadProgress),
) -> anyhow::Result<StagedUpdate> {
    validate_download_url(url)?;
    let expected = semver::Version::parse(version.trim())
        .with_context(|| format!("the update version isn't a release number: {version:?}"))?;
    let trusted_app = current_app_bundle()?;
    let root = cache_root()?;
    if let Err(error) = remove_dir_if_exists(&root) {
        smelt_sys::app_log::warn(
            "updater",
            &format!("清不掉更新缓存 {}：{error}", root.display()),
        );
    }
    let dir = root.join(uuid::Uuid::new_v4().simple().to_string());
    std::fs::create_dir_all(&dir)
        .with_context(|| format!("couldn't create the download directory: {}", dir.display()))?;

    let result = async {
        let zip_path = dir.join(UPDATE_ASSET);
        download_package(url, &zip_path, &on_progress).await?;
        on_progress(DownloadProgress::Installing);

        let extract_root = dir.join("extract");
        extract_zip(&zip_path, &extract_root)?;
        let _ = std::fs::remove_file(&zip_path);
        let extracted = find_app_bundle(&extract_root)?;
        let app = dir.join(format!("{APP_NAME}.app"));
        std::fs::rename(&extracted, &app).with_context(|| {
            format!(
                "couldn't move the new .app: {} → {}",
                extracted.display(),
                app.display()
            )
        })?;
        let _ = std::fs::remove_dir_all(&extract_root);
        validate_app_bundle_against(&app, &trusted_app)?;
        ensure_bundle_version(&app, &expected)?;
        anyhow::Ok(app)
    }
    .await;

    match result {
        Ok(app) => Ok(StagedUpdate {
            version: version.trim().to_string(),
            url: url.trim().to_string(),
            app,
        }),
        Err(error) => {
            let _ = std::fs::remove_dir_all(&dir);
            Err(error)
        }
    }
}

async fn download_package(
    url: &str,
    to: &Path,
    on_progress: &impl Fn(DownloadProgress),
) -> anyhow::Result<()> {
    let mut resp = open_package_download(url, "ZIP").await?;
    let total = resp.content_length();
    on_progress(DownloadProgress::Bytes { received: 0, total });

    let mut file = File::create(to)?;
    let mut received = 0u64;
    let mut reported = 0u64;
    while let Some(chunk) = resp.chunk().await? {
        received += chunk.len() as u64;
        ensure_package_size(received)?;
        file.write_all(&chunk)?;
        if received - reported >= PROGRESS_REPORT_STEP || Some(received) == total {
            reported = received;
            on_progress(DownloadProgress::Bytes { received, total });
        }
    }
    file.flush()?;
    Ok(())
}

fn extract_zip(zip_path: &Path, extract_root: &Path) -> anyhow::Result<()> {
    std::fs::create_dir_all(extract_root)?;
    let out = Command::new("/usr/bin/ditto")
        .args(["-x", "-k"])
        .arg(zip_path)
        .arg(extract_root)
        .output()?;
    if !out.status.success() {
        anyhow::bail!("couldn't unzip: {}", String::from_utf8_lossy(&out.stderr));
    }
    Ok(())
}

fn find_app_bundle(root: &Path) -> anyhow::Result<PathBuf> {
    let mut pending = vec![root.to_path_buf()];
    while let Some(dir) = pending.pop() {
        for entry in std::fs::read_dir(&dir)? {
            let entry = entry?;
            let path = entry.path();
            if path.extension().and_then(|ext| ext.to_str()) == Some("app") && path.is_dir() {
                return Ok(path);
            }
            if entry.file_type()?.is_dir() {
                pending.push(path);
            }
        }
    }
    anyhow::bail!("no .app bundle in the ZIP")
}

/// 读 App 的 Info.plist 里的一项。
fn bundle_info(app: &Path, key: &str) -> anyhow::Result<String> {
    let output = Command::new("/usr/bin/plutil")
        .args(["-extract", key, "raw", "-o", "-"])
        .arg(app.join("Contents/Info.plist"))
        .output()
        .with_context(|| format!("couldn't read {key} of {}", app.display()))?;
    if !output.status.success() {
        anyhow::bail!(
            "couldn't read {key} of {}: {}",
            app.display(),
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }
    Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
}

/// 包里的版本号（`CFBundleShortVersionString`，打包时写的完整版本号）要和更新源说的是同一次
/// 发版。对不上就是更新源写错了：照装的话，装完一检查还是「有新版本」，每次都再装一遍。
fn ensure_bundle_version(app: &Path, expected: &semver::Version) -> anyhow::Result<()> {
    let text = bundle_info(app, "CFBundleShortVersionString")?;
    let actual = semver::Version::parse(&text).with_context(|| {
        format!("the update package's version isn't a release number: {text:?}")
    })?;
    if actual.cmp_precedence(expected).is_ne() {
        anyhow::bail!(
            "the update package is version {actual}, but the update source says {expected}"
        );
    }
    Ok(())
}

fn validate_app_bundle_against(app: &Path, trusted_app: &Path) -> anyhow::Result<()> {
    if !app.is_dir() || app.extension().and_then(|ext| ext.to_str()) != Some("app") {
        anyhow::bail!(
            "the update package isn't a valid .app directory: {}",
            app.display()
        );
    }
    for executable in [
        app.join("Contents/MacOS/smelt"),
        app.join("Contents/MacOS/smeltd"),
        app.join("Contents/MacOS").join(INSTALLER_NAME),
    ] {
        if !executable.is_file() {
            anyhow::bail!(
                "the update package is missing a required program: {}",
                executable.display()
            );
        }
        {
            use std::os::unix::fs::PermissionsExt as _;
            if std::fs::metadata(&executable)?.permissions().mode() & 0o111 == 0 {
                anyhow::bail!(
                    "an update package program isn't executable: {}",
                    executable.display()
                );
            }
        }
    }
    let info_plist = app.join("Contents/Info.plist");
    if !info_plist.is_file() {
        anyhow::bail!(
            "the update package has no Info.plist: {}",
            info_plist.display()
        );
    }

    {
        let bundle_id = std::process::Command::new("/usr/bin/plutil")
            .args(["-extract", "CFBundleIdentifier", "raw", "-o", "-"])
            .arg(&info_plist)
            .output()?;
        if !bundle_id.status.success()
            || String::from_utf8_lossy(&bundle_id.stdout).trim() != BUNDLE_ID
        {
            anyhow::bail!("the update package's Bundle ID doesn't match");
        }
        let signature = std::process::Command::new("/usr/bin/codesign")
            .args(["--verify", "--deep", "--strict"])
            .arg(app)
            .output()?;
        if !signature.status.success() {
            anyhow::bail!(
                "the update package's signature check failed: {}",
                String::from_utf8_lossy(&signature.stderr).trim()
            );
        }
        let trusted_requirement = designated_requirement(trusted_app)?;
        let identity = std::process::Command::new("/usr/bin/codesign")
            .args(["--verify", "--deep", "--strict", "-R"])
            .arg(format!("={trusted_requirement}"))
            .arg(app)
            .output()?;
        if !identity.status.success() {
            anyhow::bail!("the update package's signing identity doesn't match this Smelt");
        }
    }
    Ok(())
}

fn designated_requirement(app: &Path) -> anyhow::Result<String> {
    let output = std::process::Command::new("/usr/bin/codesign")
        .args(["-d", "-r-"])
        .arg(app)
        .output()
        .with_context(|| {
            format!(
                "couldn't read the app's designated requirement: {}",
                app.display()
            )
        })?;
    if !output.status.success() {
        anyhow::bail!(
            "couldn't read the app's designated requirement: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }
    [&output.stdout[..], &output.stderr[..]]
        .into_iter()
        .flat_map(|bytes| {
            String::from_utf8_lossy(bytes)
                .lines()
                .map(str::to_owned)
                .collect::<Vec<_>>()
        })
        .find_map(|line| {
            let line = line.trim();
            let line = line.strip_prefix("# ").unwrap_or(line);
            line.strip_prefix("designated =>").map(str::to_owned)
        })
        .map(|requirement| requirement.trim().to_owned())
        .filter(|requirement| !requirement.is_empty())
        .ok_or_else(|| anyhow::anyhow!("the app has no designated requirement: {}", app.display()))
}

/// 拉起安装程序，返回后调用方停守护、退出 App。安装程序等这个 App 的窗口全部退出后换包，
/// `relaunch` 时换完（或没换成）再把 App 打开。
///
/// 安装程序要在 App 外面跑：换包前得确认没有进程还在用旧 App（`lsof`），它自己要是从 App
/// 里跑，就永远确认不了。所以先拷一份放到下好的包旁边，装完和包一起删掉。
///
/// 同时告诉它这个 App 是哪个版本：等它真去换时，App 要是已经被别的途径换掉了（手动拖装了
/// 别的版本、`make install`），就不能拿这次下的包盖上去。
pub fn launch_installer(update: &StagedUpdate, relaunch: bool) -> anyhow::Result<InstallerTicket> {
    let target = current_app_bundle()?;
    if !update.app.is_dir() {
        anyhow::bail!("the downloaded update is gone: {}", update.app.display());
    }
    let helper = update.app.with_file_name(INSTALLER_NAME);
    copy_installer_helper(&target.join("Contents/MacOS").join(INSTALLER_NAME), &helper)?;
    let mut command = Command::new(&helper);
    command
        .arg("--install")
        .arg(&update.app)
        .arg("--target")
        .arg(&target)
        .arg("--replacing")
        .arg(smelt_version::VERSION);
    if relaunch {
        command.arg("--relaunch");
    }
    let child = command
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .context("couldn't launch the installer")?;
    Ok(InstallerTicket { pid: child.id() })
}

fn copy_installer_helper(source: &Path, destination: &Path) -> anyhow::Result<()> {
    use std::os::unix::fs::PermissionsExt as _;

    let metadata = std::fs::symlink_metadata(source)
        .with_context(|| format!("the install helper doesn't exist: {}", source.display()))?;
    if metadata.file_type().is_symlink() || !metadata.is_file() {
        anyhow::bail!(
            "the install helper must be a regular file: {}",
            source.display()
        );
    }
    match std::fs::remove_file(destination) {
        Err(error) if error.kind() != std::io::ErrorKind::NotFound => {
            return Err(error).with_context(|| {
                format!(
                    "couldn't replace the install helper: {}",
                    destination.display()
                )
            });
        }
        _ => {}
    }
    std::fs::copy(source, destination).with_context(|| {
        format!(
            "couldn't copy the install helper: {}",
            destination.display()
        )
    })?;
    std::fs::set_permissions(destination, std::fs::Permissions::from_mode(0o700))?;
    Ok(())
}

/// `smelt-installer --install` 的全过程：等 App 的窗口全部退出，确认 `target` 还是 `replacing`
/// 这个版本，把下好的包挪到 App 旁边再校验一遍签名身份，停守护、确认没人在用旧 App，原子
/// 交换，删掉换下来的旧 App。最后删掉缓存里的这一份，`relaunch` 时打开 App：换上了是新版，
/// 没换成是原来那个，不让人对着一个消失的窗口。
pub fn install_update(
    staged_app: &Path,
    target: &Path,
    replacing: &str,
    relaunch: bool,
) -> anyhow::Result<()> {
    let installed = install_staged_app(staged_app, target, replacing);
    // 装没装上，这一份都不要了：打开的 App 会重新检查。
    if let Some(entry) = staged_entry(staged_app)
        && let Err(error) = std::fs::remove_dir_all(&entry)
    {
        smelt_sys::app_log::warn(
            "installer",
            &format!("删不掉下好的包 {}，下次启动再清：{error}", entry.display()),
        );
    }
    let relaunched = if relaunch {
        relaunch_app(target)
    } else {
        Ok(())
    };
    match (installed, relaunched) {
        (Ok(()), relaunched) => relaunched,
        (Err(error), Ok(())) => Err(error),
        (Err(error), Err(relaunch_error)) => Err(error).context(format!(
            "reopening Smelt after the failed install also failed: {relaunch_error:#}"
        )),
    }
}

fn install_staged_app(staged_app: &Path, target: &Path, replacing: &str) -> anyhow::Result<()> {
    let _lease = acquire_installer_runtime_lease()?;
    let current = bundle_info(target, "CFBundleShortVersionString")?;
    if current != replacing {
        anyhow::bail!(
            "{} was replaced by {current} after the update was downloaded for {replacing}; skipped",
            target.display()
        );
    }
    let swap = swap_path(target, &new_swap_id())?;
    move_app_bundle(staged_app, &swap)?;
    let replaced = validate_app_bundle_against(&swap, target)
        .context("the update failed its final check")
        .and_then(|()| replace_app(target, &swap, quiesce_app_target));
    if replaced.is_err() {
        let _ = remove_swap(target, &swap);
    }
    replaced
}

/// 下好的包所在的那一份缓存（`<缓存>/<id>`）。不在缓存目录里的不认，不去删它。
fn staged_entry(staged_app: &Path) -> Option<PathBuf> {
    let entry = staged_app.parent()?;
    (entry.parent()? == cache_root().ok()?).then(|| entry.to_path_buf())
}

/// 把交换位置上的新 App 换到 `target`，调用方持有独占的 runtime lease。`target` 原来有 App
/// 就先 `quiesce`（停守护、确认没人在用），再原子交换，然后删掉换下来的旧 App（删不掉留给
/// 下次启动）；原来没有就直接放上去。返回错误时什么都没换，`swap` 上还是新 App，由调用方删。
fn replace_app(
    target: &Path,
    swap: &Path,
    quiesce: impl FnOnce(&Path) -> anyhow::Result<()>,
) -> anyhow::Result<()> {
    if std::fs::symlink_metadata(target).is_err() {
        return std::fs::rename(swap, target)
            .with_context(|| format!("couldn't install the app: {}", target.display()));
    }
    quiesce(target)?;
    swap_app_bundles(target, swap)?;
    if let Err(error) = remove_swap(target, swap) {
        smelt_sys::app_log::warn(
            "installer",
            &format!(
                "新 App 已换上，换下的旧 App {} 暂时删不掉，下次启动再清：{error:#}",
                swap.display()
            ),
        );
    }
    Ok(())
}

/// 启动时调用，尽力而为：删掉缓存里上次没装的包，和 App 旁边上次换包留下的交换位置（换下
/// 来的旧 App，或没换上的新 App）。还有进程在用的留着，下次再清。
pub fn clean_up_after_update() {
    if let Ok(root) = cache_root()
        && let Err(error) = remove_dir_if_exists(&root)
    {
        smelt_sys::app_log::warn(
            "updater",
            &format!("清不掉更新缓存 {}：{error}", root.display()),
        );
    }
    let Ok(target) = current_app_bundle() else {
        return;
    };
    for swap in leftover_swaps(&target) {
        let removed = bundle_use_state(&swap).and_then(|state| match state {
            BundleUseState::Unused => remove_swap(&target, &swap),
            BundleUseState::InUse => anyhow::bail!("still in use"),
        });
        if let Err(error) = removed {
            smelt_sys::app_log::warn(
                "updater",
                &format!("上次换包留下的 {} 暂时删不掉：{error:#}", swap.display()),
            );
        }
    }
}

/// 交换位置：`target` 旁边的 `.<名字>.smelt-update-<id>.app`。换包时新 App 先放在这儿，换完
/// 这儿是旧 App。和 `target` 在同一个目录，才能原子交换。
fn swap_path(target: &Path, id: &str) -> anyhow::Result<PathBuf> {
    if id.is_empty() || !id.chars().all(|c| c.is_ascii_alphanumeric()) {
        anyhow::bail!("invalid swap id: {id:?}");
    }
    let parent = target.parent().ok_or_else(|| {
        anyhow::anyhow!("the app path has no parent directory: {}", target.display())
    })?;
    Ok(parent.join(format!("{}{id}.app", swap_prefix(target))))
}

fn new_swap_id() -> String {
    uuid::Uuid::new_v4().simple().to_string()
}

fn swap_prefix(target: &Path) -> String {
    let stem = target
        .file_stem()
        .and_then(|value| value.to_str())
        .unwrap_or(APP_NAME);
    format!(".{stem}.smelt-update-")
}

fn is_swap_path(target: &Path, path: &Path) -> bool {
    let Some(name) = path.file_name().and_then(|value| value.to_str()) else {
        return false;
    };
    path.parent().is_some()
        && path.parent() == target.parent()
        && name.starts_with(&swap_prefix(target))
        && name.ends_with(".app")
}

fn leftover_swaps(target: &Path) -> Vec<PathBuf> {
    let Some(parent) = target.parent() else {
        return Vec::new();
    };
    let Ok(entries) = std::fs::read_dir(parent) else {
        return Vec::new();
    };
    entries
        .flatten()
        .map(|entry| entry.path())
        .filter(|path| is_swap_path(target, path))
        .collect()
}

/// 删交换位置。只删 [`swap_path`] 那样的路径；是文件或符号链接（只能是中断或篡改的残留）
/// 就删路径本身，不跟着链接走。
fn remove_swap(target: &Path, path: &Path) -> anyhow::Result<()> {
    if !is_swap_path(target, path) {
        anyhow::bail!(
            "refusing to remove a path that isn't a swap: {}",
            path.display()
        );
    }
    let metadata = match std::fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(error.into()),
    };
    if metadata.file_type().is_dir() {
        std::fs::remove_dir_all(path)?;
    } else {
        std::fs::remove_file(path)?;
    }
    Ok(())
}

/// 把下好的包挪到交换位置。同一个卷上直接改名；跨卷（App 放在别的盘上）就复制一份，复制
/// 失败把半截删掉。
fn move_app_bundle(source: &Path, destination: &Path) -> anyhow::Result<()> {
    match std::fs::rename(source, destination) {
        Ok(()) => Ok(()),
        Err(error) if error.raw_os_error() == Some(libc::EXDEV) => {
            let copied = copy_app_bundle(source, destination);
            if copied.is_err() {
                let _ = std::fs::remove_dir_all(destination);
            }
            copied
        }
        Err(error) => Err(error).with_context(|| {
            format!(
                "couldn't move the new app next to the installed one: {}",
                destination.display()
            )
        }),
    }
}

fn copy_app_bundle(source: &Path, destination: &Path) -> anyhow::Result<()> {
    let output = Command::new("/usr/bin/ditto")
        .arg(source)
        .arg(destination)
        .output()?;
    if !output.status.success() {
        anyhow::bail!(
            "couldn't copy the new app: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }
    Ok(())
}

fn swap_app_bundles(left: &Path, right: &Path) -> anyhow::Result<()> {
    use std::ffi::CString;
    use std::os::unix::ffi::OsStrExt;

    let left = CString::new(left.as_os_str().as_bytes())?;
    let right = CString::new(right.as_os_str().as_bytes())?;
    if unsafe { libc::renamex_np(left.as_ptr(), right.as_ptr(), libc::RENAME_SWAP) } != 0 {
        return Err(std::io::Error::last_os_error())
            .context("couldn't atomically swap the old and new app");
    }
    Ok(())
}

fn relaunch_app(app_bundle: &Path) -> anyhow::Result<()> {
    // 不用 -n：用户已经抢先打开了的话只激活那一个，不再开第二个。
    let status = Command::new("/usr/bin/open")
        .arg(app_bundle)
        .status()
        .context("couldn't launch Smelt")?;
    if !status.success() {
        anyhow::bail!("couldn't launch Smelt: open exit {status}");
    }
    Ok(())
}

/// 本地构建的包（`make install`）的前半段：校验，复制到目标旁边的交换位置，返回交换 id，
/// 交给 [`commit_local_app`] 提交；交不出去时用 [`discard_local_app`] 删掉副本。先复制好，
/// GUI 和守护停下的时间就只剩一次交换。
///
/// 候选是开发者自己刚打的包：不和已装的版本比签名身份。
pub fn stage_local_app(source: &Path, target: &Path) -> anyhow::Result<String> {
    validate_local_app(source)?;
    if target.extension().and_then(|value| value.to_str()) != Some("app") {
        anyhow::bail!("the install target isn't a .app: {}", target.display());
    }
    let id = new_swap_id();
    let swap = swap_path(target, &id)?;
    if let Err(error) = copy_app_bundle(source, &swap) {
        let _ = remove_swap(target, &swap);
        return Err(error);
    }
    Ok(id)
}

/// 删掉还没换上去的新包副本。
pub fn discard_local_app(target: &Path, id: &str) -> anyhow::Result<()> {
    remove_swap(target, &swap_path(target, id)?)
}

/// 后半段，和在线更新同一条换包的路：让 GUI 退出并等它们全部退出，停守护、确认没人在用
/// 目标 App，原子交换，删掉旧包，再打开 `target`。守护和 smelt-agent 由新 GUI 启动时从 App
/// 里换上，这里不碰 `~/.smelt/bin`。
///
/// 守护停机会杀掉它名下终端和 ACP 的进程组，而 `make install` 常在 Smelt 自己的终端里
/// 跑：这一段必须放在脱离那个终端的新会话里（见 `smelt-installer`），也就看不到终端，
/// 进度和错误只写 app.log。
pub fn commit_local_app(target: &Path, id: &str) -> anyhow::Result<()> {
    let swap = swap_path(target, id)?;
    if let Err(error) = request_running_app_quit() {
        smelt_sys::app_log::warn(
            "installer",
            &format!("没能让 Smelt 自己退出，等用户手动退出：{error:#}"),
        );
    }
    let committed = (|| {
        let _lease = acquire_installer_runtime_lease()?;
        if !swap.is_dir() {
            anyhow::bail!(
                "the new package copy is gone (was Smelt opened meanwhile?); rerun make install"
            );
        }
        replace_app(target, &swap, quiesce_app_target)
    })();
    if committed.is_err() {
        let _ = remove_swap(target, &swap);
    }
    // 换上了打开新版；没换成也把刚才让它退出的旧版拉回来。
    let relaunched = relaunch_app(target);
    committed?;
    relaunched
}

fn validate_local_app(app: &Path) -> anyhow::Result<()> {
    if app.extension().and_then(|value| value.to_str()) != Some("app") || !app.is_dir() {
        anyhow::bail!("not a .app bundle: {}", app.display());
    }
    if !app.join("Contents/Info.plist").is_file() {
        anyhow::bail!("the app has no Contents/Info.plist: {}", app.display());
    }
    for name in ["smelt", "smeltd", "smelt-notify", INSTALLER_NAME] {
        if !app.join("Contents/MacOS").join(name).is_file() {
            anyhow::bail!("the app has no Contents/MacOS/{name}: {}", app.display());
        }
    }
    Ok(())
}

/// 和用户按 ⌘Q 不同，Apple Event 的 quit 不弹退出确认，GUI 照常落盘布局后退出。
fn request_running_app_quit() -> anyhow::Result<()> {
    let script = format!(
        "if application id \"{BUNDLE_ID}\" is running then tell application id \"{BUNDLE_ID}\" to quit"
    );
    let output = Command::new("/usr/bin/osascript")
        .arg("-e")
        .arg(script)
        .output()
        .context("couldn't run osascript")?;
    if !output.status.success() {
        anyhow::bail!("{}", String::from_utf8_lossy(&output.stderr).trim());
    }
    Ok(())
}

/// 换 App 前的最后一步，在线更新和 `make install` 共用：调用方持有独占的 runtime lease，
/// 懂锁的 GUI 都已退出、新的起不来。先停守护及其会话，再用 lsof 检查剩余占用。
fn quiesce_app_target(app_bundle: &Path) -> anyhow::Result<()> {
    quiesce_app_target_with(shutdown_local_daemon, || bundle_use_state(app_bundle))
}

fn quiesce_app_target_with(
    shutdown: impl FnOnce() -> bool,
    probe: impl FnOnce() -> anyhow::Result<BundleUseState>,
) -> anyhow::Result<()> {
    if !shutdown() {
        anyhow::bail!("the old daemon didn't exit; stopped replacing the app");
    }
    if probe()? != BundleUseState::Unused {
        anyhow::bail!("the target app is in use again; refusing to replace it");
    }
    Ok(())
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum BundleUseState {
    InUse,
    Unused,
}

/// `lsof` 对 `+D` 的约定：0=至少一个打开项，1 且完全无输出=没有匹配项。
/// 工具缺失、权限警告、信号退出和其它退出码都不能证明安全，必须 fail-closed。
fn classify_bundle_use_output(
    success: bool,
    code: Option<i32>,
    stdout: &[u8],
    stderr: &[u8],
) -> anyhow::Result<BundleUseState> {
    if success {
        return Ok(BundleUseState::InUse);
    }
    if code == Some(1) && stdout.is_empty() && stderr.is_empty() {
        return Ok(BundleUseState::Unused);
    }
    // 原样带上输出：没有它就查不到是哪个进程占着旧 App。
    anyhow::bail!(
        "can't confirm the old app is idle (lsof code={code:?} stdout={:?} stderr={:?})",
        lsof_excerpt(stdout),
        lsof_excerpt(stderr)
    )
}

fn lsof_excerpt(bytes: &[u8]) -> String {
    const LIMIT: usize = 1024;
    let text = String::from_utf8_lossy(&bytes[..bytes.len().min(LIMIT)]).into_owned();
    if bytes.len() > LIMIT {
        format!("{text}… ({} bytes total)", bytes.len())
    } else {
        text
    }
}

fn bundle_use_state(app: &Path) -> anyhow::Result<BundleUseState> {
    // p/c/n：进程号、命令名、文件路径，失败时日志里能看出是谁占着。
    let output = std::process::Command::new("/usr/sbin/lsof")
        .arg("-Fpcn")
        .arg("+D")
        .arg(app)
        .output()
        .with_context(|| {
            format!(
                "couldn't check whether the app is still in use: {}",
                app.display()
            )
        })?;
    classify_bundle_use_output(
        output.status.success(),
        output.status.code(),
        &output.stdout,
        &output.stderr,
    )
}

#[cfg(test)]
mod tests {
    use super::{
        BundleUseState, GithubRelease, acquire_app_runtime_lease_at,
        acquire_installer_runtime_lease_at, app_bundle_for_exe, classify_bundle_use_output,
        ensure_bundle_version, is_swap_path, leftover_swaps, quiesce_app_target_with, remove_swap,
        replace_app, swap_app_bundles, swap_path, update_asset,
    };
    use std::path::{Path, PathBuf};

    fn test_root(label: &str) -> PathBuf {
        std::env::temp_dir().join(format!(
            "smelt-updater-{label}-{}",
            uuid::Uuid::new_v4().simple()
        ))
    }

    /// 只有一个标记文件的假 App，用来认出交换前后是哪一份。
    fn marked_app(app: &Path, marker: &str) {
        std::fs::create_dir_all(app).unwrap();
        std::fs::write(app.join(marker), marker).unwrap();
    }

    #[test]
    fn app_bundle_resolves_only_after_following_symlinks() {
        let stamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let root =
            std::env::temp_dir().join(format!("smelt-macos-bundle-{}-{stamp}", std::process::id()));
        let app_bundle = root.join("Smelt.app");
        let app_exe = app_bundle.join("Contents/MacOS/smelt");
        let managed = root.join("managed/smelt");
        let system = root.join("system/smelt");
        std::fs::create_dir_all(app_exe.parent().unwrap()).unwrap();
        std::fs::create_dir_all(managed.parent().unwrap()).unwrap();
        std::fs::create_dir_all(system.parent().unwrap()).unwrap();
        std::fs::write(&app_exe, "").unwrap();
        std::os::unix::fs::symlink(&app_exe, &managed).unwrap();
        std::os::unix::fs::symlink(&managed, &system).unwrap();

        assert!(app_bundle_for_exe(&system).is_err());
        let resolved = std::fs::canonicalize(&system).unwrap();
        assert_eq!(
            app_bundle_for_exe(&resolved).unwrap(),
            std::fs::canonicalize(&app_bundle).unwrap()
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn quiesce_stops_daemon_before_probing_its_app_holder() {
        use std::cell::Cell;

        let managed_holder_alive = Cell::new(true);
        let result = quiesce_app_target_with(
            || {
                managed_holder_alive.set(false);
                true
            },
            || {
                Ok(if managed_holder_alive.get() {
                    BundleUseState::InUse
                } else {
                    BundleUseState::Unused
                })
            },
        );
        assert!(result.is_ok());
    }

    #[test]
    fn quiesce_refuses_unknown_holders_or_failed_daemon_shutdown() {
        use std::cell::Cell;

        let stopped = Cell::new(false);
        let occupied = quiesce_app_target_with(
            || {
                stopped.set(true);
                true
            },
            || Ok(BundleUseState::InUse),
        );
        assert!(stopped.get());
        assert!(occupied.is_err());

        let probed = Cell::new(false);
        let failed = quiesce_app_target_with(
            || false,
            || {
                probed.set(true);
                Ok(BundleUseState::Unused)
            },
        );
        assert!(failed.is_err());
        assert!(!probed.get());
    }

    fn signed_test_app(root: &std::path::Path, bundle_id: &str) -> PathBuf {
        use std::os::unix::fs::PermissionsExt as _;

        let app = root.join("Smelt.app");
        let macos = app.join("Contents/MacOS");
        let resources = app.join("Contents/Resources");
        std::fs::create_dir_all(&macos).unwrap();
        std::fs::create_dir_all(&resources).unwrap();
        std::fs::write(
            app.join("Contents/Info.plist"),
            format!(
                r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0"><dict>
<key>CFBundleExecutable</key><string>smelt</string>
<key>CFBundleIdentifier</key><string>{bundle_id}</string>
<key>CFBundleName</key><string>Smelt</string>
<key>CFBundlePackageType</key><string>APPL</string>
<key>CFBundleShortVersionString</key><string>0.0.1</string>
<key>CFBundleVersion</key><string>1</string>
</dict></plist>
"#
            ),
        )
        .unwrap();
        for executable in ["smelt", "smeltd", super::INSTALLER_NAME] {
            let path = macos.join(executable);
            std::fs::copy("/usr/bin/true", &path).unwrap();
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
        std::fs::write(resources.join("release"), b"signed-resource").unwrap();

        let output = std::process::Command::new("/usr/bin/codesign")
            .args(["--force", "--deep", "--sign", "-"])
            .arg(&app)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "测试 App 签名失败：{}",
            String::from_utf8_lossy(&output.stderr)
        );
        app
    }

    #[test]
    fn installer_gate_blocks_new_gui_before_waiting_for_existing_gui() {
        const TEST_NAME: &str =
            "updater::macos::tests::installer_gate_blocks_new_gui_before_waiting_for_existing_gui";
        const ROLE_ENV: &str = "SMELT_RUNTIME_GATE_TEST_ROLE";
        const ROOT_ENV: &str = "SMELT_RUNTIME_GATE_TEST_ROOT";

        // flock 是跨进程协议；不能用同一测试进程里的多个 fd 模拟多个 GUI，macOS
        // 对这种锁转换的语义会产生偶发假阳性。子测试分别扮演真实 GUI 进程。
        if let Ok(role) = std::env::var(ROLE_ENV) {
            let root = PathBuf::from(std::env::var_os(ROOT_ENV).expect("子进程缺少锁目录"));
            match role.as_str() {
                "hold" => {
                    let _lease = acquire_app_runtime_lease_at(&root).unwrap();
                    std::fs::write(root.join("holder-ready"), b"ready").unwrap();
                    while !root.join("release-holder").exists() {
                        std::thread::sleep(std::time::Duration::from_millis(10));
                    }
                    return;
                }
                "probe" => {
                    let code = if acquire_app_runtime_lease_at(&root).is_ok() {
                        0
                    } else {
                        75
                    };
                    std::process::exit(code);
                }
                other => panic!("未知门闩测试角色：{other}"),
            }
        }

        let root = test_root("runtime-gate");
        std::fs::create_dir_all(&root).unwrap();
        let test_binary = std::env::current_exe().unwrap();
        let spawn_role = |role: &str| {
            std::process::Command::new(&test_binary)
                .args(["--exact", TEST_NAME, "--nocapture"])
                .env(ROLE_ENV, role)
                .env(ROOT_ENV, &root)
                .spawn()
                .unwrap()
        };
        let probe = || spawn_role("probe").wait().unwrap().code();

        let mut existing_gui = spawn_role("hold");
        let holder_deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
        while !root.join("holder-ready").exists() {
            assert!(
                std::time::Instant::now() < holder_deadline,
                "旧 GUI 子进程未及时建立 runtime lease"
            );
            std::thread::sleep(std::time::Duration::from_millis(10));
        }

        let (tx, rx) = std::sync::mpsc::channel();
        let installer_root = root.clone();
        let installer = std::thread::spawn(move || {
            let lease = acquire_installer_runtime_lease_at(
                &installer_root,
                std::time::Duration::from_secs(3),
            );
            tx.send(lease).unwrap();
        });

        // installer 必须先封住 launch gate，再等待旧 GUI 的 runtime lease。
        let gate_deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
        loop {
            if probe() == Some(75) {
                break;
            }
            assert!(
                std::time::Instant::now() < gate_deadline,
                "installer 未及时封住 GUI 启动入口"
            );
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        assert!(
            rx.try_recv().is_err(),
            "旧 GUI 未退出时 installer 不应拿到独占 lease"
        );

        std::fs::write(root.join("release-holder"), b"release").unwrap();
        assert!(existing_gui.wait().unwrap().success());
        let installer_lease = rx
            .recv_timeout(std::time::Duration::from_secs(2))
            .unwrap()
            .unwrap();
        assert_eq!(probe(), Some(75), "installer 提交期间新 GUI 必须 fail-fast");

        drop(installer_lease);
        installer.join().unwrap();
        assert_eq!(probe(), Some(0), "installer 释放后 GUI 应能重新建立 lease");
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn lsof_only_proves_unused_with_exit_one_and_no_output() {
        assert_eq!(
            classify_bundle_use_output(false, Some(1), b"", b"").unwrap(),
            BundleUseState::Unused
        );
        assert_eq!(
            classify_bundle_use_output(true, Some(0), b"p42\n", b"").unwrap(),
            BundleUseState::InUse
        );
        assert!(classify_bundle_use_output(false, Some(1), b"", b"warning").is_err());
        assert!(classify_bundle_use_output(false, Some(2), b"", b"").is_err());
        assert!(classify_bundle_use_output(false, None, b"", b"").is_err());
    }

    #[test]
    fn lsof_failure_names_the_holder() {
        let error = classify_bundle_use_output(
            false,
            Some(1),
            b"p4242\ncsmelt\nn/Applications/Smelt.app/Contents/MacOS/smelt\n",
            b"",
        )
        .unwrap_err()
        .to_string();
        assert!(error.contains("p4242"), "{error}");
        assert!(error.contains("Contents/MacOS/smelt"), "{error}");

        let long = vec![b'x'; 5000];
        let error = classify_bundle_use_output(false, Some(1), &long, b"")
            .unwrap_err()
            .to_string();
        assert!(error.contains("(5000 bytes total)"), "{error}");
        assert!(error.len() < 2000, "输出要截断：{}B", error.len());
    }

    #[test]
    fn update_asset_is_the_app_zip_for_this_arch() {
        let release: GithubRelease = serde_json::from_str(
            r#"{"tag_name":"v0.9.0","assets":[
                {"name":"Smelt.dmg","browser_download_url":"https://example.test/Smelt.dmg"},
                {"name":"Smelt.app.zip","browser_download_url":"https://example.test/Smelt.app.zip"},
                {"name":"Smelt-x86_64.dmg","browser_download_url":"https://example.test/Smelt-x86_64.dmg"},
                {"name":"Smelt-x86_64.app.zip","browser_download_url":"https://example.test/Smelt-x86_64.app.zip"}]}"#,
        )
        .unwrap();
        let expected = if cfg!(target_arch = "aarch64") {
            "https://example.test/Smelt.app.zip"
        } else {
            "https://example.test/Smelt-x86_64.app.zip"
        };
        assert_eq!(
            update_asset(&release).map(|asset| asset.browser_download_url.as_str()),
            Some(expected)
        );
    }

    #[test]
    fn macos_bundle_validation_checks_identity_and_signed_resources() {
        let root = test_root("signed-bundle");
        let app = signed_test_app(&root.join("valid"), super::BUNDLE_ID);

        super::validate_app_bundle_against(&app, &app).unwrap();

        std::fs::write(app.join("Contents/Resources/release"), b"tampered-resource").unwrap();
        let signature_error = super::validate_app_bundle_against(&app, &app).unwrap_err();
        assert!(
            signature_error
                .to_string()
                .contains("signature check failed")
        );

        let missing_installer = signed_test_app(&root.join("missing-installer"), super::BUNDLE_ID);
        std::fs::remove_file(
            missing_installer
                .join("Contents/MacOS")
                .join(super::INSTALLER_NAME),
        )
        .unwrap();
        let helper_error =
            super::validate_app_bundle_against(&missing_installer, &missing_installer).unwrap_err();
        assert!(
            helper_error
                .to_string()
                .contains("missing a required program")
        );

        let wrong_identity = signed_test_app(&root.join("wrong-id"), "com.example.not-smelt");
        let identity_error = super::validate_app_bundle_against(&wrong_identity, &app).unwrap_err();
        assert!(
            identity_error
                .to_string()
                .contains("Bundle ID doesn't match")
        );

        let different_signature =
            signed_test_app(&root.join("different-signature"), super::BUNDLE_ID);
        std::fs::write(
            different_signature.join("Contents/Resources/other"),
            b"different signed payload",
        )
        .unwrap();
        let output = std::process::Command::new("/usr/bin/codesign")
            .args(["--force", "--deep", "--sign", "-"])
            .arg(&different_signature)
            .output()
            .unwrap();
        assert!(output.status.success());
        let signer_error =
            super::validate_app_bundle_against(&different_signature, &app).unwrap_err();
        assert!(signer_error.to_string().contains("signing identity"));
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn replace_app_swaps_in_the_new_app_and_removes_the_old_one() {
        let root = test_root("replace");
        let target = root.join("Smelt.app");
        let swap = swap_path(&target, "t1").unwrap();
        marked_app(&target, "old");
        marked_app(&swap, "new");

        replace_app(&target, &swap, |_| Ok(())).unwrap();

        assert!(target.join("new").is_file());
        assert!(!target.join("old").exists());
        assert!(!swap.exists(), "换下来的旧 App 要删掉");
        std::fs::remove_dir_all(root).unwrap();
    }

    /// 停不下守护、还有进程在用旧 App：什么都不换，新 App 留在交换位置上由调用方删。
    #[test]
    fn replace_app_changes_nothing_when_the_app_cannot_be_quiesced() {
        let root = test_root("replace-busy");
        let target = root.join("Smelt.app");
        let swap = swap_path(&target, "t1").unwrap();
        marked_app(&target, "old");
        marked_app(&swap, "new");

        assert!(replace_app(&target, &swap, |_| anyhow::bail!("in use")).is_err());

        assert!(target.join("old").is_file());
        assert!(swap.join("new").is_file());
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn replace_app_installs_when_there_is_no_app_yet() {
        let root = test_root("replace-fresh");
        let target = root.join("Smelt.app");
        let swap = swap_path(&target, "t1").unwrap();
        marked_app(&swap, "new");

        replace_app(&target, &swap, |_| panic!("没有旧 App，不用停守护")).unwrap();

        assert!(target.join("new").is_file());
        assert!(!swap.exists());
        std::fs::remove_dir_all(root).unwrap();
    }

    /// 启动时只清 App 旁边的交换位置，别的东西一概不碰。
    #[test]
    fn only_swap_paths_next_to_the_app_count_as_leftovers() {
        let root = test_root("leftovers");
        let target = root.join("Smelt.app");
        let swap = swap_path(&target, "abc").unwrap();
        marked_app(&target, "current");
        marked_app(&swap, "old");
        marked_app(&root.join("Other.app"), "other");
        marked_app(&root.join(".Other.smelt-update-abc.app"), "other");
        std::fs::write(root.join(".Smelt.smelt-update-abc.txt"), "x").unwrap();

        assert_eq!(leftover_swaps(&target), vec![swap.clone()]);
        assert!(!is_swap_path(&target, &target));
        assert!(!is_swap_path(
            &target,
            &root.join("nested").join(".Smelt.smelt-update-abc.app")
        ));
        assert!(remove_swap(&target, &root.join("Other.app")).is_err());
        assert!(root.join("Other.app").is_dir());

        remove_swap(&target, &swap).unwrap();
        assert!(!swap.exists());
        remove_swap(&target, &swap).unwrap();
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn swap_id_cannot_escape_the_app_directory() {
        let target = Path::new("/Applications/Smelt.app");
        assert_eq!(
            swap_path(target, "abc123").unwrap(),
            Path::new("/Applications/.Smelt.smelt-update-abc123.app")
        );
        assert!(swap_path(target, "../../evil").is_err());
        assert!(swap_path(target, "").is_err());
    }

    /// 包里的版本要和更新源说的是同一次发版：构建信息不同不要紧，版本号不同就是更新源写错了。
    #[test]
    fn package_version_must_match_the_update_source() {
        let root = test_root("bundle-version");
        let app = root.join("Smelt.app");
        std::fs::create_dir_all(app.join("Contents")).unwrap();
        std::fs::write(
            app.join("Contents/Info.plist"),
            r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0"><dict>
<key>CFBundleShortVersionString</key><string>2026.1102.0+g32c3172</string>
</dict></plist>
"#,
        )
        .unwrap();
        let version = |text: &str| semver::Version::parse(text).unwrap();

        ensure_bundle_version(&app, &version("2026.1102.0")).unwrap();
        let error = ensure_bundle_version(&app, &version("2026.1103.0"))
            .unwrap_err()
            .to_string();
        assert!(error.contains("2026.1103.0"), "{error}");
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn app_directories_are_exchanged_atomically() {
        let root = std::env::temp_dir().join(format!(
            "smelt-update-swap-test-{}",
            uuid::Uuid::new_v4().simple()
        ));
        let current = root.join("Smelt.app");
        let candidate = root.join(".Smelt.smelt-update-test.app");
        std::fs::create_dir_all(&current).unwrap();
        std::fs::create_dir_all(&candidate).unwrap();
        std::fs::write(current.join("old"), b"old").unwrap();
        std::fs::write(candidate.join("new"), b"new").unwrap();

        swap_app_bundles(&current, &candidate).unwrap();

        assert!(current.join("new").is_file());
        assert!(candidate.join("old").is_file());
        std::fs::remove_dir_all(root).unwrap();
    }
}
