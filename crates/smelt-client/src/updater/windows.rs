//! Windows 的更新：下载 Inno Setup 安装程序，对着同一个 Release 的 `SHA256SUMS` 校验；
//! 应用时拉起它静默覆盖安装，桌面随即退出。安装程序先等桌面进程退出再换文件，装完按需
//! 重新打开 Smelt（`scripts/windows-installer.iss` 的 `/WAITPID`、`/RELAUNCH`）。
//!
//! 换文件、出错回滚是安装程序的事，这里不记持久化事务：下好的安装程序只在这次运行里
//! 等着装，重启后重新检查；同一个版本已经下好、校验过就不再下一遍。

use super::*;

use std::io::{Read, Write};
use std::path::{Path, PathBuf};

use anyhow::Context as _;
use sha2::{Digest, Sha256};

/// 发版时和安装程序传到同一个 Release 的校验和清单。
const CHECKSUMS_ASSET: &str = "SHA256SUMS";

/// Release 上的安装程序名，和 `release.yml` 的 windows 任务一致。
fn installer_asset_name(version: &str) -> String {
    format!("smelt-{version}-windows-x86_64-setup.exe")
}

/// Release 里的安装程序。同一个 Release 还得有校验和清单，不然下下来也没法校验。
pub(super) fn update_asset(release: &GithubRelease) -> Option<&GithubAsset> {
    release.asset(CHECKSUMS_ASSET)?;
    release.asset(&installer_asset_name(release.version()))
}

/// 查指定通道最新的发布，并确认它的校验和清单里已经有安装程序：发版流水线等所有产物
/// 都传完才把清单重写成全量的，在那之前这个发布还装不了。
pub async fn fetch_latest(channel: UpdateChannel) -> anyhow::Result<UpdateCandidate> {
    let candidate = fetch_candidate(channel).await?;
    expected_sha256(&candidate.url).await?;
    Ok(candidate)
}

/// 装着的就是编进程序的那次发版。开发构建没有发版号。
pub fn installed_release() -> anyhow::Result<InstalledRelease> {
    Ok(
        smelt_version::RELEASE.map_or(InstalledRelease::Unknown, |release| {
            InstalledRelease::Version(release.to_string())
        }),
    )
}

/// 下好、校验过的安装程序，等用户点「立即重启更新」或退出时拉起。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StagedUpdate {
    pub version: String,
    pub url: String,
    installer: PathBuf,
    sha256: String,
}

impl StagedUpdate {
    /// 安装尝试不跨进程记录（见模块说明），启动时恢复不出失败过的作业。
    pub fn installer_failed(&self) -> bool {
        false
    }
}

/// 已经拉起的安装程序。它的日志写在安装程序旁边，装失败时去那儿看。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct InstallerTicket {
    pid: u32,
    log: PathBuf,
}

impl std::fmt::Display for InstallerTicket {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "pid={} log={}", self.pid, self.log.display())
    }
}

/// 下载的安装程序放在 `%LOCALAPPDATA%\Smelt\update`，不进数据目录 `~\.smelt`。
fn download_dir() -> anyhow::Result<PathBuf> {
    Ok(dirs::cache_dir()
        .ok_or_else(|| anyhow::anyhow!("找不到系统缓存目录"))?
        .join("Smelt")
        .join("update"))
}

fn installer_path(dir: &Path, version: &str) -> PathBuf {
    dir.join(format!("smelt-{}-setup.exe", safe_component(version)))
}

/// 安装程序旁边那份 `SHA256SUMS` 里它的校验和。GitHub 的下载地址是
/// `…/releases/download/<tag>/<文件名>`，清单就在同一个目录；用更新清单自己分发的，
/// 也要把 `SHA256SUMS` 和安装程序放在一起。
async fn expected_sha256(installer_url: &str) -> anyhow::Result<String> {
    let url = url::Url::parse(installer_url)?;
    let file = url
        .path_segments()
        .and_then(|segments| segments.last())
        .filter(|name| !name.is_empty())
        .ok_or_else(|| anyhow::anyhow!("安装程序地址里没有文件名：{installer_url}"))?
        .to_string();
    let sums_url = url.join(CHECKSUMS_ASSET)?;
    validate_download_url(sums_url.as_str())?;
    let resp = reqwest::Client::new()
        .get(sums_url.clone())
        .header("User-Agent", "smelt-updater")
        .send()
        .await?
        .error_for_status()?;
    validate_download_url(resp.url().as_str())?;
    let sums = resp.text().await?;
    smelt_version::checksum_for(&sums, &file)
        .map(str::to_ascii_lowercase)
        .ok_or_else(|| anyhow::anyhow!("{sums_url} 里还没有 {file}，发布可能还没传完"))
}

