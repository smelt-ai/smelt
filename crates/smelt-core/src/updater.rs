//! 在线更新：检查公司 COS manifest、静默下载新版 zip、退出时把 `.app` 换成新版本。
//!
//! 不碰 GPUI，纯文件/网络操作，方便独立验证。这里没有 tokio 运行时，`reqwest`
//! 得在临时 current-thread 运行时里 `block_on`（调用方负责套
//! `cx.background_executor().spawn`）。

use std::fs::{File, OpenOptions};
use std::io::{Read, Write};
#[cfg(unix)]
use std::os::fd::AsRawFd;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, MutexGuard};
use std::time::Duration;

use anyhow::Context as _;
use sha2::{Digest, Sha256};

const BUNDLE_ID: &str = "com.zzfn.smelt";
const APP_NAME: &str = "Smelt";
const DEV_MANIFEST_URL: &str =
    "https://oss-fx-int.nioint.com/fx/app-ios-smelt-mac/__fx_config__/update.dev.json";
const PROD_MANIFEST_URL: &str =
    "https://oss-fx-int.nioint.com/fx/app-ios-smelt-mac/__fx_config__/update.prod.json";
const BUNDLED_RELEASE_URL_FILE: &str = "Contents/Resources/SmeltUpdateURL";
const UPDATE_STATE_FILE: &str = "update-state.json";
const UPDATE_STATE_DIR: &str = ".smelt";
const UPDATE_LOCK_FILE: &str = "update.lock";
const APP_LAUNCH_GATE_FILE: &str = "app-launch-gate.lock";
const APP_RUNTIME_LOCK_FILE: &str = "app-runtime.lock";
const BUNDLE_FINGERPRINT_VERSION: &str = "sha256-tree-v1";
const INSTALLER_NAME: &str = "smelt-installer";
/// 下载进度上报节流：攒够这么多字节才推一次事件，别让每个 chunk 都触发一次重绘。
const PROGRESS_REPORT_STEP: u64 = 512 * 1024;

/// 更新通道。manifest 地址固定在客户端，COS 文件只负责维护该通道当前指向的 ZIP。
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum UpdateChannel {
    /// 内测包：允许同一个展示版本反复发布不同构建。
    Dev,
    /// 生产包：默认只检查正式发布链路。
    #[default]
    Prod,
}

impl UpdateChannel {
    pub const ALL: [Self; 2] = [Self::Dev, Self::Prod];

    pub const fn label(self) -> &'static str {
        match self {
            Self::Dev => "内测版",
            Self::Prod => "生产版",
        }
    }

    pub const fn manifest_url(self) -> &'static str {
        match self {
            Self::Dev => DEV_MANIFEST_URL,
            Self::Prod => PROD_MANIFEST_URL,
        }
    }
}

/// manifest 中的候选更新。`version` 只是展示信息，是否更新由 URL 是否变化决定，
/// 因为内测可能在同一个用户版本下连续发布多个不同 ZIP。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct UpdateCandidate {
    pub version: String,
    pub url: String,
}

/// 更新流程的状态机，展示在设置页。
#[derive(Clone, Default)]
pub enum UpdateStatus {
    #[default]
    Idle,
    Checking,
    UpToDate,
    /// 关掉自动下载时的落点：查到新版本只如实告知，等用户点「下载并更新」再往下走。
    /// 保留 `url` 是因为下载仍以 manifest 给的地址为准，不能等按下按钮再查一次
    /// ——那样两次查到的可能已经不是同一个包了。
    Available {
        version: String,
        url: String,
    },
    /// `total` 为 `None` 表示服务端没给 Content-Length，进度条只能跑不确定动画。
    Downloading {
        version: String,
        received: u64,
        total: Option<u64>,
    },
    /// ZIP 下完了，正在解压 + 拷贝 `.app`，耗时不可测。
    Installing {
        version: String,
    },
    /// 正在复制、持久化并派生 helper；派生成功后 GUI 才会正常退出。
    Applying {
        version: String,
    },
    ReadyToInstall(StagedUpdate),
    /// 安装 helper 启动失败；已验证的包和持久化作业仍保留，可直接重试。
    InstallFailed(StagedUpdate),
    /// UI 只保存可直接展示的失败类别；底层网络、文件系统和协议错误写入 app.log，
    /// 避免把内部 URL 或 reqwest 文本直接呈现给用户。
    Failed(UpdateFailure),
}

/// 更新链路中面向用户的失败类别。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum UpdateFailure {
    Check,
    Download,
    Recovery,
}

impl UpdateFailure {
    pub const fn title(self) -> &'static str {
        match self {
            Self::Check => "检查更新失败",
            Self::Download => "下载更新失败",
            Self::Recovery => "恢复更新失败",
        }
    }

    pub const fn detail(self) -> &'static str {
        match self {
            Self::Check => "暂时无法连接更新服务，请检查网络后重试。",
            Self::Download => "更新包下载或准备失败，请检查网络和可用存储空间后重试。",
            Self::Recovery => "上次更新事务尚未恢复，请重试；现有安装不会被覆盖。",
        }
    }
}

impl UpdateStatus {
    pub fn can_check(&self) -> bool {
        matches!(
            self,
            Self::Idle
                | Self::UpToDate
                | Self::Available { .. }
                | Self::InstallFailed(_)
                | Self::Failed(UpdateFailure::Check | UpdateFailure::Download)
        )
    }

    pub fn can_retry_recovery(&self) -> bool {
        matches!(self, Self::Failed(UpdateFailure::Recovery))
    }

    pub fn ready_update(&self) -> Option<&StagedUpdate> {
        match self {
            Self::ReadyToInstall(update) | Self::InstallFailed(update) => Some(update),
            _ => None,
        }
    }

    pub fn staged_update(&self) -> Option<&StagedUpdate> {
        match self {
            Self::ReadyToInstall(update) | Self::InstallFailed(update) => Some(update),
            _ => None,
        }
    }
}

/// `download_and_stage` 通过回调往外推的进度事件。
pub enum DownloadProgress {
    Bytes { received: u64, total: Option<u64> },
    Installing,
}

/// 候选 App 已复制到正式 App 同卷并完成校验后，调用方在交换前完成守护交接。
pub enum InstallPreparation {
    Proceed,
    RetryLater,
}

pub enum FinalizeOutcome {
    Installed,
    RetryLater,
    Invalidated,
}

/// GUI 成功派生一次性 installer 后的持久化凭据。命令行只携带这两个不透明 id；
/// 候选、目标 App、父进程和重启策略全部从受锁保护的 SQLite 事务读取。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct InstallerTicket {
    pub update_id: String,
    pub attempt_id: String,
}

const INSTALLER_PARENT_WAIT_TIMEOUT: Duration = Duration::from_secs(120);
const INSTALLER_PARENT_POLL_INTERVAL: Duration = Duration::from_millis(100);

/// 判断 manifest 是否指向一个本机没有记录过的发布包。
///
/// `None` 只会出现在迁移前的旧安装或手工安装的首个包：没有可比较的发布地址时，
/// 选择让 updater 下载一次并在成功安装后记住它，避免同版本内测包永远无法更新。
pub fn is_update_available(latest_url: &str, current_url: Option<&str>) -> bool {
    let latest_url = latest_url.trim();
    current_url
        .map(str::trim)
        .is_none_or(|current| current != latest_url)
}

/// 检查拿到结果之后该往哪走。
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CheckOutcome {
    /// 自动更新开着：直接进后台下载，不打扰用户。
    Download(UpdateCandidate),
    /// 自动更新关着：只把版本号亮出来，等用户点。
    Notify(UpdateCandidate),
    NoUpdate,
}

/// 把"要不要自动下载"这段分支从 GPUI 回调里抽出来单独可测。
///
/// 这里是「关掉开关后会不会偷偷下载」的唯一判定点，写错了用户是察觉不到的
/// ——包已经装好了才发现，所以值得用测试钉死。
pub fn decide_check_outcome(
    candidate: UpdateCandidate,
    current_release_url: Option<&str>,
    auto_install: bool,
) -> CheckOutcome {
    if !is_update_available(&candidate.url, current_release_url) {
        return CheckOutcome::NoUpdate;
    }
    if auto_install {
        CheckOutcome::Download(candidate)
    } else {
        CheckOutcome::Notify(candidate)
    }
}

/// 安装失败后手动检查的决策：更新源仍指向同一包时保留已验证的暂存作业，避免
/// 无意义地重新下载；URL 已变化（包括服务端回滚到当前版本）时返回替换后的常规落点。
pub fn decide_failed_update_check(
    candidate: UpdateCandidate,
    current_release_url: Option<&str>,
    failed: &StagedUpdate,
    auto_install: bool,
) -> Option<CheckOutcome> {
    if candidate.url.trim() == failed.url.trim() {
        return None;
    }
    Some(decide_check_outcome(
        candidate,
        current_release_url,
        auto_install,
    ))
}

#[derive(serde::Deserialize)]
struct UpdateManifest {
    #[serde(default)]
    version: Option<serde_json::Value>,
    url: String,
}

/// 查指定通道的 manifest，返回版本展示文本和 ZIP 发布地址。
pub async fn fetch_latest(channel: UpdateChannel) -> anyhow::Result<UpdateCandidate> {
    let url = channel.manifest_url();
    let resp = reqwest::Client::new()
        .get(url)
        .header("User-Agent", "smelt-updater")
        .header("Accept", "application/json")
        .send()
        .await?
        .error_for_status()?;
    validate_download_url(resp.url().as_str())?;
    let manifest: UpdateManifest = resp.json().await?;
    let download_url = manifest.url.trim().to_string();
    if download_url.is_empty() {
        anyhow::bail!("{} 缺少 url", channel.manifest_url());
    }
    validate_download_url(&download_url)?;

    let version = manifest
        .version
        .as_ref()
        .and_then(manifest_version_text)
        .filter(|v| !v.is_empty() && v != "0")
        .unwrap_or_else(|| release_label_from_url(&download_url));
    Ok(UpdateCandidate {
        version,
        url: download_url,
    })
}

fn validate_download_url(download_url: &str) -> anyhow::Result<()> {
    let parsed = url::Url::parse(download_url)
        .map_err(|error| anyhow::anyhow!("更新 url 无效：{download_url}（{error}）"))?;
    if parsed.scheme() != "https" {
        anyhow::bail!("更新 url 必须使用 HTTPS：{download_url}");
    }
    Ok(())
}

fn manifest_version_text(value: &serde_json::Value) -> Option<String> {
    match value {
        serde_json::Value::String(value) => Some(value.trim().to_string()),
        serde_json::Value::Number(value) => Some(value.to_string()),
        _ => None,
    }
}

fn release_label_from_url(download_url: &str) -> String {
    url::Url::parse(download_url)
        .ok()
        .and_then(|url| {
            url.path_segments().and_then(|mut segments| {
                segments
                    .rfind(|segment| !segment.is_empty())
                    .map(str::to_string)
            })
        })
        .map(|value| value.trim_end_matches(".zip").to_string())
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| "更新包".to_string())
}

/// 已下载暂存、尚未应用到 App 的更新包。持久化到 SQLite `json/update-state.json`，
/// 让用户在任何时刻退出（包括强杀）后，下次启动都能接上安装。
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
enum StagedUpdatePhase {
    Preparing,
    #[default]
    Ready,
    Applying,
}

#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct StagedUpdate {
    #[serde(default)]
    id: String,
    pub version: String,
    pub url: String,
    pub staged_app: PathBuf,
    #[serde(default)]
    fingerprint: String,
    #[serde(default)]
    phase: StagedUpdatePhase,
    #[serde(default)]
    swap_app: Option<PathBuf>,
    /// 已启动、正等待 GUI 退出的一次性安装尝试。helper 只能凭作业 id + attempt id
    /// 取回它，不能从命令行指定候选或目标路径。
    #[serde(default)]
    installer_attempt: Option<InstallerAttempt>,
    /// 进程外 helper 曾失败。恢复时只展示失败并等待用户显式重试；新 attempt 创建
    /// 成功时清除。只存状态，不持久化可能含路径或系统细节的错误正文。
    #[serde(default)]
    installer_failed: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
