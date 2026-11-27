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

/// 自动更新走不通时让用户手动下载的地方。
const RELEASES_PAGE: &str = "https://github.com/smelt-ai/smelt/releases";

/// 打包时可选指定的更新清单地址，`{channel}` 换成通道名（`dev` / `prod`）。设了就不查
/// GitHub Releases，改读这份清单：自己分发安装包的打包方用它把更新指向自己的服务器。
/// 清单格式是 `{"version": "0.9.0", "url": "https://…/Smelt.app.zip"}`，`url` 指向本平台的
/// 更新包，`version` 必填，是包里那次发版的版本号（`smelt_version::RELEASE` 的写法）：比装着的
/// 新才算更新。macOS 和 Windows 各编各的包，打包时各给各的清单地址。
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

    /// 填进更新清单地址 `{channel}` 的名字，和存盘的写法一致。
    const fn slug(self) -> &'static str {
        match self {
            Self::Dev => "dev",
            Self::Prod => "prod",
        }
    }
}

/// 更新源给出的候选更新。`version` 比装着的新（见 [`installed_version`]）才算更新。
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
    /// 安装程序没拉起来，App 没退出；下好的包还在，可以直接再装一次。
    InstallFailed(StagedUpdate),
    /// UI 只保存可直接展示的失败类别；底层网络、文件系统和协议错误写入 app.log，
    /// 避免把内部 URL 或 reqwest 文本直接呈现给用户。
    Failed(UpdateFailure),
}

/// 更新链路中面向用户的失败类别。给人看的字由界面按类别翻（更新插件）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum UpdateFailure {
    Check,
    Download,
    /// App 被系统转移或放在只读位置，更新装不上；见 `app_location`。先移到「应用程序」，
    /// 不检查、不下载。
    Location,
    /// 不是发布包装好的（开发构建；macOS 不在 `.app` 里跑，Windows 不是安装程序装的）：
    /// 没有版本号可比、也没有能整个换掉的安装，不检查、不下载。
    NotInstalled,
}

impl UpdateStatus {
    pub fn can_check(&self) -> bool {
        matches!(
            self,
            Self::Idle
                | Self::UpToDate
                | Self::Available { .. }
                | Self::ReadyToInstall(_)
                | Self::InstallFailed(_)
                | Self::Failed(UpdateFailure::Check | UpdateFailure::Download)
        )
    }

    /// 自动更新卡住时给的退路：去发布页手动下载安装。位置不对的有自己的「移到应用程序」，
    /// 不给这个。
    pub fn offers_manual_download(&self) -> bool {
        matches!(
            self,
            Self::InstallFailed(_)
                | Self::Failed(
                    UpdateFailure::Check | UpdateFailure::Download | UpdateFailure::NotInstalled
                )
        )
    }