fn file_sha256(path: &Path) -> std::io::Result<String> {
    let mut file = std::fs::File::open(path)?;
    let mut hasher = Sha256::new();
    let mut buf = vec![0u8; 1024 * 1024];
    loop {
        let read = file.read(&mut buf)?;
        if read == 0 {
            break;
        }
        hasher.update(&buf[..read]);
    }
    Ok(format!("{:x}", hasher.finalize()))
}

/// 下载安装程序，边下边算校验和，对上了才算下好。
///
/// 已经下好、校验过的同一个版本直接用。下新版本前清掉目录里别的旧文件：同一时间只等着
/// 装一个版本。
pub async fn download_and_stage(
    url: &str,
    version: &str,
    on_progress: impl Fn(DownloadProgress),
) -> anyhow::Result<StagedUpdate> {
    validate_download_url(url)?;
    if version.trim().is_empty() {
        anyhow::bail!("更新包缺少版本标识");
    }
    let sha256 = expected_sha256(url).await?;
    let dir = download_dir()?;
    std::fs::create_dir_all(&dir)
        .with_context(|| format!("创建下载目录失败：{}", dir.display()))?;
    let update = StagedUpdate {
        version: version.to_string(),
        url: url.trim().to_string(),
        installer: installer_path(&dir, version),
        sha256,
    };
    if file_sha256(&update.installer).is_ok_and(|actual| actual == update.sha256) {
        return Ok(update);
    }
    remove_other_downloads(&dir, &update.installer);

    let partial = update.installer.with_extension("exe.part");
    let result = download_verified(&update.url, &partial, &update.sha256, &on_progress)
        .await
        .and_then(|()| {
            std::fs::rename(&partial, &update.installer).with_context(|| {
                format!(
                    "移动安装程序失败：{} → {}",
                    partial.display(),
                    update.installer.display()
                )
            })
        });
    if result.is_err() {
        let _ = std::fs::remove_file(&partial);
    }
    result.map(|()| update)
}

/// 目录里除了 `keep` 都删掉。删不掉的（上一个安装程序还在跑）留着，下次再清。
fn remove_other_downloads(dir: &Path, keep: &Path) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path != keep {
            let _ = std::fs::remove_file(&path);
        }
    }
}

async fn download_verified(
    url: &str,
    to: &Path,
    expected: &str,
    on_progress: &dyn Fn(DownloadProgress),
) -> anyhow::Result<()> {
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
        anyhow::bail!("更新地址返回了 HTML，不是安装程序：{url}");
    }
    let total = resp.content_length();
    on_progress(DownloadProgress::Bytes { received: 0, total });

    let mut file = std::fs::File::create(to)?;
    let mut hasher = Sha256::new();
    let mut received = 0u64;
    let mut reported = 0u64;
    while let Some(chunk) = resp.chunk().await? {
        file.write_all(&chunk)?;
        hasher.update(&chunk);
        received += chunk.len() as u64;
        if received - reported >= PROGRESS_REPORT_STEP || Some(received) == total {
            reported = received;
            on_progress(DownloadProgress::Bytes { received, total });
        }
    }
    file.flush()?;
    drop(file);

    let actual = format!("{:x}", hasher.finalize());
    if actual != expected {
        anyhow::bail!("安装程序校验失败（期望 {expected}，实际 {actual}）");
    }
    Ok(())
}