struct InstallerAttempt {
    id: String,
    target_app: PathBuf,
    relaunch: bool,
    helper_dir: PathBuf,
}

impl StagedUpdate {
    fn new(version: &str, url: &str) -> anyhow::Result<Self> {
        validate_download_url(url)?;
        if version.trim().is_empty() {
            anyhow::bail!("更新作业缺少版本标识");
        }
        let id = uuid::Uuid::new_v4().simple().to_string();
        let root = cache_root()?;
        let staged_app = root.join(format!("{APP_NAME}-{}-{id}.app", safe_component(version)));
        Ok(Self {
            id,
            version: version.to_string(),
            url: url.trim().to_string(),
            staged_app,
            fingerprint: String::new(),
            phase: StagedUpdatePhase::Preparing,
            swap_app: None,
            installer_attempt: None,
            installer_failed: false,
        })
    }

    fn normalize_legacy_metadata(&mut self) {
        if self.id.is_empty() {
            let mut hasher = Sha256::new();
            hasher.update(self.url.as_bytes());
            hasher.update(self.staged_app.as_os_str().as_encoded_bytes());
            self.id = format!("legacy-{:x}", hasher.finalize());
        }
    }

    pub fn installer_failed(&self) -> bool {
        self.installer_failed
    }
}

#[derive(Default, serde::Serialize, serde::Deserialize)]
struct UpdateState {
    /// 最近一次成功安装的 manifest URL。
    #[serde(default)]
    current_url: Option<String>,
    /// 上次下载暂存、还没应用成功的更新；启动时存在则自动补装。
    #[serde(default)]
    staged: Option<StagedUpdate>,
    /// 原子交换成功后留下的旧 App。当前旧进程退出前保留完整 bundle；下次启动时
    /// 只清理由本次事务明确登记的目录。
    #[serde(default)]
    cleanup_app: Option<PathBuf>,
}

static UPDATE_GATE: Mutex<()> = Mutex::new(());

struct UpdateFileLock {
    _process_guard: MutexGuard<'static, ()>,
    _file: File,
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
        .with_context(|| format!("打开 App 锁失败：{name}"))
}

#[cfg(unix)]
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

#[cfg(unix)]
fn acquire_app_runtime_lease_at(dir: &Path) -> anyhow::Result<AppRuntimeLease> {
    let launch_gate = open_app_lock_at(dir, APP_LAUNCH_GATE_FILE)?;
    if !try_flock(&launch_gate, libc::LOCK_SH, "获取 App launch gate 失败")? {
        anyhow::bail!("App 正在由 installer 更新，请稍后重新打开");
    }
    let runtime = open_app_lock_at(dir, APP_RUNTIME_LOCK_FILE)?;
    if !try_flock(&runtime, libc::LOCK_SH, "获取 App runtime 共享锁失败")? {
        anyhow::bail!("App 正在由 installer 更新，请稍后重新打开");
    }
    // runtime lease 已建立，installer 即使随后拿到 launch gate，也必须等本 GUI 退出。
    drop(launch_gate);
    Ok(AppRuntimeLease {
        _runtime_file: runtime,
        _launch_gate_file: None,
    })
}

#[cfg(unix)]
pub fn acquire_app_runtime_lease() -> anyhow::Result<AppRuntimeLease> {
    acquire_app_runtime_lease_at(&update_dir()?)
}

#[cfg(not(unix))]
pub fn acquire_app_runtime_lease() -> anyhow::Result<AppRuntimeLease> {
    anyhow::bail!("当前平台不支持 App runtime 锁")
}

#[cfg(unix)]
fn acquire_installer_runtime_lease_at(
    dir: &Path,
    timeout: Duration,
) -> anyhow::Result<AppRuntimeLease> {
    let launch_gate = open_app_lock_at(dir, APP_LAUNCH_GATE_FILE)?;
    let runtime = open_app_lock_at(dir, APP_RUNTIME_LOCK_FILE)?;
    let started = std::time::Instant::now();
    loop {
        if try_flock(
            &launch_gate,
            libc::LOCK_EX,
            "获取 App launch gate 独占锁失败",
        )? {
            break;
        }
        if started.elapsed() >= timeout {
            anyhow::bail!("等待 App launch gate 超时，未提交 App 更新");
        }
        std::thread::sleep(INSTALLER_PARENT_POLL_INTERVAL);
    }
    // 从这里起新 GUI 无法进入；只需等待已经持有 runtime lease 的 GUI 全部退出。
    loop {
        if try_flock(&runtime, libc::LOCK_EX, "获取 App runtime 独占锁失败")? {
            return Ok(AppRuntimeLease {
                _runtime_file: runtime,
                _launch_gate_file: Some(launch_gate),
            });
        }
        if started.elapsed() >= timeout {
            anyhow::bail!("等待其它 Smelt GUI 退出超时，未提交 App 更新");
        }
        std::thread::sleep(INSTALLER_PARENT_POLL_INTERVAL);
    }
}

#[cfg(unix)]
fn acquire_installer_runtime_lease() -> anyhow::Result<AppRuntimeLease> {
    acquire_installer_runtime_lease_at(&update_dir()?, INSTALLER_PARENT_WAIT_TIMEOUT)
}

#[cfg(not(unix))]
fn acquire_installer_runtime_lease() -> anyhow::Result<AppRuntimeLease> {
    anyhow::bail!("当前平台不支持 App runtime 锁")
}

fn update_dir() -> anyhow::Result<PathBuf> {
    dirs::home_dir()
        .map(|home| home.join(UPDATE_STATE_DIR))
        .ok_or_else(|| anyhow::anyhow!("找不到用户目录"))
}

fn update_state_path() -> anyhow::Result<PathBuf> {
    Ok(update_dir()?.join(UPDATE_STATE_FILE))
}

fn acquire_update_lock() -> anyhow::Result<UpdateFileLock> {
    let process_guard = UPDATE_GATE
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    let dir = update_dir()?;
    std::fs::create_dir_all(&dir)?;
    let file = OpenOptions::new()
        .create(true)
        .read(true)
        .write(true)
        .truncate(false)
        .open(dir.join(UPDATE_LOCK_FILE))?;
    #[cfg(unix)]
    if unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX) } != 0 {
        return Err(std::io::Error::last_os_error().into());
    }
    Ok(UpdateFileLock {
        _process_guard: process_guard,
        _file: file,
    })
}

fn update_state_store(path: &Path) -> anyhow::Result<smelt_store::Store> {
    let database = if path.file_name().and_then(|name| name.to_str())
        == Some(smelt_store::DATABASE_FILE_NAME)
    {
        path.to_path_buf()
    } else {
        path.parent()
            .unwrap_or_else(|| Path::new("."))
            .join(smelt_store::DATABASE_FILE_NAME)
    };
    crate::sqlite_state::open_sqlite_store(&database)
        .map_err(anyhow::Error::msg)
        .with_context(|| format!("打开更新状态库失败：{}", database.display()))
}

fn load_update_state_locked(path: &Path) -> anyhow::Result<UpdateState> {
    let store = update_state_store(path)
        .with_context(|| format!("读取更新状态失败：{}", path.display()))?;
    match store.get_update_state_snapshot() {
        Ok(Some(snapshot)) => state_from_snapshot(snapshot),
        Ok(None) => Ok(UpdateState::default()),
        Err(error) => Err(anyhow::Error::msg(error))
            .with_context(|| format!("读取更新状态失败：{}", path.display())),
    }
}

fn save_update_state_locked(path: &Path, state: &UpdateState) -> anyhow::Result<()> {
    let store = update_state_store(path)
        .with_context(|| format!("保存更新状态失败：{}", path.display()))?;
    store
        .put_update_state_snapshot(&snapshot_from_state(state)?)
        .map_err(anyhow::Error::msg)
        .with_context(|| format!("保存更新状态失败：{}", path.display()))
}

fn snapshot_from_state(state: &UpdateState) -> anyhow::Result<smelt_store::UpdateStateSnapshot> {
    Ok(smelt_store::UpdateStateSnapshot {
        current_url: state.current_url.clone(),
        staged_json: state
            .staged
            .as_ref()
            .map(serde_json::to_vec)
            .transpose()
            .context("序列化暂存更新失败")?,
        cleanup_app: state
            .cleanup_app
            .as_ref()
            .map(|path| path.to_string_lossy().into_owned()),
    })
}

fn state_from_snapshot(snapshot: smelt_store::UpdateStateSnapshot) -> anyhow::Result<UpdateState> {
    Ok(UpdateState {
        current_url: snapshot.current_url,
        staged: snapshot
            .staged_json
            .as_deref()
            .map(serde_json::from_slice)
            .transpose()
            .context("解析暂存更新失败")?,
        cleanup_app: snapshot.cleanup_app.map(PathBuf::from),
    })
}

fn same_update(left: &StagedUpdate, right: &StagedUpdate) -> bool {
    let same_payload = left.url == right.url && left.staged_app == right.staged_app;
    if left.id.is_empty() || right.id.is_empty() {
        same_payload
    } else {
        left.id == right.id && same_payload
    }
}

fn begin_staged_update_in_state(
    state: &mut UpdateState,
    update: &StagedUpdate,
) -> anyhow::Result<()> {
    if update.phase != StagedUpdatePhase::Preparing {
        anyhow::bail!("新更新作业必须从 Preparing 开始");
    }
    if state.staged.is_some() {
        anyhow::bail!("已有另一条更新作业正在准备或等待安装");
    }
    state.staged = Some(update.clone());
    Ok(())
}

fn promote_staged_update_in_state(
    state: &mut UpdateState,
    update: &StagedUpdate,
) -> anyhow::Result<()> {
    if update.phase != StagedUpdatePhase::Ready || update.fingerprint.is_empty() {
        anyhow::bail!("只有校验完成的更新作业才能进入 Ready");
    }
    let current = state
        .staged
        .as_ref()
        .filter(|current| same_update(current, update))
        .ok_or_else(|| anyhow::anyhow!("更新作业已被恢复流程取消或替换"))?;
    match current.phase {
        StagedUpdatePhase::Preparing => {
            state.staged = Some(update.clone());
            Ok(())
        }
        StagedUpdatePhase::Ready if current == update => Ok(()),
        StagedUpdatePhase::Ready | StagedUpdatePhase::Applying => {
            anyhow::bail!("拒绝由迟到的下载回调回退更新事务")
        }
    }
}

fn clear_preparing_update_in_state(state: &mut UpdateState, expected: &StagedUpdate) -> bool {
    if state.staged.as_ref().is_some_and(|current| {
        same_update(current, expected) && current.phase == StagedUpdatePhase::Preparing
    }) {
        state.staged = None;
        true
    } else {
        false
    }
}

/// 返回允许由手动检查替换的精确失败作业。只认同一条 Ready 事务；若恢复流程已经
/// 推进到另一条作业或 Applying，调用方必须停下，不能用过期 UI 状态清除它。
fn failed_update_to_replace(
    state: &UpdateState,
    expected: &StagedUpdate,
) -> anyhow::Result<StagedUpdate> {
    if expected.phase != StagedUpdatePhase::Ready {
        anyhow::bail!("安装失败作业不在 Ready 阶段");
    }
    let pending = state
        .staged
        .clone()
        .ok_or_else(|| anyhow::anyhow!("安装失败作业已不存在"))?;
    if !same_update(&pending, expected) {
        anyhow::bail!("待安装更新已被另一条作业替换");
    }
    if pending.phase != StagedUpdatePhase::Ready {
        anyhow::bail!("待安装更新已进入应用阶段，不能由检查操作作废");
    }
    Ok(pending)
}

/// 下载开始前先登记 Preparing；应用成功前不清除，中途强杀由启动恢复接管。
fn begin_staged_update(update: &StagedUpdate) -> anyhow::Result<()> {
    let _guard = acquire_update_lock()?;
    let path = update_state_path()?;
    let mut state = load_update_state_locked(&path)?;
    // 旧 Bundle 仍占用时允许下载和暂存下一版；唯一不能跨越的边界是下一次交换。
    // 这样 cleanup_app 仍只有一个明确所有者，也不会让网络下载被长寿命旧 daemon 卡死。
    begin_staged_update_in_state(&mut state, update)?;
    save_update_state_locked(&path, &state)
}

