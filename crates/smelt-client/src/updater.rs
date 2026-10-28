//! 在线更新：查 GitHub Releases（或打包时指定的更新清单）、静默下载新版，再交给平台的
//! 安装方式换上去。macOS 下载 zip、退出时把 `.app` 换成新版本（`updater/macos.rs`）；
//! Windows 下载 Inno Setup 安装程序，退出后由它静默覆盖安装（`updater/windows.rs`）。
//! 通道、查新版、状态机两边共用，放在这里。
//!
//! 不碰 GPUI，纯文件/网络操作，方便独立验证。这里没有 tokio 运行时，`reqwest`
//! 得在临时 current-thread 运行时里 `block_on`（调用方负责套
//! `cx.background_executor().spawn`）。

use anyhow::Context as _;

#[cfg(target_os = "macos")]
mod macos;
#[cfg(target_os = "macos")]
use macos as platform;
#[cfg(target_os = "macos")]
pub use macos::*;

#[cfg(windows)]
mod windows;
#[cfg(windows)]
use windows as platform;
#[cfg(windows)]
pub use windows::*;

/// 查列表而不是 `releases/latest`：内测通道要能看到预发布，两个通道走同一条路径。
const RELEASES_API: &str = "https://api.github.com/repos/smelt-ai/smelt/releases?per_page=20";

/// 打包时可选指定的更新清单地址，`{channel}` 换成通道名（`dev` / `prod`）。设了就不查
/// GitHub Releases，改读这份清单：自己分发安装包的打包方用它把更新指向自己的服务器。
/// 清单格式是 `{"version": "0.9.0", "url": "https://…/Smelt.app.zip"}`，`url` 指向本平台的
/// 更新包。macOS 和 Windows 各编各的包，打包时各给各的清单地址。
const UPDATE_MANIFEST_URL: Option<&str> = match option_env!("SMELT_UPDATE_MANIFEST_URL") {
    Some(url) if !url.is_empty() => Some(url),
    _ => None,
};

/// 下载进度上报节流：攒够这么多字节才推一次事件，别让每个 chunk 都触发一次重绘。
const PROGRESS_REPORT_STEP: u64 = 512 * 1024;

/// 更新通道。从 GitHub Releases 取时，区别只在认不认预发布（tag 带 `-` 的 rc 版）；
/// 打包时指定了更新清单的，两个通道各读各的清单。
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum UpdateChannel {
    /// 内测包：最新的发布，预发布也算。
    Dev,
    /// 生产包：只认正式发布。
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

    /// 填进更新清单地址 `{channel}` 的名字，和存盘的写法一致。
    const fn slug(self) -> &'static str {
        match self {
            Self::Dev => "dev",
            Self::Prod => "prod",
        }
    }
}

/// 更新源给出的候选更新。是不是本机已经装着的那个，由 [`InstalledRelease`] 判断。
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
    /// 保留 `url` 是因为下载仍以检查时拿到的地址为准，不能等按下按钮再查一次
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
    /// 包下完了，正在解压、校验、准备，耗时不可测。
    Installing {
        version: String,
    },
    /// 正在派生安装程序；派生成功后 GUI 才会正常退出。
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
    /// App 被系统转移或放在只读位置，更新装不上；见 `app_location`。先移到「应用程序」，
    /// 不检查、不下载。
    Location,
    /// Windows 上不是安装程序装的（开发构建、拷出来的 exe）：没有能覆盖的安装，不检查、
    /// 不下载。
    NotInstalled,
}