/// 拉起安装程序静默覆盖安装。返回后调用方停掉本机守护、退出桌面；安装程序先等这个
/// 进程退出（`/WAITPID`）再换文件，`relaunch` 时装完重新打开 Smelt。
///
/// 拉起前再校验一遍：下载之后文件被换过就不装。安装程序要脱离桌面的 Job，不然桌面
/// 一退出它就跟着没了。
pub fn launch_installer(update: &StagedUpdate, relaunch: bool) -> anyhow::Result<InstallerTicket> {
    let actual = file_sha256(&update.installer)
        .with_context(|| format!("读取安装程序失败：{}", update.installer.display()))?;
    if actual != update.sha256 {
        anyhow::bail!(
            "安装程序和下载时校验的不一致：{}",
            update.installer.display()
        );
    }
    let log = update.installer.with_extension("log");
    let mut command = std::process::Command::new(&update.installer);
    command
        .args(["/VERYSILENT", "/SUPPRESSMSGBOXES", "/NORESTART", "/SP-"])
        .arg(format!("/WAITPID={}", std::process::id()))
        .arg(format!("/LOG={}", log.display()))
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null());
    if relaunch {
        command.arg("/RELAUNCH=1");
    }
    let child = smelt_sys::process::spawn_detached(&mut command).context("拉起安装程序失败")?;
    Ok(InstallerTicket {
        pid: child.id(),
        log,
    })
}

/// 启动时调用。没有跨进程的待装作业要恢复（见模块说明），只清掉已经装上的这个版本的
/// 安装程序和日志。新版是安装程序装完直接拉起的，那时它可能还没退出、删不掉，下次再清。
pub fn recover_pending_update() -> anyhow::Result<Option<StagedUpdate>> {
    if let Some(release) = smelt_version::RELEASE {
        let installer = installer_path(&download_dir()?, release);
        let _ = std::fs::remove_file(installer.with_extension("log"));
        let _ = std::fs::remove_file(&installer);
    }
    Ok(None)
}

/// 安装失败后更新源换了包：删掉旧的安装程序。
pub fn discard_failed_update(expected: &StagedUpdate) -> anyhow::Result<()> {
    match std::fs::remove_file(&expected.installer) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error)
            .with_context(|| format!("删除安装程序失败：{}", expected.installer.display())),
    }
}

#[cfg(test)]
mod tests {
    use super::{GithubRelease, installer_path, update_asset};
    use std::path::Path;

    fn release(json: &str) -> GithubRelease {
        serde_json::from_str(json).unwrap()
    }

    #[test]
    fn update_asset_is_the_setup_of_the_same_release() {
        let complete = release(
            r#"{"tag_name":"v2026.1027.0","assets":[
                {"name":"Smelt.app.zip","browser_download_url":"https://example.test/Smelt.app.zip"},
                {"name":"smelt-2026.1027.0-windows-x86_64-setup.exe",
                 "browser_download_url":"https://example.test/smelt-2026.1027.0-windows-x86_64-setup.exe"},
                {"name":"SHA256SUMS","browser_download_url":"https://example.test/SHA256SUMS"}]}"#,
        );
        assert_eq!(
            update_asset(&complete).map(|asset| asset.name.as_str()),
            Some("smelt-2026.1027.0-windows-x86_64-setup.exe")
        );
    }

    /// 校验和清单还没传、或者传的是别的版本的安装程序，都不算这次发布能装。
    #[test]
    fn update_asset_needs_checksums_and_matching_version() {
        let without_sums = release(
            r#"{"tag_name":"v2026.1027.0","assets":[
                {"name":"smelt-2026.1027.0-windows-x86_64-setup.exe",
                 "browser_download_url":"https://example.test/setup.exe"}]}"#,
        );
        assert!(update_asset(&without_sums).is_none());
        let other_version = release(
            r#"{"tag_name":"v2026.1027.1","assets":[
                {"name":"smelt-2026.1027.0-windows-x86_64-setup.exe",
                 "browser_download_url":"https://example.test/setup.exe"},
                {"name":"SHA256SUMS","browser_download_url":"https://example.test/SHA256SUMS"}]}"#,
        );
        assert!(update_asset(&other_version).is_none());
    }

    #[test]
    fn installer_file_name_stays_inside_the_download_dir() {
        let dir = Path::new(r"C:\cache\Smelt\update");
        assert_eq!(
            installer_path(dir, "2026.1027.0-rc.1"),
            dir.join("smelt-2026.1027.0-rc.1-setup.exe")
        );
        assert_eq!(
            installer_path(dir, r"..\..\evil"),
            dir.join("smelt-evil-setup.exe")
        );
    }
}