/// 只允许 Preparing -> Ready，不能让迟到的下载结果覆盖 Applying。
fn promote_staged_update(update: &StagedUpdate) -> anyhow::Result<()> {
    let _guard = acquire_update_lock()?;
    let path = update_state_path()?;
    let mut state = load_update_state_locked(&path)?;
    promote_staged_update_in_state(&mut state, update)?;
    save_update_state_locked(&path, &state)
}

/// 下载失败只清除仍停在 Preparing 的同一作业，不能抹掉恢复/安装已推进的状态。
fn clear_preparing_update(expected: &StagedUpdate) -> anyhow::Result<()> {
    let _guard = acquire_update_lock()?;
    let path = update_state_path()?;
    let mut state = load_update_state_locked(&path)?;
    if clear_preparing_update_in_state(&mut state, expected) {
        save_update_state_locked(&path, &state)?;
    }
    Ok(())
}

/// manifest 已指向另一份发布包时，原子作废精确匹配的安装失败作业。状态先持久化
/// 清空，再删除缓存；中途强杀至多留下可清理的缓存，不会让新旧作业同时生效。
pub fn discard_failed_update(expected: &StagedUpdate) -> anyhow::Result<()> {
    let _guard = acquire_update_lock()?;
    let state_path = update_state_path()?;
    let mut state = load_update_state_locked(&state_path)?;
    if state.staged.is_none() {
        return Ok(());
    }
    let app_bundle = current_app_bundle()?;
    discard_failed_update_locked(&state_path, &mut state, expected, &app_bundle)
}

fn discard_failed_update_locked(
    state_path: &Path,
    state: &mut UpdateState,
    expected: &StagedUpdate,
    app_bundle: &Path,
) -> anyhow::Result<()> {
    if state.staged.is_none() {
        return Ok(());
    }
    let pending = failed_update_to_replace(state, expected)?;
    abandon_pending_update_locked(state_path, state, &pending, app_bundle, None)
}

/// 读取当前包对应的发布 URL。先读升级成功后写入的用户状态，再读打包时可选携带的
/// `Contents/Resources/SmeltUpdateURL`，兼容已经发布但没有状态文档的安装包。
pub fn current_release_url() -> anyhow::Result<Option<String>> {
    let _guard = acquire_update_lock()?;
    let state = load_update_state_locked(&update_state_path()?)?;
    if let Some(url) = state.current_url.filter(|url| !url.trim().is_empty()) {
        return Ok(Some(url));
    }

    let bundled_path = current_app_bundle()?.join(BUNDLED_RELEASE_URL_FILE);
    match std::fs::read_to_string(&bundled_path) {
        Ok(url) => Ok(Some(url.trim().to_string()).filter(|url| !url.is_empty())),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => {
            Err(error).with_context(|| format!("读取当前发布 URL 失败：{}", bundled_path.display()))
        }
    }
}

fn installer_root() -> anyhow::Result<PathBuf> {
    let dir = update_dir()?.join("installer");
    std::fs::create_dir_all(&dir)?;
    Ok(dir)
}

fn copy_installer_helper(source: &Path, destination: &Path) -> anyhow::Result<()> {
    let metadata = std::fs::symlink_metadata(source)
        .with_context(|| format!("安装 helper 不存在：{}", source.display()))?;
    if metadata.file_type().is_symlink() || !metadata.is_file() {
        anyhow::bail!("安装 helper 必须是普通文件：{}", source.display());
    }
    let parent = destination
        .parent()
        .ok_or_else(|| anyhow::anyhow!("安装 helper 目标缺少父目录"))?;
    std::fs::create_dir(parent)
        .with_context(|| format!("创建安装 helper 目录失败：{}", parent.display()))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        std::fs::set_permissions(parent, std::fs::Permissions::from_mode(0o700))?;
    }
    std::fs::copy(source, destination)
        .with_context(|| format!("复制安装 helper 失败：{}", destination.display()))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        std::fs::set_permissions(destination, std::fs::Permissions::from_mode(0o700))?;
    }
    Ok(())
}

fn clear_installer_attempt_in_state(
    state: &mut UpdateState,
    update_id: &str,
    attempt_id: &str,
) -> bool {
    let Some(pending) = state.staged.as_mut() else {
        return false;
    };
    if pending.id == update_id
        && pending
            .installer_attempt
            .as_ref()
            .is_some_and(|attempt| attempt.id == attempt_id)
    {
        pending.installer_attempt = None;
        true
    } else {
        false
    }
}

fn clear_installer_attempt(update_id: &str, attempt_id: &str) -> anyhow::Result<()> {
    let _guard = acquire_update_lock()?;
    let state_path = update_state_path()?;
    let mut state = load_update_state_locked(&state_path)?;
    if clear_installer_attempt_in_state(&mut state, update_id, attempt_id) {
        save_update_state_locked(&state_path, &state)?;
    }
    Ok(())
}

fn record_installer_failure_in_state(
    state: &mut UpdateState,
    update_id: &str,
    attempt_id: &str,
) -> bool {
    let Some(pending) = state.staged.as_mut() else {
        return false;
    };
    if pending.id != update_id
        || !pending
            .installer_attempt
            .as_ref()
            .is_some_and(|attempt| attempt.id == attempt_id)
    {
        return false;
    }
    pending.installer_attempt = None;
    pending.installer_failed = true;
    true
}

fn record_installer_failure(update_id: &str, attempt_id: &str) -> anyhow::Result<bool> {
    let _guard = acquire_update_lock()?;
    let state_path = update_state_path()?;
    let mut state = load_update_state_locked(&state_path)?;
    let recorded = record_installer_failure_in_state(&mut state, update_id, attempt_id);
    if recorded {
        save_update_state_locked(&state_path, &state)?;
    }
    Ok(recorded)
}

/// 复制已随当前 App 签名的 helper 到私有目录，先持久化 attempt，再派生进程。
/// spawn 成功前 GUI 不得退出；spawn 失败会撤销精确匹配的 attempt，Ready 作业仍保留。
pub fn launch_installer(update: &StagedUpdate, relaunch: bool) -> anyhow::Result<InstallerTicket> {
    let app_bundle = current_app_bundle()?;
    let source = app_bundle.join("Contents/MacOS").join(INSTALLER_NAME);
    let attempt_id = uuid::Uuid::new_v4().simple().to_string();
    let helper_dir = installer_root()?.join(&attempt_id);
    let helper = helper_dir.join(INSTALLER_NAME);
    copy_installer_helper(&source, &helper)?;

    let ticket = (|| -> anyhow::Result<InstallerTicket> {
        let _guard = acquire_update_lock()?;
        let state_path = update_state_path()?;
        let mut state = load_update_state_locked(&state_path)?;
        let pending = state
            .staged
            .as_mut()
            .filter(|pending| same_update(pending, update))
            .ok_or_else(|| anyhow::anyhow!("待安装更新已被另一条更新作业替换"))?;
        if pending.phase != StagedUpdatePhase::Ready {
            anyhow::bail!("更新事务不在 Ready 阶段");
        }
        if pending.installer_attempt.is_some() {
            anyhow::bail!("已有 installer 正在等待 GUI 退出");
        }
        pending.installer_failed = false;
        pending.installer_attempt = Some(InstallerAttempt {
            id: attempt_id.clone(),
            target_app: app_bundle,
            relaunch,
            helper_dir: helper_dir.clone(),
        });
        let update_id = pending.id.clone();
        save_update_state_locked(&state_path, &state)?;
        Ok(InstallerTicket {
            update_id,
            attempt_id: attempt_id.clone(),
        })
    })();
    let ticket = match ticket {
        Ok(ticket) => ticket,
        Err(error) => {
            let _ = std::fs::remove_dir_all(&helper_dir);
            return Err(error);
        }
    };

    let spawned = std::process::Command::new(&helper)
        .arg("--update-id")
        .arg(&ticket.update_id)
        .arg("--attempt-id")
        .arg(&ticket.attempt_id)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn();
    if let Err(error) = spawned {
        let rollback = clear_installer_attempt(&ticket.update_id, &ticket.attempt_id);
        let _ = std::fs::remove_dir_all(&helper_dir);
        rollback.context("派生 installer 失败，且撤销 attempt 也失败")?;
        return Err(error).context("派生 installer 失败");
    }
    Ok(ticket)
}

fn installer_attempt_from_state(
    state: UpdateState,
    update_id: &str,
    attempt_id: &str,
) -> anyhow::Result<(StagedUpdate, InstallerAttempt)> {
    if !is_valid_update_id(update_id) || !is_valid_update_id(attempt_id) {
        anyhow::bail!("installer ticket 非法");
    }
    let pending = state
        .staged
        .filter(|pending| pending.id == update_id && pending.phase == StagedUpdatePhase::Ready)
        .ok_or_else(|| anyhow::anyhow!("installer 对应的 Ready 更新作业不存在"))?;
    let attempt = pending
        .installer_attempt
        .clone()
        .filter(|attempt| attempt.id == attempt_id)
        .ok_or_else(|| anyhow::anyhow!("installer attempt 已被撤销或替换"))?;
    Ok((pending, attempt))
}

fn load_installer_attempt(
    update_id: &str,
    attempt_id: &str,
) -> anyhow::Result<(StagedUpdate, InstallerAttempt)> {
    let _guard = acquire_update_lock()?;
    let state = load_update_state_locked(&update_state_path()?)?;
    installer_attempt_from_state(state, update_id, attempt_id)
}

fn relaunch_app(app_bundle: &Path) -> anyhow::Result<()> {
    // 不使用 -n：若用户已抢先启动新版，只激活已有实例，不能再制造重复 GUI。
    let status = std::process::Command::new("/usr/bin/open")
        .arg(app_bundle)
        .status()
        .context("启动新版 Smelt 失败")?;
    if !status.success() {
        anyhow::bail!("启动新版 Smelt 失败：open exit {status}");
    }
    Ok(())
}

/// `smelt-installer` 的完整生命周期：重新读取 ticket，获取排他的 App runtime lease，
/// 再次读取并提交同一 attempt，释放 lease 后按事务策略拉起固定目标路径。
/// 安装失败也会清除精确 attempt、保留 Ready 作业并尽力拉回旧 App。
pub fn run_installer(update_id: &str, attempt_id: &str) -> anyhow::Result<()> {
    let (_pending, attempt) = load_installer_attempt(update_id, attempt_id)?;
    let outcome = (|| {
        let runtime_lease = acquire_installer_runtime_lease()?;
        let outcome = finalize_pending_update_for_attempt(
            update_id,
            attempt_id,
            &attempt.target_app,
            &runtime_lease,
        );
        // 新 GUI 必须能在启动时拿到共享 lease；不能持着独占锁调用 LaunchServices。
        drop(runtime_lease);
        outcome
    })();
    // Invalidated 只有在锁内重新验证精确 attempt 后才会返回，并且作废状态已经落盘；
    // 它不再有 Ready 作业可标记失败，但仍属于当前 helper，有权把稳定 GUI 拉回来。
    let invalidated_by_this_attempt = matches!(outcome, Ok(FinalizeOutcome::Invalidated));
    let result = match outcome {
        Ok(FinalizeOutcome::Installed) => Ok(()),
        Ok(FinalizeOutcome::Invalidated) => anyhow::bail!("候选 App 最终校验失效"),
        Ok(FinalizeOutcome::RetryLater) => anyhow::bail!("installer 不支持延后提交"),
        Err(error) => Err(error),
    };
    let persistence_result = if result.is_err() {
        Some(
            record_installer_failure(update_id, attempt_id)
                .context("installer 失败，且持久化失败状态也失败"),
        )
    } else {
        None
    };
    // 成功安装，或失败确实由当前 attempt 原子登记时，helper 才有权 relaunch。
    // ticket 已失效意味着另一个 GUI/attempt 已接管；旧 helper 不能抢跑污染新事务。
    let may_relaunch = result.is_ok()
        || invalidated_by_this_attempt
        || matches!(persistence_result, Some(Ok(true)));
    let relaunch_result = if attempt.relaunch && may_relaunch {
        relaunch_app(&attempt.target_app)
    } else {
        Ok(())
    };
    if result.is_ok() {
        let _ = std::fs::remove_dir_all(&attempt.helper_dir);
    }
    let result = match (result, persistence_result) {
        (Err(install_error), Some(Err(persistence_error))) => Err(install_error).context(format!(
            "持久化 installer 失败状态也失败：{persistence_error:#}"
        )),
        (result, _) => result,
    };
    match (result, relaunch_result) {
        (Ok(()), Ok(())) => Ok(()),
        (Err(install_error), Ok(())) => Err(install_error),
        (Ok(()), Err(relaunch_error)) => Err(relaunch_error),
        (Err(install_error), Err(relaunch_error)) => Err(install_error).context(format!(
            "安装失败后重新打开 Smelt 也失败：{relaunch_error:#}"
        )),
    }
}