    /// 下好、等着装的包。
    pub fn ready_update(&self) -> Option<&StagedUpdate> {
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

/// 装着的是哪次发版：编进程序的 `smelt_version::RELEASE`。只认程序自己，不另外记：别处记的
/// 「装的是哪个」和磁盘上真正的 App 会对不上（比如手动拖装了别的包）。开发构建没有发版号，
/// 不参与更新（`can_self_update` 为 false）。
pub fn installed_version() -> anyhow::Result<semver::Version> {
    let release = smelt_version::RELEASE.ok_or_else(|| {
        anyhow::anyhow!("development builds have no release number and don't auto-update")
    })?;
    semver::Version::parse(release)
        .with_context(|| format!("the built-in release number isn't semver: {release}"))
}

/// 候选比装着的新才算更新。认不出版本号的不算：不知道它新还是旧。同一个版本号下的不同
/// 构建（`+g<提交>`）不算新：semver 比先后不看构建信息。
fn is_newer(candidate: &UpdateCandidate, installed: &semver::Version) -> bool {
    semver::Version::parse(candidate.version.trim())
        .is_ok_and(|candidate| candidate.cmp_precedence(installed).is_gt())
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
    installed: &semver::Version,
    auto_install: bool,
) -> CheckOutcome {
    if !is_newer(&candidate, installed) {
        return CheckOutcome::NoUpdate;
    }
    if auto_install {
        CheckOutcome::Download(candidate)
    } else {
        CheckOutcome::Notify(candidate)
    }
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
        .with_context(|| {
            format!(
                "the {} channel has no release with an uploaded package",
                channel.slug()
            )
        })
}

/// 手动下载页。生产版直接落到最新正式版；内测版看列表（最新的可能是预发布）。打包时
/// 指定了更新清单的不知道打包方的下载页，返回 `None`。
pub fn manual_download_page(channel: UpdateChannel) -> Option<String> {
    if UPDATE_MANIFEST_URL.is_some() {
        return None;
    }
    Some(match channel {
        UpdateChannel::Dev => RELEASES_PAGE.to_string(),
        UpdateChannel::Prod => format!("{RELEASES_PAGE}/latest"),
    })
}

fn manifest_url(template: &str, channel: UpdateChannel) -> String {
    template.replace("{channel}", channel.slug())
}

#[derive(serde::Deserialize)]
struct UpdateManifest {
    version: String,
    url: String,
}

/// 读打包时指定的更新清单。清单只维护该通道当前指向的包。版本号认不出来就是清单写错了，
/// 报检查失败，不当成「已是最新」：那样清单坏了谁也发现不了。
async fn fetch_manifest(manifest_url: &str) -> anyhow::Result<UpdateCandidate> {
    let body = fetch_metadata(manifest_url, "application/json").await?;
    parse_manifest(&body).with_context(|| format!("{manifest_url} isn't a valid update manifest"))
}

fn parse_manifest(body: &[u8]) -> anyhow::Result<UpdateCandidate> {
    let manifest: UpdateManifest = serde_json::from_slice(body)?;
    let version = manifest.version.trim().to_string();
    semver::Version::parse(&version)
        .with_context(|| format!("the version isn't a release number: {version:?}"))?;
    let url = manifest.url.trim().to_string();
    validate_download_url(&url)?;
    Ok(UpdateCandidate { version, url })
}

async fn fetch_github_releases(channel: UpdateChannel) -> anyhow::Result<Vec<UpdateCandidate>> {
    let body = fetch_metadata(RELEASES_API, "application/vnd.github+json").await?;
    let releases: Vec<GithubRelease> =
        serde_json::from_slice(&body).context("the GitHub release list is malformed")?;
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
        .map_err(|error| anyhow::anyhow!("invalid update url: {download_url} ({error})"))?;
    if parsed.scheme() != "https" {
        anyhow::bail!("the update url must use HTTPS: {download_url}");
    }
    Ok(())
}

/// 连上服务器最多等多久。
const CONNECT_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(15);
/// 两次收到数据之间最多等多久：大包下得慢不算超时，连接卡死才算。没有它，卡住的下载
/// 会一直停在「下载中」，检查和重试都按不了，只能重启 App。
const READ_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(60);
/// 清单、发布列表、校验和这类小请求，整个请求的时限。
const METADATA_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(60);
const MAX_REDIRECTS: usize = 10;
/// 小请求的响应上限。GitHub 发布列表（20 条带附件）也就几百 KB。
const MAX_METADATA_BYTES: u64 = 8 * 1024 * 1024;
/// 更新包上限。现在的包一两百 MB，这里只防服务端发个没完把磁盘写满。
const MAX_PACKAGE_BYTES: u64 = 2 * 1024 * 1024 * 1024;

/// 更新器所有请求共用：连接和读都有超时，跳转只跟 HTTPS。不限主机：打包方可以用
/// 更新清单把更新指向自己的服务器，包的来源由平台校验把关（macOS 比签名身份）。
fn http_client() -> anyhow::Result<reqwest::Client> {
    reqwest::Client::builder()
        .user_agent("smelt-updater")
        .connect_timeout(CONNECT_TIMEOUT)
        .read_timeout(READ_TIMEOUT)
        .redirect(reqwest::redirect::Policy::custom(|attempt| {
            if attempt.previous().len() >= MAX_REDIRECTS {
                attempt.error("the update URL redirected too many times")
            } else if attempt.url().scheme() != "https" {
                attempt.error("the update URL redirected to a non-HTTPS address")
            } else {
                attempt.follow()
            }
        }))
        .build()
        .context("couldn't create the HTTP client for updates")
}

/// 取一个小响应（清单、发布列表、校验和）：整个请求限时，响应限大小。
async fn fetch_metadata(url: &str, accept: &str) -> anyhow::Result<Vec<u8>> {
    validate_download_url(url)?;
    let mut resp = http_client()?
        .get(url)
        .header(reqwest::header::ACCEPT, accept)
        .timeout(METADATA_TIMEOUT)
        .send()
        .await?
        .error_for_status()?;
    validate_download_url(resp.url().as_str())?;
    if resp
        .content_length()
        .is_some_and(|len| len > MAX_METADATA_BYTES)
    {
        anyhow::bail!("the response from {url} is too large");
    }
    let mut body = Vec::new();
    while let Some(chunk) = resp.chunk().await? {
        body.extend_from_slice(&chunk);
        if body.len() as u64 > MAX_METADATA_BYTES {
            anyhow::bail!("the response from {url} is too large");
        }
    }
    Ok(body)
}

/// 开始下载更新包。不设整体时限（包大、网慢时要下很久），靠读超时发现卡死；边读边用
/// [`ensure_package_size`] 守着大小上限。`kind` 只用在报错里。
async fn open_package_download(url: &str, kind: &str) -> anyhow::Result<reqwest::Response> {
    validate_download_url(url)?;
    let resp = http_client()?.get(url).send().await?.error_for_status()?;
    validate_download_url(resp.url().as_str())?;
    if resp
        .headers()
        .get(reqwest::header::CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .is_some_and(|value| value.to_ascii_lowercase().contains("text/html"))
    {
        anyhow::bail!("the update URL returned HTML, not the {kind}: {url}");
    }
    if resp
        .content_length()
        .is_some_and(|len| len > MAX_PACKAGE_BYTES)
    {
        anyhow::bail!("the update package exceeds the size limit: {url}");
    }
    Ok(resp)
}

fn ensure_package_size(received: u64) -> anyhow::Result<()> {
    if received > MAX_PACKAGE_BYTES {
        anyhow::bail!("the update package exceeded the size limit; download stopped");
    }
    Ok(())
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
        CheckOutcome, GithubAsset, GithubRelease, UpdateCandidate, UpdateChannel,
        decide_check_outcome, is_newer, manifest_url, parse_manifest, pick_releases,
        validate_download_url,
    };

    fn versioned(version: &str) -> UpdateCandidate {
        UpdateCandidate {
            version: version.into(),
            url: format!("https://example.test/{version}/Smelt.app.zip"),
        }
    }

    fn installed(version: &str) -> semver::Version {
        semver::Version::parse(version).unwrap()
    }

    /// 关掉自动更新后**绝不能**再走下载分支——这是这个开关唯一的实质承诺。
    #[test]
    fn auto_install_off_only_notifies_never_downloads() {
        let outcome = decide_check_outcome(versioned("0.6.16"), &installed("0.6.15"), false);
        assert_eq!(outcome, CheckOutcome::Notify(versioned("0.6.16")));
    }

    #[test]
    fn auto_install_on_downloads_without_asking() {
        let outcome = decide_check_outcome(versioned("0.6.16"), &installed("0.6.15"), true);
        assert_eq!(outcome, CheckOutcome::Download(versioned("0.6.16")));
    }

    /// 开关只管"发现新版之后怎么办"，没有新版时两边都该是同一个结论：没得更新。
    /// 别让关掉开关变成"永远显示有新版"。
    #[test]
    fn same_release_is_no_update_regardless_of_switch() {
        for auto_install in [true, false] {
            let outcome =
                decide_check_outcome(versioned("0.6.15"), &installed("0.6.15"), auto_install);
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
    fn manifest_gives_the_version_and_package() {
        let candidate = parse_manifest(
            br#"{"version":" 2026.1102.0-rc.1 ","url":" https://example.test/releases/35039 "}"#,
        )
        .unwrap();
        assert_eq!(
            candidate,
            UpdateCandidate {
                version: "2026.1102.0-rc.1".into(),
                url: "https://example.test/releases/35039".into(),
            }
        );
    }

    /// 清单少了版本号、写成占位的 0、或者不是发版号，都是清单写错了：报出来，不当成没有更新。
    #[test]
    fn manifest_without_a_release_number_is_rejected() {
        for body in [
            r#"{"url":"https://example.test/releases/35039"}"#,
            r#"{"version":0,"url":"https://example.test/releases/35039"}"#,
            r#"{"version":"0","url":"https://example.test/releases/35039"}"#,
            r#"{"version":"latest","url":"https://example.test/releases/35039"}"#,
            r#"{"version":"0.9.0","url":"http://example.test/releases/35039"}"#,
        ] {
            assert!(parse_manifest(body.as_bytes()).is_err(), "{body}");
        }
    }

    /// 认版本号：同一个版本换了下载地址（比如改走镜像）也不算新版。
    #[test]
    fn version_identity_ignores_the_download_url() {
        let same = UpdateCandidate {
            version: "0.6.15".into(),
            url: "https://mirror.test/a".into(),
        };
        assert!(!is_newer(&same, &installed("0.6.15")));
        assert!(is_newer(&versioned("0.6.16"), &installed("0.6.15")));
    }

    /// 装着内测版时切回生产通道，生产通道最新的比它旧：不能装回去。
    #[test]
    fn version_older_than_installed_is_not_an_update() {
        let current = installed("2026.1027.1-rc.1");
        assert!(!is_newer(&versioned("2026.1026.3"), &current));
        assert!(!is_newer(&versioned("2026.1027.1-rc.1"), &current));
        assert!(is_newer(&versioned("2026.1027.1-rc.2"), &current));
        assert!(is_newer(&versioned("2026.1027.1"), &current));
        assert_eq!(
            decide_check_outcome(versioned("2026.1026.3"), &current, true),
            CheckOutcome::NoUpdate
        );
    }

    /// 同一个版本号下的另一个构建不算新：semver 比先后不看 `+` 后的构建信息。
    #[test]
    fn build_metadata_is_not_newer() {
        assert!(!is_newer(
            &versioned("2026.1102.0+g32c3172"),
            &installed("2026.1102.0")
        ));
    }

    /// 预发布号按数字比：rc.10 比 rc.9 新。
    #[test]
    fn prerelease_numbers_compare_numerically() {
        assert!(is_newer(
            &versioned("2026.1027.0-rc.10"),
            &installed("2026.1027.0-rc.9")
        ));
    }

    #[test]
    fn unparsable_candidate_version_is_not_an_update() {
        assert!(!is_newer(
            &versioned("Smelt-latest"),
            &installed("2026.1027.0")
        ));
    }

    #[test]
    fn update_payload_url_requires_https() {
        assert!(validate_download_url("https://example.test/release.zip").is_ok());
        assert!(validate_download_url("http://example.test/release.zip").is_err());
        assert!(validate_download_url("file:///tmp/release.zip").is_err());
    }
}