impl UpdateFailure {
    pub const fn title(self) -> &'static str {
        match self {
            Self::Check => "检查更新失败",
            Self::Download => "下载更新失败",
            Self::Recovery => "恢复更新失败",
            Self::Location => "需要先移到「应用程序」",
            Self::NotInstalled => "这个 Smelt 不能自动更新",
        }
    }

    pub const fn detail(self) -> &'static str {
        match self {
            Self::Check => "暂时无法连接更新服务，请检查网络后重试。",
            Self::Download => "更新包下载或准备失败，请检查网络和可用存储空间后重试。",
            Self::Recovery => "上次更新事务尚未恢复，请重试；现有安装不会被覆盖。",
            Self::Location => {
                "Smelt 没放进「应用程序」就打开了，macOS 让它在只读位置运行，更新装不上。移过去后会自动重新打开，后台会话不受影响。"
            }
            Self::NotInstalled => {
                "它不是安装程序装的（比如开发构建）。从 Releases 下载安装程序装好后，就能在这里更新。"
            }
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

/// 本机装着的是哪个发布，用来判断更新源给的候选是不是新的。
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum InstalledRelease {
    /// macOS 记的是装上它的那个更新包地址：每个 Release 的下载地址都带 tag，换了发布
    /// 就换了 URL，同一个版本号重发的内测包也认得出来。
    Url(String),
    /// Windows 认编进程序的发版号（`smelt_version::RELEASE`）：安装程序装的就是那次发版，
    /// 没有另外记录的地方。只有比它新的才算更新：切回生产通道、或者最新的发布还没传完
    /// 退到上一个时，候选可能比装着的旧，不能装回去（旧版读不了新版升级过的客户端库）。
    Version(semver::Version),
    /// 没有可比较的记录：迁移前的旧安装、手工安装的首个包、开发构建。这时让 updater
    /// 下载一次，装好之后就有记录了，免得同版本内测包永远无法更新。
    Unknown,
}

impl InstalledRelease {
    /// 候选不用装：就是装着的这个，或者（按版本号认时）不比它新。
    fn covers(&self, candidate: &UpdateCandidate) -> bool {
        match self {
            Self::Url(url) => url.trim() == candidate.url.trim(),
            // 认不出版本号的候选也不装：不知道它新还是旧。
            Self::Version(installed) => semver::Version::parse(candidate.version.trim())
                .map_or(true, |candidate| candidate <= *installed),
            Self::Unknown => false,
        }
    }
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
    installed: &InstalledRelease,
    auto_install: bool,
) -> CheckOutcome {
    if installed.covers(&candidate) {
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
    installed: &InstalledRelease,
    failed: &StagedUpdate,
    auto_install: bool,
) -> Option<CheckOutcome> {
    if candidate.url.trim() == failed.url.trim() {
        return None;
    }
    Some(decide_check_outcome(candidate, installed, auto_install))
}

#[derive(serde::Deserialize)]
struct GithubRelease {
    tag_name: String,
    #[serde(default)]
    draft: bool,
    #[serde(default)]
    prerelease: bool,
    /// 草稿没有发布时间；排除草稿后都有值。
    #[serde(default)]
    published_at: Option<String>,
    #[serde(default)]
    assets: Vec<GithubAsset>,
}

impl GithubRelease {
    /// tag 去掉 `v`，即这次发版的版本号。
    fn version(&self) -> &str {
        self.tag_name.trim().trim_start_matches('v')
    }

    fn asset(&self, name: &str) -> Option<&GithubAsset> {
        self.assets.iter().find(|asset| asset.name == name)
    }
}

#[derive(serde::Deserialize)]
struct GithubAsset {
    name: String,
    browser_download_url: String,
}

/// 查指定通道的发布，新的在前，每个给出版本展示文本和本平台更新包的下载地址。更新
/// 清单只给一个。平台的 `fetch_latest` 从里面挑能装的。
async fn fetch_candidates(channel: UpdateChannel) -> anyhow::Result<Vec<UpdateCandidate>> {
    match UPDATE_MANIFEST_URL {
        Some(template) => Ok(vec![
            fetch_manifest(&manifest_url(template, channel)).await?,
        ]),
        None => fetch_github_releases(channel).await,
    }
}

/// `fetch_candidates` 里最新的那个。
async fn fetch_candidate(channel: UpdateChannel) -> anyhow::Result<UpdateCandidate> {
    fetch_candidates(channel)
        .await?
        .into_iter()
        .next()
        .with_context(|| format!("没有已传好更新包的{}", channel.label()))
}

fn manifest_url(template: &str, channel: UpdateChannel) -> String {
    template.replace("{channel}", channel.slug())
}

#[derive(serde::Deserialize)]
struct UpdateManifest {
    #[serde(default)]
    version: Option<serde_json::Value>,
    url: String,
}

/// 读打包时指定的更新清单。清单只维护该通道当前指向的 ZIP。
async fn fetch_manifest(manifest_url: &str) -> anyhow::Result<UpdateCandidate> {
    validate_download_url(manifest_url)?;
    let resp = reqwest::Client::new()
        .get(manifest_url)
        .header("User-Agent", "smelt-updater")
        .header("Accept", "application/json")
        .send()
        .await?
        .error_for_status()?;
    validate_download_url(resp.url().as_str())?;
    let manifest: UpdateManifest = resp.json().await?;
    let download_url = manifest.url.trim().to_string();
    if download_url.is_empty() {
        anyhow::bail!("{manifest_url} 缺少 url");
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

fn manifest_version_text(value: &serde_json::Value) -> Option<String> {
    match value {
        serde_json::Value::String(value) => Some(value.trim().to_string()),
        serde_json::Value::Number(value) => Some(value.to_string()),
        _ => None,
    }
}

/// 清单没给版本号时，拿下载地址最后一段当展示文本。
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

async fn fetch_github_releases(channel: UpdateChannel) -> anyhow::Result<Vec<UpdateCandidate>> {
    let releases: Vec<GithubRelease> = reqwest::Client::new()
        .get(RELEASES_API)
        .header("User-Agent", "smelt-updater")
        .header("Accept", "application/vnd.github+json")
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;
    let candidates = pick_releases(&releases, channel, platform::update_asset);
    for candidate in &candidates {
        validate_download_url(&candidate.url)?;
    }
    Ok(candidates)
}

/// 符合通道、且已经传上更新包的发布，按发布时间从新到旧排。不靠列表顺序：GitHub 按
/// tag 所在提交的时间排，rc 和正式版打在同一个提交上时分不出先后。更新包还没传完的
/// 发布先跳过，等下次检查。哪个文件是更新包由平台的 `update_asset` 认。
fn pick_releases(
    releases: &[GithubRelease],
    channel: UpdateChannel,
    update_asset: fn(&GithubRelease) -> Option<&GithubAsset>,
) -> Vec<UpdateCandidate> {
    let mut picked: Vec<_> = releases
        .iter()
        .filter(|release| !release.draft)
        .filter(|release| channel == UpdateChannel::Dev || !release.prerelease)
        .filter_map(|release| Some((release, update_asset(release)?)))
        .collect();
    picked.sort_by(|(left, _), (right, _)| right.published_at.cmp(&left.published_at));
    picked
        .into_iter()
        .map(|(release, asset)| UpdateCandidate {
            version: release.version().to_string(),
            url: asset.browser_download_url.trim().to_string(),
        })
        .collect()
}

fn validate_download_url(download_url: &str) -> anyhow::Result<()> {
    let parsed = url::Url::parse(download_url)
        .map_err(|error| anyhow::anyhow!("更新 url 无效：{download_url}（{error}）"))?;
    if parsed.scheme() != "https" {
        anyhow::bail!("更新 url 必须使用 HTTPS：{download_url}");
    }
    Ok(())
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

/// 停掉本机 smeltd，并等到进程退出。安装程序替换文件之前调用。
///
/// 连不上表示没有守护在跑。进程仍活着则返回 false：macOS 的安装器换包前会再停一次；
/// Windows 的安装程序会关掉还占着要替换文件的进程。
pub fn shutdown_local_daemon() -> bool {
    crate::daemon_stop::stop_local_daemon()
}

#[cfg(test)]
mod tests {
    use super::{
        CheckOutcome, GithubAsset, GithubRelease, InstalledRelease, UpdateCandidate, UpdateChannel,
        UpdateFailure, UpdateManifest, decide_check_outcome, manifest_url, manifest_version_text,
        pick_releases, release_label_from_url, validate_download_url,
    };

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
            &InstalledRelease::Url("https://example.com/old.zip".into()),
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
            &InstalledRelease::Url("https://example.com/old.zip".into()),
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
                &InstalledRelease::Url("https://example.com/same.zip".into()),
                auto_install,
            );
            assert_eq!(outcome, CheckOutcome::NoUpdate, "auto={auto_install}");
        }
    }

    fn releases(json: &str) -> Vec<GithubRelease> {
        serde_json::from_str(json).unwrap()
    }

    /// 测试里认 macOS 的更新包名，平台的 `update_asset` 各自测。
    fn app_zip(release: &GithubRelease) -> Option<&GithubAsset> {
        release.asset("Smelt.app.zip")
    }

    const RELEASES: &str = r#"[
        {"tag_name":"v0.9.0-rc.5","prerelease":true,"published_at":"2026-10-21T00:00:00Z",
         "assets":[{"name":"Smelt.dmg","browser_download_url":"https://example.test/rc5/Smelt.dmg"}]},
        {"tag_name":"v0.9.0","prerelease":false,"published_at":"2026-10-20T00:00:00Z",
         "assets":[{"name":"Smelt.app.zip","browser_download_url":"https://example.test/090/Smelt.app.zip"}]},
        {"tag_name":"v0.9.0-rc.4","prerelease":true,"published_at":"2026-10-19T00:00:00Z",
         "assets":[{"name":"Smelt.app.zip","browser_download_url":"https://example.test/rc4/Smelt.app.zip"}]},
        {"tag_name":"v1.0.0","draft":true,"published_at":null,
         "assets":[{"name":"Smelt.app.zip","browser_download_url":"https://example.test/100/Smelt.app.zip"}]}
    ]"#;

    #[test]
    fn channel_picks_latest_published_release_with_update_asset() {
        let releases = releases(RELEASES);
        // rc.5 还没传更新包、v1.0.0 是草稿，都不算；同一提交上的正式版晚于 rc.4 发布。
        assert_eq!(
            pick_releases(&releases, UpdateChannel::Dev, app_zip).first(),
            Some(&UpdateCandidate {
                version: "0.9.0".into(),
                url: "https://example.test/090/Smelt.app.zip".into(),
            })
        );
        assert_eq!(
            first_version(&releases, UpdateChannel::Prod),
            Some("0.9.0".into())
        );
    }

    fn first_version(releases: &[GithubRelease], channel: UpdateChannel) -> Option<String> {
        pick_releases(releases, channel, app_zip)
            .into_iter()
            .next()
            .map(|c| c.version)
    }

    /// 最新的发布装不了时（比如 Windows 的校验和清单还没重写），平台要能退到上一个。
    #[test]
    fn releases_are_listed_newest_first() {
        let versions: Vec<_> = pick_releases(&releases(RELEASES), UpdateChannel::Dev, app_zip)
            .into_iter()
            .map(|c| c.version)
            .collect();
        assert_eq!(versions, ["0.9.0", "0.9.0-rc.4"]);
    }

    #[test]
    fn prod_channel_ignores_prereleases() {
        let releases = releases(
            r#"[{"tag_name":"v0.9.0-rc.4","prerelease":true,"published_at":"2026-10-19T00:00:00Z",
                 "assets":[{"name":"Smelt.app.zip","browser_download_url":"https://example.test/rc4/Smelt.app.zip"}]}]"#,
        );
        assert_eq!(first_version(&releases, UpdateChannel::Prod), None);
        assert_eq!(
            first_version(&releases, UpdateChannel::Dev),
            Some("0.9.0-rc.4".into())
        );
    }

    #[test]
    fn manifest_url_names_the_channel() {
        let template = "https://example.test/update.{channel}.json";
        assert_eq!(
            manifest_url(template, UpdateChannel::Dev),
            "https://example.test/update.dev.json"
        );
        assert_eq!(
            manifest_url(template, UpdateChannel::Prod),
            "https://example.test/update.prod.json"
        );
    }

    #[test]
    fn manifest_accepts_numeric_placeholder_version() {
        let manifest: UpdateManifest =
            serde_json::from_str(r#"{"version":0,"url":"https://example.test/releases/35039"}"#)
                .unwrap();
        assert_eq!(
            manifest_version_text(manifest.version.as_ref().unwrap()),
            Some("0".into())
        );
        assert_eq!(release_label_from_url(&manifest.url), "35039");
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
        let installed = InstalledRelease::Url("https://example.test/a".into());
        assert!(installed.covers(&candidate(" https://example.test/a ")));
        assert!(!installed.covers(&candidate("https://example.test/b")));
        assert!(!InstalledRelease::Unknown.covers(&candidate("https://example.test/a")));
    }

    fn versioned(version: &str) -> UpdateCandidate {
        UpdateCandidate {
            version: version.into(),
            url: format!("https://example.test/{version}/setup.exe"),
        }
    }

    fn installed_version(version: &str) -> InstalledRelease {
        InstalledRelease::Version(semver::Version::parse(version).unwrap())
    }

    /// Windows 认版本号：同一个版本换了下载地址（比如改走镜像）也不算新版。
    #[test]
    fn version_identity_ignores_the_download_url() {
        let installed = installed_version("0.6.15");
        assert!(installed.covers(&candidate("https://mirror.test/a")));
        assert!(!installed.covers(&versioned("0.6.16")));
    }

    /// 装着内测版时切回生产通道，生产通道最新的比它旧：不能装回去。
    #[test]
    fn version_older_than_installed_is_not_an_update() {
        let installed = installed_version("2026.1027.1-rc.1");
        assert!(installed.covers(&versioned("2026.1026.3")));
        assert!(installed.covers(&versioned("2026.1027.1-rc.1")));
        assert!(!installed.covers(&versioned("2026.1027.1-rc.2")));
        assert!(!installed.covers(&versioned("2026.1027.1")));
        assert_eq!(
            decide_check_outcome(versioned("2026.1026.3"), &installed, true),
            CheckOutcome::NoUpdate
        );
    }

    /// 预发布号按数字比：rc.10 比 rc.9 新。
    #[test]
    fn prerelease_numbers_compare_numerically() {
        let installed = installed_version("2026.1027.0-rc.9");
        assert!(!installed.covers(&versioned("2026.1027.0-rc.10")));
    }

    #[test]
    fn unparsable_candidate_version_is_not_an_update() {
        assert!(installed_version("2026.1027.0").covers(&versioned("Smelt-latest")));
    }

    #[test]
    fn update_payload_url_requires_https() {
        assert!(validate_download_url("https://example.test/release.zip").is_ok());
        assert!(validate_download_url("http://example.test/release.zip").is_err());
        assert!(validate_download_url("file:///tmp/release.zip").is_err());
    }
}