fn cache_root() -> anyhow::Result<PathBuf> {
    let dir = dirs::cache_dir()
        .ok_or_else(|| anyhow::anyhow!("找不到系统缓存目录"))?
        .join(BUNDLE_ID)
        .join("update");
    std::fs::create_dir_all(&dir)?;
    Ok(dir)
}

/// 下载 ZIP → 解压 → 校验 `.app` → 原子记录待安装作业。
///
/// ZIP 有几十 MB，一次性 `.bytes()` 读完既吃内存又让 UI 整段时间无进度可显示，
/// 所以按 chunk 流式落盘，边下边通过 `on_progress` 上报字节数。
pub async fn download_and_stage(
    url: &str,
    version: &str,
    on_progress: impl Fn(DownloadProgress),
) -> anyhow::Result<StagedUpdate> {
    let mut update = StagedUpdate::new(version, url)?;
    begin_staged_update(&update)?;

    let root = cache_root()?;
    let zip_path = root.join(format!(".{APP_NAME}-{}.zip", update.id));
    let extract_root = root.join(format!(".{APP_NAME}-{}-extract", update.id));
    let _ = std::fs::remove_dir_all(&extract_root);
    let _ = std::fs::remove_dir_all(&update.staged_app);
    let _ = std::fs::remove_file(&zip_path);

    let result = async {
        let mut resp = reqwest::Client::new()
            .get(url)
            .header("User-Agent", "smelt-updater")
            .send()
            .await?
            .error_for_status()?;
        validate_download_url(resp.url().as_str())?;
        if resp
            .headers()
            .get(reqwest::header::CONTENT_TYPE)
            .and_then(|value| value.to_str().ok())
            .is_some_and(|value| value.to_ascii_lowercase().contains("text/html"))
        {
            anyhow::bail!("更新地址返回了 HTML，不是 ZIP：{url}");
        }
        let total = resp.content_length();
        on_progress(DownloadProgress::Bytes { received: 0, total });

        let mut file = std::fs::File::create(&zip_path)?;
        let mut received = 0u64;
        let mut reported = 0u64;
        while let Some(chunk) = resp.chunk().await? {
            file.write_all(&chunk)?;
            received += chunk.len() as u64;
            if received - reported >= PROGRESS_REPORT_STEP || Some(received) == total {
                reported = received;
                on_progress(DownloadProgress::Bytes { received, total });
            }
        }
        file.flush()?;
        drop(file);
        on_progress(DownloadProgress::Installing);

        extract_zip(&zip_path, &extract_root)?;
        let extracted_app = find_app_bundle(&extract_root)?;
        std::fs::rename(&extracted_app, &update.staged_app).map_err(|error| {
            anyhow::anyhow!(
                "移动新版 .app 失败：{} → {}（{error}）",
                extracted_app.display(),
                update.staged_app.display()
            )
        })?;
        let trusted_app = current_app_bundle()?;
        validate_staged_update(&update, &trusted_app)
    }
    .await;

    let _ = std::fs::remove_dir_all(&extract_root);
    let _ = std::fs::remove_file(&zip_path);

    match result {
        Ok(fingerprint) => {
            update.fingerprint = fingerprint;
            update.phase = StagedUpdatePhase::Ready;
            if let Err(error) = promote_staged_update(&update) {
                let _ = clear_preparing_update(&update);
                let _ = std::fs::remove_dir_all(&update.staged_app);
                return Err(error);
            }
            Ok(update)
        }
        Err(error) => {
            let _ = clear_preparing_update(&update);
            let _ = std::fs::remove_dir_all(&update.staged_app);
            Err(error)
        }
    }
}

fn safe_component(value: &str) -> String {
    let value: String = value
        .chars()
        .filter(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'))
        .collect();
    let value = value.trim_matches('.');
    if value.is_empty() {
        "latest".to_string()
    } else {
        value.to_string()
    }
}

fn extract_zip(zip_path: &Path, extract_root: &Path) -> anyhow::Result<()> {
    std::fs::create_dir_all(extract_root)?;
    let out = std::process::Command::new("/usr/bin/ditto")
        .args(["-x", "-k"])
        .arg(zip_path)
        .arg(extract_root)
        .output()?;
    if !out.status.success() {
        anyhow::bail!("解压 ZIP 失败：{}", String::from_utf8_lossy(&out.stderr));
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
    anyhow::bail!("ZIP 中没有找到 .app 包")
}

fn validate_staged_update(update: &StagedUpdate, trusted_app: &Path) -> anyhow::Result<String> {
    validate_staged_metadata(update)?;
    let staged = registered_staged_path(update)?;
    let fingerprint = validate_app_bundle_against(&staged, trusted_app)?;
    if !update.fingerprint.is_empty() && update.fingerprint != fingerprint {
        anyhow::bail!("暂存 App 指纹与下载完成时不一致");
    }
    Ok(fingerprint)
}

fn validate_staged_metadata(update: &StagedUpdate) -> anyhow::Result<()> {
    validate_download_url(&update.url)?;
    if !is_valid_update_id(&update.id) {
        anyhow::bail!("更新作业 id 非法");
    }
    if update.version.trim().is_empty() {
        anyhow::bail!("更新作业缺少版本标识");
    }
    Ok(())
}

fn validate_app_bundle_against(app: &Path, trusted_app: &Path) -> anyhow::Result<String> {
    if !app.is_dir() || app.extension().and_then(|ext| ext.to_str()) != Some("app") {
        anyhow::bail!("更新包不是有效的 .app 目录：{}", app.display());
    }
    for executable in [
        app.join("Contents/MacOS/smelt"),
        app.join("Contents/MacOS/smeltd"),
        app.join("Contents/MacOS").join(INSTALLER_NAME),
    ] {
        if !executable.is_file() {
            anyhow::bail!("更新包缺少必要程序：{}", executable.display());
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            if std::fs::metadata(&executable)?.permissions().mode() & 0o111 == 0 {
                anyhow::bail!("更新包程序不可执行：{}", executable.display());
            }
        }
    }
    let info_plist = app.join("Contents/Info.plist");
    if !info_plist.is_file() {
        anyhow::bail!("更新包缺少 Info.plist：{}", info_plist.display());
    }

    #[cfg(target_os = "macos")]
    {
        let bundle_id = std::process::Command::new("/usr/bin/plutil")
            .args(["-extract", "CFBundleIdentifier", "raw", "-o", "-"])
            .arg(&info_plist)
            .output()?;
        if !bundle_id.status.success()
            || String::from_utf8_lossy(&bundle_id.stdout).trim() != BUNDLE_ID
        {
            anyhow::bail!("更新包 Bundle ID 不匹配");
        }
        let signature = std::process::Command::new("/usr/bin/codesign")
            .args(["--verify", "--deep", "--strict"])
            .arg(app)
            .output()?;
        if !signature.status.success() {
            anyhow::bail!(
                "更新包签名校验失败：{}",
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
            anyhow::bail!("更新包签发身份与当前 Smelt 不匹配");
        }
    }

    app_fingerprint(app)
}

#[cfg(target_os = "macos")]
fn designated_requirement(app: &Path) -> anyhow::Result<String> {
    let output = std::process::Command::new("/usr/bin/codesign")
        .args(["-d", "-r-"])
        .arg(app)
        .output()
        .with_context(|| format!("读取 App designated requirement 失败：{}", app.display()))?;
    if !output.status.success() {
        anyhow::bail!(
            "读取 App designated requirement 失败：{}",
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
        .ok_or_else(|| anyhow::anyhow!("App 缺少 designated requirement：{}", app.display()))
}

fn app_fingerprint(app: &Path) -> anyhow::Result<String> {
    let mut hasher = Sha256::new();
    let mut entries = vec![app.to_path_buf()];
    let mut cursor = 0;
    while cursor < entries.len() {
        let path = entries[cursor].clone();
        cursor += 1;
        if std::fs::symlink_metadata(&path)?.file_type().is_dir() {
            for entry in std::fs::read_dir(&path)? {
                entries.push(entry?.path());
            }
        }
    }
    entries.sort_by(|left, right| {
        left.strip_prefix(app)
            .unwrap_or(left)
            .as_os_str()
            .as_encoded_bytes()
            .cmp(
                right
                    .strip_prefix(app)
                    .unwrap_or(right)
                    .as_os_str()
                    .as_encoded_bytes(),
            )
    });

    hasher.update(BUNDLE_FINGERPRINT_VERSION.as_bytes());
    hasher.update((entries.len() as u64).to_le_bytes());
    let mut buffer = [0u8; 64 * 1024];
    for path in entries {
        let relative = path.strip_prefix(app).unwrap_or(&path);
        let relative = relative.as_os_str().as_encoded_bytes();
        hasher.update((relative.len() as u64).to_le_bytes());
        hasher.update(relative);

        let metadata = std::fs::symlink_metadata(&path)?;
        let file_type = metadata.file_type();
        if file_type.is_dir() {
            hasher.update(b"d");
        } else if file_type.is_file() {
            hasher.update(b"f");
            hasher.update(metadata.len().to_le_bytes());
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt as _;
                hasher.update((metadata.permissions().mode() & 0o111).to_le_bytes());
            }
            let mut file = File::open(&path)
                .with_context(|| format!("读取 App 文件失败：{}", path.display()))?;
            loop {
                let read = file.read(&mut buffer)?;
                if read == 0 {
                    break;
                }
                hasher.update(&buffer[..read]);
            }
        } else if file_type.is_symlink() {
            hasher.update(b"l");
            let target = std::fs::read_link(&path)?;
            let target = target.as_os_str().as_encoded_bytes();
            hasher.update((target.len() as u64).to_le_bytes());
            hasher.update(target);
        } else {
            anyhow::bail!("App 包含不支持的文件类型：{}", path.display());
        }
    }
    Ok(format!(
        "{BUNDLE_FINGERPRINT_VERSION}:{:x}",
        hasher.finalize()
    ))
}

/// 从当前可执行文件路径反推 `Smelt.app` 的位置：
/// `<App>.app/Contents/MacOS/smelt` 往上 3 层就是 `<App>.app`。
/// 非 `.app` 环境（比如 `cargo run`）直接报错，不做任何文件操作。
pub fn current_app_bundle_path() -> anyhow::Result<PathBuf> {
    current_app_bundle()
}

fn current_app_bundle() -> anyhow::Result<PathBuf> {
    let exe = std::env::current_exe()?;
    let bundle = exe
        .parent()
        .and_then(Path::parent)
        .and_then(Path::parent)
        .ok_or_else(|| anyhow::anyhow!("可执行文件路径层级不足：{}", exe.display()))?
        .to_path_buf();
    if bundle.extension().and_then(|e| e.to_str()) != Some("app") {
        anyhow::bail!("不在 .app 包里运行（{}），跳过自更新", bundle.display());
    }
    Ok(bundle)
}

fn swap_path(app_bundle: &Path, update_id: &str) -> anyhow::Result<PathBuf> {
    if !is_valid_update_id(update_id) {
        anyhow::bail!("更新作业 id 非法");
    }
    let parent = app_bundle
        .parent()
        .ok_or_else(|| anyhow::anyhow!("App 路径没有父目录：{}", app_bundle.display()))?;
    let stem = app_bundle
        .file_stem()
        .and_then(|value| value.to_str())
        .unwrap_or(APP_NAME);
    Ok(parent.join(format!(".{stem}.smelt-update-{update_id}.app")))
}

fn is_valid_update_id(update_id: &str) -> bool {
    !update_id.is_empty()
        && update_id
            .chars()
            .all(|value| value.is_ascii_alphanumeric() || value == '-')
}

fn is_registered_swap_path(app_bundle: &Path, path: &Path) -> bool {
    let Some(parent) = app_bundle.parent() else {
        return false;
    };
    let Some(name) = path.file_name().and_then(|value| value.to_str()) else {
        return false;
    };
    path.parent() == Some(parent)
        && name.starts_with(&format!(
            ".{}.smelt-update-",
            app_bundle
                .file_stem()
                .and_then(|value| value.to_str())
                .unwrap_or(APP_NAME)
        ))
        && name.ends_with(".app")
}

fn remove_registered_swap(app_bundle: &Path, path: &Path) -> anyhow::Result<()> {
    if !is_registered_swap_path(app_bundle, path) {
        anyhow::bail!("拒绝清理未登记在 App 同目录的路径：{}", path.display());
    }
    let metadata = match std::fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(error.into()),
    };
    if metadata.file_type().is_dir() {
        std::fs::remove_dir_all(path)?;
    } else {
        // 正常交换目录一定是目录；文件或符号链接只能是中断/篡改残留，删除路径本身，
        // 绝不跟随链接。
        std::fs::remove_file(path)?;
    }
    Ok(())
}

fn is_registered_staged_path(cache: &Path, path: &Path) -> bool {
    let Some(name) = path.file_name().and_then(|value| value.to_str()) else {
        return false;
    };
    path.parent() == Some(cache)
        && name.starts_with(&format!("{APP_NAME}-"))
        && name.ends_with(".app")
}

fn registered_staged_path(update: &StagedUpdate) -> anyhow::Result<PathBuf> {
    let cache = cache_root()?.canonicalize()?;
    if std::fs::symlink_metadata(&update.staged_app)?
        .file_type()
        .is_symlink()
    {
        anyhow::bail!("暂存 App 不能是符号链接：{}", update.staged_app.display());
    }
    let staged = update
        .staged_app
        .canonicalize()
        .with_context(|| format!("暂存 App 不存在：{}", update.staged_app.display()))?;
    if !is_registered_staged_path(&cache, &staged) {
        anyhow::bail!("暂存 App 不属于更新缓存：{}", staged.display());
    }
    Ok(staged)
}

fn copy_app_bundle(source: &Path, destination: &Path) -> anyhow::Result<()> {
    let output = std::process::Command::new("/usr/bin/ditto")
        .arg(source)
        .arg(destination)
        .output()?;
    if !output.status.success() {
        anyhow::bail!(
            "复制新版 App 失败：{}",
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }
    Ok(())
}

#[cfg(target_os = "macos")]
fn swap_app_bundles(left: &Path, right: &Path) -> anyhow::Result<()> {
    use std::ffi::CString;
    use std::os::unix::ffi::OsStrExt;

    let left = CString::new(left.as_os_str().as_bytes())?;
    let right = CString::new(right.as_os_str().as_bytes())?;
    if unsafe { libc::renamex_np(left.as_ptr(), right.as_ptr(), libc::RENAME_SWAP) } != 0 {
        return Err(std::io::Error::last_os_error()).context("原子交换新旧 App 失败");
    }
    Ok(())
}

#[cfg(target_os = "linux")]
fn swap_app_bundles(left: &Path, right: &Path) -> anyhow::Result<()> {
    use std::ffi::CString;
    use std::os::unix::ffi::OsStrExt;

    let left = CString::new(left.as_os_str().as_bytes())?;
    let right = CString::new(right.as_os_str().as_bytes())?;
    if unsafe {
        libc::renameat2(
            libc::AT_FDCWD,
            left.as_ptr(),
            libc::AT_FDCWD,
            right.as_ptr(),
            libc::RENAME_EXCHANGE,
        )
    } != 0
    {
        return Err(std::io::Error::last_os_error()).context("原子交换新旧 App 失败");
    }
    Ok(())
}

#[cfg(not(any(target_os = "macos", target_os = "linux")))]
fn swap_app_bundles(_left: &Path, _right: &Path) -> anyhow::Result<()> {
    anyhow::bail!("当前平台不支持原子交换 App 目录")
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ApplyingRecoveryState {
    Installed,
    NotSwapped,
}

/// `Applying` 落盘后只能凭磁盘上的完整指纹判定原子交换是否发生。
/// 任一路径无法读取或两边都不是候选包时都保持现场并报错，不能猜测性删除可能是
/// 唯一旧版本的交换目录。
fn classify_applying_recovery(
    pending: &StagedUpdate,
    app_bundle: &Path,
) -> anyhow::Result<ApplyingRecoveryState> {
    let swap = pending
        .swap_app
        .as_ref()
        .ok_or_else(|| anyhow::anyhow!("Applying 状态缺少交换目录，无法判定安装结果"))?;
    let expected_swap = swap_path(app_bundle, &pending.id)?;
    if swap != &expected_swap {
        anyhow::bail!("Applying 状态包含非法交换路径：{}", swap.display());
    }

    let current_fingerprint = app_fingerprint(app_bundle)
        .with_context(|| format!("读取当前 App 指纹失败：{}", app_bundle.display()))?;
    if current_fingerprint == pending.fingerprint {
        return Ok(ApplyingRecoveryState::Installed);
    }

    let swap_fingerprint = app_fingerprint(swap)
        .with_context(|| format!("读取交换 App 指纹失败：{}", swap.display()))?;
    if swap_fingerprint == pending.fingerprint {
        return Ok(ApplyingRecoveryState::NotSwapped);
    }

    anyhow::bail!("当前 App 与交换 App 都不匹配待安装指纹，保留现场以避免误删旧版本")
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
    anyhow::bail!(
        "无法证明旧 App 已空闲（lsof code={code:?} stdout={}B stderr={}B）",
        stdout.len(),
        stderr.len()
    )
}

fn bundle_use_state(app: &Path) -> anyhow::Result<BundleUseState> {
    let output = std::process::Command::new("/usr/sbin/lsof")
        .arg("-Fn")
        .arg("+D")
        .arg(app)
        .output()
        .with_context(|| format!("探测 App 是否仍被使用失败：{}", app.display()))?;
    classify_bundle_use_output(
        output.status.success(),
        output.status.code(),
        &output.stdout,
        &output.stderr,
    )
}

/// 尝试回收事务登记的旧 Bundle。返回 false 表示仍占用；探测失败则报错并保留。
fn cleanup_registered_app_locked(
    state_path: &Path,
    state: &mut UpdateState,
    app_bundle: &Path,
) -> anyhow::Result<bool> {
    let Some(cleanup) = state.cleanup_app.clone() else {
        return Ok(true);
    };
    if !is_registered_swap_path(app_bundle, &cleanup) {
        anyhow::bail!("更新状态包含非法清理路径：{}", cleanup.display());
    }
    if bundle_use_state(&cleanup)? == BundleUseState::InUse {
        return Ok(false);
    }
    remove_registered_swap(app_bundle, &cleanup)
        .with_context(|| format!("清理旧 App 失败：{}", cleanup.display()))?;
    state.cleanup_app = None;
    save_update_state_locked(state_path, state)?;
    Ok(true)
}

fn discard_update_artifacts(update: &StagedUpdate) {
    if let Ok(cache) = cache_root() {
        if is_registered_staged_path(&cache, &update.staged_app) {
            match std::fs::symlink_metadata(&update.staged_app) {
                Ok(metadata) if metadata.file_type().is_symlink() => {
                    let _ = std::fs::remove_file(&update.staged_app);
                }
                Ok(_) => {
                    let _ = std::fs::remove_dir_all(&update.staged_app);
                }
                Err(_) => {}
            }
        }

        // Preparing 阶段被强杀时 ZIP 和解压目录也属于同一个 job；id 只接受生成器
        // 会产生的字符，防止损坏状态通过路径分隔符逃出缓存根目录。
        if is_valid_update_id(&update.id) {
            let _ = std::fs::remove_file(cache.join(format!(".{APP_NAME}-{}.zip", update.id)));
            let _ =
                std::fs::remove_dir_all(cache.join(format!(".{APP_NAME}-{}-extract", update.id)));
        }
    }
}

fn finish_recovered_update_locked(
    state_path: &Path,
    state: &mut UpdateState,
    pending: &StagedUpdate,
    app_bundle: &Path,
) -> anyhow::Result<()> {
    state.current_url = Some(pending.url.trim().to_string());
    state.cleanup_app = pending.swap_app.clone();
    state.staged = None;
    save_update_state_locked(state_path, state)?;
    discard_update_artifacts(pending);
    if let Err(error) = cleanup_registered_app_locked(state_path, state, app_bundle) {
        // 提交已经完成，清理失败不能把一次成功安装重新报告成失败；登记路径仍在
        // update-state.json 中，下次启动会继续收敛。
        crate::app_log::error(
            "updater",
            &format!("更新事务已恢复，旧 App 将留到下次启动清理：{error:#}"),
        );
    }
    Ok(())
}

fn abandon_pending_update_locked(
    state_path: &Path,
    state: &mut UpdateState,
    pending: &StagedUpdate,
    app_bundle: &Path,
    verified_cleanup_app: Option<PathBuf>,
) -> anyhow::Result<()> {
    state.cleanup_app = verified_cleanup_app;
    state.staged = None;
    save_update_state_locked(state_path, state)?;
    discard_update_artifacts(pending);
    if let Err(error) = cleanup_registered_app_locked(state_path, state, app_bundle) {
        crate::app_log::error(
            "updater",
            &format!("作废更新作业后清理交换目录失败，将在下次启动重试：{error:#}"),
        );
    }
    Ok(())
}

/// 启动时在同一个更新事务锁内完成状态恢复和遗留清理。
///
/// `Applying` 表示原子交换前已经写入日志：当前 App 指纹等于候选指纹时说明交换
/// 已完成，只需提交状态；否则回到 Ready 并复用同一个安装流程重试。
pub fn recover_pending_update() -> anyhow::Result<Option<StagedUpdate>> {
    let _guard = acquire_update_lock()?;
    let state_path = update_state_path()?;
    let state = match load_update_state_locked(&state_path) {
        Ok(state) => state,
        Err(error) => {
            let quarantined = crate::sqlite_state::quarantine_json(
                Some(state_path.clone()),
                "update-state.corrupt",
            )
            .map_err(anyhow::Error::msg)
            .with_context(|| format!("更新状态损坏且无法隔离：{}", state_path.display()))?
            .ok_or_else(|| anyhow::anyhow!("更新状态读取失败，但存储中没有可隔离的 payload"))?;
            crate::app_log::error(
                "updater",
                &format!("更新状态损坏，已隔离到 {quarantined}：{error:#}"),
            );
            UpdateState::default()
        }
    };

    let app_bundle = current_app_bundle()?;
    reconcile_update_state_locked(&state_path, state, &app_bundle)
}

fn reconcile_update_state_locked(
    state_path: &Path,
    mut state: UpdateState,
    app_bundle: &Path,
) -> anyhow::Result<Option<StagedUpdate>> {
    match cleanup_registered_app_locked(state_path, &mut state, app_bundle) {
        Ok(true) => {}
        Ok(false) => {
            crate::app_log::info(
                "updater",
                "旧 App 仍被使用；允许暂存更新，但提交下一次交换前必须先回收",
            );
        }
        Err(error) => crate::app_log::error(
            "updater",
            &format!("旧 App 暂未清理；允许恢复暂存作业，但提交交换前会拒绝：{error:#}"),
        ),
    }

    let Some(mut pending) = state.staged.clone() else {
        return Ok(None);
    };
    pending.normalize_legacy_metadata();

    if let Err(error) = validate_staged_metadata(&pending) {
        if pending.phase == StagedUpdatePhase::Applying {
            return Err(error).context("Applying 更新元数据无效，保留交换现场等待人工确认");
        }
        crate::app_log::error(
            "updater",
            &format!("待安装更新元数据无效，已作废：{error:#}"),
        );
        abandon_pending_update_locked(state_path, &mut state, &pending, app_bundle, None)?;
        return Ok(None);
    }

    // Applying 是 write-ahead 状态。先看正式 App 是否已经是目标指纹；此时缓存副本
    // 是否还在都不影响提交，必须先于暂存校验判定。
    let mut applying_recovery = None;
    if pending.phase == StagedUpdatePhase::Applying && !pending.fingerprint.is_empty() {
        let recovery = classify_applying_recovery(&pending, app_bundle)?;
        if recovery == ApplyingRecoveryState::Installed {
            finish_recovered_update_locked(state_path, &mut state, &pending, app_bundle)?;
            return Ok(None);
        }
        applying_recovery = Some(recovery);
    }

    let fingerprint = match validate_staged_update(&pending, app_bundle) {
        Ok(fingerprint) => fingerprint,
        Err(error) => {
            if pending.phase == StagedUpdatePhase::Applying {
                if applying_recovery == Some(ApplyingRecoveryState::NotSwapped) {
                    let verified_swap = pending.swap_app.clone();
                    crate::app_log::error(
                        "updater",
                        &format!(
                            "未交换的候选包 {} 无法通过恢复校验，已安全作废：{error:#}",
                            pending.version
                        ),
                    );
                    abandon_pending_update_locked(
                        state_path,
                        &mut state,
                        &pending,
                        app_bundle,
                        verified_swap,
                    )?;
                    return Ok(None);
                }
                return Err(error)
                    .context("Applying 更新缺少可验证的暂存包，保留交换现场以避免误删旧版本");
            }
            crate::app_log::error(
                "updater",
                &format!(
                    "待安装更新 {} 无法通过恢复校验，已作废并允许重新下载：{error:#}",
                    pending.version
                ),
            );
            abandon_pending_update_locked(state_path, &mut state, &pending, app_bundle, None)?;
            return Ok(None);
        }
    };
    if pending.fingerprint.is_empty() {
        pending.fingerprint = fingerprint;
    }

    if pending.phase == StagedUpdatePhase::Applying {
        // 兼容旧状态缺少指纹、但暂存包仍完整的情况：补出指纹后再判定一次。
        let recovery = match applying_recovery {
            Some(recovery) => recovery,
            None => classify_applying_recovery(&pending, app_bundle)?,
        };
        if recovery == ApplyingRecoveryState::Installed {
            finish_recovered_update_locked(state_path, &mut state, &pending, app_bundle)?;
            return Ok(None);
        }
        let swap = pending
            .swap_app
            .take()
            .ok_or_else(|| anyhow::anyhow!("NotSwapped 判定后交换目录意外缺失"))?;
        remove_registered_swap(app_bundle, &swap)?;
    }

    pending.phase = StagedUpdatePhase::Ready;
    pending.swap_app = None;
    // GUI 能进入恢复就说明旧 helper 没有持有独占 launch gate；遗留 attempt 必须视为
    // 外部安装失败。旧 helper 不得复活，新 attempt 只能由用户显式重试创建。
    pending.installer_failed |= pending.installer_attempt.is_some();
    pending.installer_attempt = None;
    state.staged = Some(pending.clone());
    save_update_state_locked(state_path, &state)?;

    Ok(Some(pending))
}

/// 把当前 App 与同目录的完整候选包做原子交换。当前 App 的路径始终存在；交换前后
/// 都有持久化事务状态，因此进程在任意一步被强杀时，下一次启动都能判定并收敛。
/// `prepare` 只会收到复制并验证完成的同卷候选 App，守护交接不能越过这条校验边界。
pub fn finalize_pending_update(
    update: &StagedUpdate,
    prepare: impl FnOnce(&Path) -> anyhow::Result<InstallPreparation>,
) -> anyhow::Result<FinalizeOutcome> {
    let app_bundle = current_app_bundle()?;
    finalize_pending_update_at(update, None, &app_bundle, prepare)
}

fn finalize_pending_update_for_attempt(
    update_id: &str,
    attempt_id: &str,
    target_app: &Path,
    _runtime_lease: &AppRuntimeLease,
) -> anyhow::Result<FinalizeOutcome> {
    let (update, attempt) = load_installer_attempt(update_id, attempt_id)?;
    if attempt.target_app != target_app {
        anyhow::bail!("installer 目标 App 与持久化 attempt 不一致");
    }
    finalize_pending_update_at(&update, Some(attempt_id), target_app, |_| {
        Ok(InstallPreparation::Proceed)
    })
}

fn finalize_pending_update_at(
    update: &StagedUpdate,
    expected_attempt_id: Option<&str>,
    app_bundle: &Path,
    prepare: impl FnOnce(&Path) -> anyhow::Result<InstallPreparation>,
) -> anyhow::Result<FinalizeOutcome> {
    let _guard = acquire_update_lock()?;
    let state_path = update_state_path()?;
    let mut state = load_update_state_locked(&state_path)?;
    let mut pending = state
        .staged
        .clone()
        .filter(|current| same_update(current, update))
        .ok_or_else(|| anyhow::anyhow!("待安装更新已被另一条更新作业替换"))?;
    pending.normalize_legacy_metadata();
    if pending.phase != StagedUpdatePhase::Ready {
        anyhow::bail!("更新事务不在 Ready 阶段，请先执行恢复");
    }
    if let Some(expected_attempt_id) = expected_attempt_id {
        let _attempt = pending
            .installer_attempt
            .as_ref()
            .filter(|attempt| attempt.id == expected_attempt_id && attempt.target_app == app_bundle)
            .ok_or_else(|| anyhow::anyhow!("installer attempt 已被撤销或替换"))?;
        // 调用者持有 AppRuntimeLease 独占锁；所有新 GUI 要先拿共享锁，因此这里到
        // RENAME_SWAP 之间不会再有 GUI 从目标 Bundle 启动。lsof 继续兜住不懂锁的旧版。
        if bundle_use_state(app_bundle)? != BundleUseState::Unused {
            anyhow::bail!("目标 App 又被进程使用，拒绝提交更新");
        }
    }
    // 旧 Bundle 可能仍被 managed 迁移前的 smeltd 引用。提交正确性不依赖旧包清理；
    // 无法证明没有进程使用时必须保留，由独立 fail-closed GC 后续处理。
    if state.cleanup_app.is_some() {
        anyhow::bail!("上一条更新的旧 App 尚未安全回收，拒绝开始新的替换事务");
    }
    let fingerprint = match validate_staged_update(&pending, app_bundle) {
        Ok(fingerprint) => fingerprint,
        Err(error) => {
            crate::app_log::error(
                "updater",
                &format!("待安装更新校验已失效，作废后重新下载：{error:#}"),
            );
            abandon_pending_update_locked(&state_path, &mut state, &pending, &app_bundle, None)?;
            return Ok(FinalizeOutcome::Invalidated);
        }
    };
    if !pending.fingerprint.is_empty() && pending.fingerprint != fingerprint {
        anyhow::bail!("暂存 App 指纹与持久化记录不一致");
    }
    pending.fingerprint = fingerprint.clone();
    pending.phase = StagedUpdatePhase::Ready;
    pending.swap_app = None;
    state.staged = Some(pending.clone());
    save_update_state_locked(&state_path, &state)?;

    let swap = swap_path(&app_bundle, &pending.id)?;
    remove_registered_swap(&app_bundle, &swap)?;
    if let Err(error) = copy_app_bundle(&pending.staged_app, &swap) {
        let _ = remove_registered_swap(&app_bundle, &swap);
        return Err(error);
    }
    let copied_fingerprint = match validate_app_bundle_against(&swap, app_bundle) {
        Ok(fingerprint) => fingerprint,
        Err(error) => {
            let _ = remove_registered_swap(&app_bundle, &swap);
            return Err(error).context("复制到安装目录后的 App 校验失败");
        }
    };
    if copied_fingerprint != fingerprint {
        let _ = remove_registered_swap(&app_bundle, &swap);
        anyhow::bail!("复制到安装目录后的 App 指纹发生变化");
    }

    match prepare(&swap) {
        Ok(InstallPreparation::Proceed) => {}
        Ok(InstallPreparation::RetryLater) => {
            remove_registered_swap(&app_bundle, &swap)?;
            return Ok(FinalizeOutcome::RetryLater);
        }
        Err(error) => {
            let _ = remove_registered_swap(&app_bundle, &swap);
            return Err(error);
        }
    }

    pending.phase = StagedUpdatePhase::Applying;
    pending.swap_app = Some(swap.clone());
    state.staged = Some(pending.clone());
    if let Err(error) = save_update_state_locked(&state_path, &state) {
        let _ = remove_registered_swap(&app_bundle, &swap);
        return Err(error);
    }

    if let Err(error) = swap_app_bundles(&app_bundle, &swap) {
        pending.phase = StagedUpdatePhase::Ready;
        pending.swap_app = None;
        state.staged = Some(pending);
        if let Err(persist_error) = save_update_state_locked(&state_path, &state) {
            // 磁盘仍是已持久化的 Applying + swap_app。保留候选目录作为恢复证据；
            // 删除它会让下次启动无法判定交换边界。
            return Err(error).context(format!(
                "App 原子交换失败，且恢复 Ready 状态写入失败；已保留交换现场：{persist_error:#}"
            ));
        }
        let _ = remove_registered_swap(&app_bundle, &swap);
        return Err(error);
    }

    let previous_current_url = state.current_url.clone();
    state.current_url = Some(pending.url.trim().to_string());
    state.staged = None;
    state.cleanup_app = Some(swap.clone());
    if let Err(commit_error) = save_update_state_locked(&state_path, &state) {
        if let Err(rollback_error) = swap_app_bundles(&app_bundle, &swap) {
            return Err(commit_error).context(format!(
                "更新状态提交失败，且 App 原子回滚也失败：{rollback_error:#}"
            ));
        }
        pending.phase = StagedUpdatePhase::Ready;
        pending.swap_app = None;
        state.current_url = previous_current_url;
        state.cleanup_app = None;
        state.staged = Some(pending);
        if let Err(persist_error) = save_update_state_locked(&state_path, &state) {
            // 持久状态仍是 Applying，且 App 已回滚。保留 swap 候选，让启动恢复能证明
            // NotSwapped；不能先删现场再留下一个不可解释的 Applying 快照。
            return Err(commit_error).context(format!(
                "App 已原子回滚，但恢复 Ready 状态写入失败；已保留交换现场：{persist_error:#}"
            ));
        }
        let _ = remove_registered_swap(&app_bundle, &swap);
        return Err(commit_error);
    }

    discard_update_artifacts(&pending);
    Ok(FinalizeOutcome::Installed)
}

#[cfg(test)]
mod tests {
    #[cfg(any(target_os = "macos", target_os = "linux"))]
    use super::swap_app_bundles;
    use super::{
        ApplyingRecoveryState, BundleUseState, CheckOutcome, InstallerAttempt, StagedUpdate,
        StagedUpdatePhase, UpdateCandidate, UpdateChannel, UpdateFailure, UpdateManifest,
        UpdateState, UpdateStatus, acquire_app_runtime_lease_at,
        acquire_installer_runtime_lease_at, app_fingerprint, begin_staged_update_in_state,
        classify_applying_recovery, classify_bundle_use_output, clear_installer_attempt_in_state,
        clear_preparing_update_in_state, decide_check_outcome, decide_failed_update_check,
        discard_failed_update_locked, failed_update_to_replace, installer_attempt_from_state,
        is_update_available, is_valid_update_id, load_update_state_locked, manifest_version_text,
        promote_staged_update_in_state, reconcile_update_state_locked,
        record_installer_failure_in_state, release_label_from_url, safe_component, same_update,
        save_update_state_locked, state_from_snapshot, validate_download_url,
    };
    use std::path::PathBuf;

    fn staged_update(id: &str) -> StagedUpdate {
        StagedUpdate {
            id: id.into(),
            version: "0.6.15".into(),
            url: "https://example.test/releases/35039".into(),
            staged_app: PathBuf::from(format!("/tmp/Smelt-{id}.app")),
            fingerprint: "abc123".into(),
            phase: StagedUpdatePhase::Ready,
            swap_app: None,
            installer_attempt: None,
            installer_failed: false,
        }
    }

    fn test_root(label: &str) -> PathBuf {
        std::env::temp_dir().join(format!(
            "smelt-updater-{label}-{}",
            uuid::Uuid::new_v4().simple()
        ))
    }

    fn fake_app(app: PathBuf, executable: &[u8]) -> PathBuf {
        let bin = app.join("Contents/MacOS");
        std::fs::create_dir_all(&bin).unwrap();
        std::fs::write(bin.join("smelt"), executable).unwrap();
        app
    }

    fn fake_current_app(root: &std::path::Path, executable: &[u8]) -> PathBuf {
        fake_app(root.join("Smelt.app"), executable)
    }

    #[cfg(target_os = "macos")]
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

    #[cfg(unix)]
    #[test]
    fn installer_gate_blocks_new_gui_before_waiting_for_existing_gui() {
        const TEST_NAME: &str =
            "updater::tests::installer_gate_blocks_new_gui_before_waiting_for_existing_gui";
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
    fn installer_ticket_is_bound_to_exact_ready_attempt() {
        let mut pending = staged_update("update-one");
        pending.installer_attempt = Some(InstallerAttempt {
            id: "attempt-one".into(),
            target_app: PathBuf::from("/Applications/Smelt.app"),
            relaunch: true,
            helper_dir: PathBuf::from("/tmp/attempt-one"),
        });
        let state = || UpdateState {
            staged: Some(pending.clone()),
            ..UpdateState::default()
        };

        assert!(installer_attempt_from_state(state(), "update-one", "attempt-one").is_ok());
        assert!(installer_attempt_from_state(state(), "update-one", "attempt-old").is_err());
        assert!(installer_attempt_from_state(state(), "update-old", "attempt-one").is_err());

        let mut applying = state();
        applying.staged.as_mut().unwrap().phase = StagedUpdatePhase::Applying;
        assert!(installer_attempt_from_state(applying, "update-one", "attempt-one").is_err());
    }

    #[test]
    fn installer_failure_is_bound_to_exact_attempt() {
        let mut pending = staged_update("update-one");
        pending.installer_attempt = Some(InstallerAttempt {
            id: "attempt-new".into(),
            target_app: PathBuf::from("/Applications/Smelt.app"),
            relaunch: true,
            helper_dir: PathBuf::from("/tmp/attempt-new"),
        });
        let mut state = UpdateState {
            staged: Some(pending),
            ..UpdateState::default()
        };

        assert!(!record_installer_failure_in_state(
            &mut state,
            "update-one",
            "attempt-old"
        ));
        let pending = state.staged.as_ref().unwrap();
        assert!(!pending.installer_failed);
        assert_eq!(
            pending.installer_attempt.as_ref().unwrap().id,
            "attempt-new"
        );

        assert!(record_installer_failure_in_state(
            &mut state,
            "update-one",
            "attempt-new"
        ));
        let pending = state.staged.as_ref().unwrap();
        assert!(pending.installer_failed);
        assert!(pending.installer_attempt.is_none());
    }

    #[test]
    fn stale_helper_cannot_clear_a_new_attempt() {
        let mut pending = staged_update("update-one");
        pending.installer_attempt = Some(InstallerAttempt {
            id: "attempt-new".into(),
            target_app: PathBuf::from("/Applications/Smelt.app"),
            relaunch: true,
            helper_dir: PathBuf::from("/tmp/attempt-new"),
        });
        let mut state = UpdateState {
            staged: Some(pending),
            ..UpdateState::default()
        };

        assert!(!clear_installer_attempt_in_state(
            &mut state,
            "update-one",
            "attempt-old"
        ));
        assert_eq!(
            state
                .staged
                .as_ref()
                .unwrap()
                .installer_attempt
                .as_ref()
                .unwrap()
                .id,
            "attempt-new"
        );
        assert!(clear_installer_attempt_in_state(
            &mut state,
            "update-one",
            "attempt-new"
        ));
        assert!(state.staged.unwrap().installer_attempt.is_none());
    }

    fn candidate(url: &str) -> UpdateCandidate {
        UpdateCandidate {
            version: "0.6.15".into(),
            url: url.into(),
        }
    }

    /// 关掉自动更新后**绝不能**再走下载分支——这是这个开关唯一的实质承诺。
    #[test]
    fn auto_install_off_only_notifies_never_downloads() {
        let outcome = decide_check_outcome(
            candidate("https://example.com/new.zip"),
            Some("https://example.com/old.zip"),
            false,
        );
        assert_eq!(
            outcome,
            CheckOutcome::Notify(candidate("https://example.com/new.zip"))
        );
    }

    #[test]
    fn auto_install_on_downloads_without_asking() {
        let outcome = decide_check_outcome(
            candidate("https://example.com/new.zip"),
            Some("https://example.com/old.zip"),
            true,
        );
        assert_eq!(
            outcome,
            CheckOutcome::Download(candidate("https://example.com/new.zip"))
        );
    }

    /// 开关只管"发现新版之后怎么办"，没有新版时两边都该是同一个结论：没得更新。
    /// 别让关掉开关变成"永远显示有新版"。
    #[test]
    fn same_release_is_no_update_regardless_of_switch() {
        for auto_install in [true, false] {
            let outcome = decide_check_outcome(
                candidate("https://example.com/same.zip"),
                Some("https://example.com/same.zip"),
                auto_install,
            );
            assert_eq!(outcome, CheckOutcome::NoUpdate, "auto={auto_install}");
        }
    }

    #[test]
    fn update_channel_points_to_the_two_internal_manifests() {
        assert!(
            UpdateChannel::Dev
                .manifest_url()
                .ends_with("update.dev.json")
        );
        assert!(
            UpdateChannel::Prod
                .manifest_url()
                .ends_with("update.prod.json")
        );
        assert_ne!(
            UpdateChannel::Dev.manifest_url(),
            UpdateChannel::Prod.manifest_url()
        );
    }

    #[test]
    fn update_failure_messages_stay_user_facing() {
        assert_eq!(UpdateFailure::Check.title(), "检查更新失败");
        assert_eq!(
            UpdateFailure::Check.detail(),
            "暂时无法连接更新服务，请检查网络后重试。"
        );
        assert_eq!(UpdateFailure::Download.title(), "下载更新失败");
        assert_eq!(UpdateFailure::Recovery.title(), "恢复更新失败");
        assert!(
            !UpdateFailure::Check.detail().contains("http"),
            "设置页不应展示底层请求 URL"
        );
    }

    #[test]
    fn url_difference_is_the_update_identity() {
        assert!(!is_update_available(
            " https://example.test/a ",
            Some("https://example.test/a")
        ));
        assert!(is_update_available(
            "https://example.test/b",
            Some("https://example.test/a")
        ));
        assert!(is_update_available("https://example.test/a", None));
    }

    #[test]
    fn update_payload_url_requires_https() {
        assert!(validate_download_url("https://example.test/release.zip").is_ok());
        assert!(validate_download_url("http://example.test/release.zip").is_err());
        assert!(validate_download_url("file:///tmp/release.zip").is_err());
    }

    #[test]
    fn manifest_accepts_numeric_placeholder_version() {
        let manifest: UpdateManifest = serde_json::from_str(
            r#"{"version":0,"url":"https://ios.nioint.com/download/releases/35039"}"#,
        )
        .unwrap();
        assert_eq!(
            manifest_version_text(manifest.version.as_ref().unwrap()),
            Some("0".into())
        );
        assert_eq!(release_label_from_url(&manifest.url), "35039");
    }

    #[test]
    fn staged_file_component_is_safe() {
        assert_eq!(
            safe_component("3q64A_0.6.14_202608101017"),
            "3q64A_0.6.14_202608101017"
        );
        assert_eq!(safe_component("../../$(bad)"), "bad");
        assert_eq!(safe_component("..."), "latest");
        assert!(is_valid_update_id("legacy-abc123"));
        assert!(!is_valid_update_id("../../other"));
    }

    /// 暂存记录要能完整跨进程存活：版本、URL、暂存 .app 路径一个都不能丢。
    #[test]
    fn staged_update_serde_roundtrip() {
        let mut staged = staged_update("job-a");
        staged.phase = StagedUpdatePhase::Applying;
        staged.swap_app = Some(PathBuf::from("/Applications/.Smelt.smelt-update-job-a.app"));
        let json = serde_json::to_string(&staged).unwrap();
        let back: StagedUpdate = serde_json::from_str(&json).unwrap();
        assert_eq!(back, staged);
    }

    #[test]
    fn staged_update_from_previous_version_defaults_to_ready() {
        let staged: StagedUpdate = serde_json::from_str(
            r#"{
                "version":"0.6.15",
                "url":"https://example.test/releases/35039",
                "staged_app":"/tmp/Smelt-0.6.15.app"
            }"#,
        )
        .unwrap();
        assert_eq!(staged.phase, StagedUpdatePhase::Ready);
        assert!(staged.id.is_empty());
        assert!(staged.fingerprint.is_empty());
        assert!(staged.swap_app.is_none());
    }

    #[test]
    fn only_failed_install_jobs_allow_a_new_manual_check() {
        assert!(!UpdateStatus::ReadyToInstall(staged_update("ready")).can_check());
        assert!(UpdateStatus::InstallFailed(staged_update("failed")).can_check());
        assert!(UpdateStatus::Idle.can_check());
        assert!(UpdateStatus::Failed(UpdateFailure::Download).can_check());
        assert!(!UpdateStatus::Failed(UpdateFailure::Recovery).can_check());
        assert!(UpdateStatus::Failed(UpdateFailure::Recovery).can_retry_recovery());
    }

    #[test]
    fn failed_install_check_keeps_the_same_release_but_replaces_a_changed_one() {
        let failed = staged_update("failed");
        let same = UpdateCandidate {
            version: "0.6.15".into(),
            url: failed.url.clone(),
        };
        assert_eq!(
            decide_failed_update_check(
                same,
                Some("https://example.test/releases/old"),
                &failed,
                true
            ),
            None
        );

        let replacement = UpdateCandidate {
            version: "0.6.16".into(),
            url: "https://example.test/releases/35040".into(),
        };
        assert_eq!(
            decide_failed_update_check(
                replacement.clone(),
                Some("https://example.test/releases/old"),
                &failed,
                true,
            ),
            Some(CheckOutcome::Download(replacement))
        );

        let rolled_back = UpdateCandidate {
            version: "0.6.14".into(),
            url: "https://example.test/releases/current".into(),
        };
        assert_eq!(
            decide_failed_update_check(
                rolled_back,
                Some("https://example.test/releases/current"),
                &failed,
                true,
            ),
            Some(CheckOutcome::NoUpdate)
        );
    }

    #[test]
    fn replacing_a_failed_job_never_clears_a_different_or_applying_transaction() {
        let failed = staged_update("failed");
        let mut state = UpdateState {
            staged: Some(failed.clone()),
            ..UpdateState::default()
        };
        assert_eq!(failed_update_to_replace(&state, &failed).unwrap(), failed);

        let newer = staged_update("newer");
        state.staged = Some(newer.clone());
        assert!(failed_update_to_replace(&state, &failed).is_err());
        assert_eq!(state.staged, Some(newer));

        let mut applying = failed.clone();
        applying.phase = StagedUpdatePhase::Applying;
        applying.swap_app = Some(PathBuf::from(
            "/Applications/.Smelt.smelt-update-failed.app",
        ));
        state.staged = Some(applying.clone());
        assert!(failed_update_to_replace(&state, &failed).is_err());
        assert_eq!(state.staged, Some(applying));
    }

    #[test]
    fn replacing_a_failed_job_persists_the_clear_before_the_next_download() {
        let root = test_root("replace-failed");
        let app = fake_current_app(&root, b"current-version");
        let state_path = root.join("update-state.json");
        let failed = staged_update("failed");
        let mut state = UpdateState {
            current_url: Some("https://example.test/releases/current".into()),
            staged: Some(failed.clone()),
            cleanup_app: None,
        };

        discard_failed_update_locked(&state_path, &mut state, &failed, &app).unwrap();

        assert!(state.staged.is_none());
        let persisted = load_update_state_locked(&state_path).unwrap();
        assert!(persisted.staged.is_none());
        assert_eq!(persisted.current_url, state.current_url);
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn update_identity_prevents_late_job_from_clearing_newer_one() {
        let first = staged_update("first");
        let second = staged_update("second");
        let mut forged = first.clone();
        forged.url = "https://example.test/releases/other".into();
        assert!(same_update(&first, &first));
        assert!(!same_update(&first, &second));
        assert!(!same_update(&first, &forged));
    }

    #[test]
    fn late_download_callbacks_cannot_regress_applying_transaction() {
        let mut preparing = staged_update("monotonic");
        preparing.phase = StagedUpdatePhase::Preparing;
        preparing.fingerprint.clear();
        let mut state = UpdateState::default();
        begin_staged_update_in_state(&mut state, &preparing).unwrap();

        let mut ready = preparing.clone();
        ready.phase = StagedUpdatePhase::Ready;
        ready.fingerprint = "sha256-tree-v1:ready".into();
        promote_staged_update_in_state(&mut state, &ready).unwrap();

        let mut applying = ready.clone();
        applying.phase = StagedUpdatePhase::Applying;
        applying.swap_app = Some(PathBuf::from(
            "/Applications/.Smelt.smelt-update-monotonic.app",
        ));
        state.staged = Some(applying.clone());

        assert!(promote_staged_update_in_state(&mut state, &ready).is_err());
        assert!(!clear_preparing_update_in_state(&mut state, &preparing));
        assert_eq!(state.staged, Some(applying));
    }

    #[test]
    fn critical_update_state_write_reports_failure() {
        let path = PathBuf::from("/dev/null/update-state.json");
        let error = save_update_state_locked(&path, &UpdateState::default()).unwrap_err();
        assert!(error.to_string().contains("保存更新状态失败"));
    }

    #[test]
    fn critical_update_state_read_rejects_corrupt_snapshot() {
        let error = state_from_snapshot(smelt_store::UpdateStateSnapshot {
            current_url: None,
            staged_json: Some(b"{not-json".to_vec()),
            cleanup_app: None,
        })
        .err()
        .expect("corrupt snapshot must fail");

        assert!(error.to_string().contains("解析暂存更新失败"));
    }

    #[test]
    fn bundle_fingerprint_covers_resources_not_just_main_executable() {
        let root = test_root("bundle-fingerprint");
        let app = fake_current_app(&root, b"same-main-binary");
        let resources = app.join("Contents/Resources");
        std::fs::create_dir_all(&resources).unwrap();
        std::fs::write(resources.join("release"), b"first").unwrap();
        let first = app_fingerprint(&app).unwrap();

        std::fs::write(resources.join("release"), b"second").unwrap();
        let second = app_fingerprint(&app).unwrap();

        assert_ne!(first, second);
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn bundle_fingerprint_is_independent_of_install_path() {
        let root = test_root("bundle-fingerprint-path");
        let first = fake_current_app(&root.join("first"), b"same-main-binary");
        let second = fake_current_app(&root.join("second"), b"same-main-binary");
        for app in [&first, &second] {
            let resources = app.join("Contents/Resources");
            std::fs::create_dir_all(&resources).unwrap();
            std::fs::write(resources.join("release"), b"same-resource").unwrap();
        }

        assert_eq!(
            app_fingerprint(&first).unwrap(),
            app_fingerprint(&second).unwrap()
        );
        std::fs::remove_dir_all(root).unwrap();
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn macos_bundle_validation_checks_identity_and_signed_resources() {
        let root = test_root("signed-bundle");
        let app = signed_test_app(&root.join("valid"), super::BUNDLE_ID);

        let fingerprint = super::validate_app_bundle_against(&app, &app).unwrap();
        assert!(fingerprint.starts_with(super::BUNDLE_FINGERPRINT_VERSION));

        std::fs::write(app.join("Contents/Resources/release"), b"tampered-resource").unwrap();
        let signature_error = super::validate_app_bundle_against(&app, &app).unwrap_err();
        assert!(signature_error.to_string().contains("签名校验失败"));

        let missing_installer = signed_test_app(&root.join("missing-installer"), super::BUNDLE_ID);
        std::fs::remove_file(
            missing_installer
                .join("Contents/MacOS")
                .join(super::INSTALLER_NAME),
        )
        .unwrap();
        let helper_error =
            super::validate_app_bundle_against(&missing_installer, &missing_installer).unwrap_err();
        assert!(helper_error.to_string().contains("缺少必要程序"));

        let wrong_identity = signed_test_app(&root.join("wrong-id"), "com.example.not-smelt");
        let identity_error = super::validate_app_bundle_against(&wrong_identity, &app).unwrap_err();
        assert!(identity_error.to_string().contains("Bundle ID 不匹配"));

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
        assert!(signer_error.to_string().contains("签发身份"));
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn recovery_commits_completed_swap_even_when_staged_cache_is_missing() {
        let root = test_root("commit-after-swap");
        let app = fake_current_app(&root, b"new-version");
        let swap = fake_app(
            root.join(".Smelt.smelt-update-completed.app"),
            b"old-version",
        );
        let state_path = root.join("update-state.json");
        let mut pending = staged_update("completed");
        pending.phase = StagedUpdatePhase::Applying;
        pending.staged_app = root.join("already-removed.app");
        pending.fingerprint = app_fingerprint(&app).unwrap();
        pending.swap_app = Some(swap.clone());
        let state = UpdateState {
            current_url: Some("https://example.test/old".into()),
            staged: Some(pending.clone()),
            cleanup_app: None,
        };

        let recovered = reconcile_update_state_locked(&state_path, state, &app).unwrap();

        assert!(recovered.is_none());
        let state = load_update_state_locked(&state_path).unwrap();
        assert_eq!(state.current_url.as_deref(), Some(pending.url.as_str()));
        assert!(state.staged.is_none());
        assert!(!swap.exists());
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn applying_recovery_only_removes_a_verified_unswapped_candidate() {
        let root = test_root("classify-before-swap");
        let app = fake_current_app(&root, b"old-version");
        let swap = fake_app(
            root.join(".Smelt.smelt-update-before-swap.app"),
            b"new-version",
        );
        let mut pending = staged_update("before-swap");
        pending.phase = StagedUpdatePhase::Applying;
        pending.fingerprint = app_fingerprint(&swap).unwrap();
        pending.swap_app = Some(swap.clone());

        assert_eq!(
            classify_applying_recovery(&pending, &app).unwrap(),
            ApplyingRecoveryState::NotSwapped
        );
        assert!(swap.is_dir());
        std::fs::remove_dir_all(root).unwrap();
    }

    #[cfg(any(target_os = "macos", target_os = "linux"))]
    #[test]
    fn applying_recovery_tracks_the_atomic_exchange_boundary() {
        let root = test_root("classify-atomic-swap");
        let app = fake_current_app(&root, b"old-version");
        let swap = fake_app(
            root.join(".Smelt.smelt-update-atomic-boundary.app"),
            b"new-version",
        );
        let mut pending = staged_update("atomic-boundary");
        pending.phase = StagedUpdatePhase::Applying;
        pending.fingerprint = app_fingerprint(&swap).unwrap();
        pending.swap_app = Some(swap.clone());

        assert_eq!(
            classify_applying_recovery(&pending, &app).unwrap(),
            ApplyingRecoveryState::NotSwapped
        );
        swap_app_bundles(&app, &swap).unwrap();
        assert_eq!(
            classify_applying_recovery(&pending, &app).unwrap(),
            ApplyingRecoveryState::Installed
        );
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn applying_recovery_preserves_both_apps_when_exchange_is_ambiguous() {
        let root = test_root("ambiguous-swap");
        let app = fake_current_app(&root, b"unexpected-current");
        let swap = fake_app(
            root.join(".Smelt.smelt-update-ambiguous.app"),
            b"previous-version",
        );
        let candidate = fake_app(root.join("candidate.app"), b"expected-new-version");
        let mut pending = staged_update("ambiguous");
        pending.phase = StagedUpdatePhase::Applying;
        pending.fingerprint = app_fingerprint(&candidate).unwrap();
        pending.swap_app = Some(swap.clone());

        let error = classify_applying_recovery(&pending, &app).unwrap_err();

        assert!(error.to_string().contains("保留现场"));
        assert!(app.is_dir());
        assert!(swap.is_dir());
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn applying_recovery_preserves_swap_when_candidate_cannot_be_verified() {
        let root = test_root("missing-applying-candidate");
        let app = fake_current_app(&root, b"current-version");
        let swap = fake_app(
            root.join(".Smelt.smelt-update-missing-candidate.app"),
            b"previous-version",
        );
        let state_path = root.join("update-state.json");
        let mut pending = staged_update("missing-candidate");
        pending.phase = StagedUpdatePhase::Applying;
        pending.fingerprint.clear();
        pending.staged_app = root.join("missing.app");
        pending.swap_app = Some(swap.clone());
        let state = UpdateState {
            current_url: Some("https://example.test/current".into()),
            staged: Some(pending),
            cleanup_app: None,
        };

        let error = reconcile_update_state_locked(&state_path, state, &app).unwrap_err();

        assert!(error.to_string().contains("保留交换现场"));
        assert!(app.is_dir());
        assert!(swap.is_dir());
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn recovery_discards_unverifiable_ready_job_instead_of_getting_stuck() {
        let root = test_root("discard-invalid-ready");
        let app = fake_current_app(&root, b"old-version");
        let state_path = root.join("update-state.json");
        let mut pending = staged_update("missing");
        pending.staged_app = root.join("missing.app");
        let state = UpdateState {
            current_url: Some("https://example.test/current".into()),
            staged: Some(pending),
            cleanup_app: None,
        };

        let recovered = reconcile_update_state_locked(&state_path, state, &app).unwrap();

        assert!(recovered.is_none());
        let state = load_update_state_locked(&state_path).unwrap();
        assert_eq!(
            state.current_url.as_deref(),
            Some("https://example.test/current")
        );
        assert!(state.staged.is_none());
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn recovery_never_deletes_unregistered_cleanup_path() {
        let root = test_root("reject-cleanup-path");
        let app = fake_current_app(&root, b"current-version");
        let unrelated = root.join("do-not-delete.app");
        std::fs::create_dir_all(&unrelated).unwrap();
        let state_path = root.join("update-state.json");
        let state = UpdateState {
            current_url: None,
            staged: None,
            cleanup_app: Some(unrelated.clone()),
        };

        save_update_state_locked(&state_path, &state).unwrap();
        let recovered = reconcile_update_state_locked(&state_path, state, &app).unwrap();

        assert!(recovered.is_none());
        assert!(unrelated.is_dir());
        assert_eq!(
            load_update_state_locked(&state_path).unwrap().cleanup_app,
            Some(unrelated.clone())
        );
        std::fs::remove_dir_all(root).unwrap();
    }

    #[cfg(any(target_os = "macos", target_os = "linux"))]
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

    /// 老版本的状态文档没有 staged 字段，反序列化必须回退成"没有暂存更新"，
    /// 不能因为新增字段把旧用户卡在启动补装流程里。
    #[test]
    fn update_state_deserializes_without_staged() {
        let state: UpdateState =
            serde_json::from_str(r#"{"current_url":"https://example.test/a.zip"}"#).unwrap();
        assert_eq!(
            state.current_url.as_deref(),
            Some("https://example.test/a.zip")
        );
        assert!(state.staged.is_none());
        assert!(state.cleanup_app.is_none());

        let empty: UpdateState = serde_json::from_str("{}").unwrap();
        assert!(empty.current_url.is_none());
        assert!(empty.staged.is_none());
        assert!(empty.cleanup_app.is_none());
    }
}
