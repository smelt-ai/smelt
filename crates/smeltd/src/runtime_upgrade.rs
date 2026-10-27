//! 桌面请运行时升到它的版本（设计文档 D7，协议见
//! [`smelt_runtime_client::protocol::runtime_upgrade`]）。运行时自己从 GitHub Release
//! 下载那个版本的包，对着同一个 Release 的 SHA256SUMS 校验，和一键安装（`get-smeltd.sh`
//! 加包里的 `install.sh`）装的是同一份东西：随运行时一起的小工具原子替换进
//! `~/.smelt/bin`，smeltd 发布成不可变映像后重启。进行中的回合会中断，历史记录还在。
//!
//! 只升不降：要的版本比自己旧就拒绝。两台不同版本的桌面连同一个运行时时，运行时最终
//! 跟着较新的那台。系统包和远程桌面的配置要 root，这里不碰，变了仍要重新跑安装脚本。

use super::ClientSocket;
use std::sync::atomic::{AtomicBool, Ordering};

use smelt_runtime_client::protocol::runtime_upgrade::{RuntimeUpgradeReply, RuntimeUpgradeRequest};

/// 同一时间只做一次：两台桌面同时请求时，后来的直接回失败，等前一次的结局。
static IN_PROGRESS: AtomicBool = AtomicBool::new(false);

pub(crate) fn handle_runtime_upgrade<C: ClientSocket>(mut conn: C, request: &serde_json::Value) {
    let reply = match serde_json::from_value::<RuntimeUpgradeRequest>(request.clone()) {
        Ok(request) => upgrade(&mut conn, &request.version),
        Err(error) => RuntimeUpgradeReply::Failed {
            error: format!("请求不合法：{error}"),
        },
    };
    let _ = smelt_runtime_client::protocol::write_json_line(&mut conn, &reply);
}

fn upgrade<C: ClientSocket>(conn: &mut C, version: &str) -> RuntimeUpgradeReply {
    let target = match check_target(version, smelt_version::VERSION) {
        Ok(Some(target)) => target,
        Ok(None) => return RuntimeUpgradeReply::Current,
        Err(error) => return RuntimeUpgradeReply::Failed { error },
    };
    if IN_PROGRESS.swap(true, Ordering::AcqRel) {
        return RuntimeUpgradeReply::Failed {
            error: "已经在升级了，等它结束".to_string(),
        };
    }
    struct Done;
    impl Drop for Done {
        fn drop(&mut self) {
            IN_PROGRESS.store(false, Ordering::Release);
        }
    }
    let _done = Done;
    let _ = smelt_runtime_client::protocol::write_json_line(conn, &RuntimeUpgradeReply::Accepted);
    let _ = conn.flush();
    crate::dlog(&format!("runtime_upgrade：开始升到 v{target}"));
    let reply = install_release(&target);
    crate::dlog(&format!("runtime_upgrade：v{target} → {reply:?}"));
    reply
}

/// 版本号要是合法的 semver（顺带挡住拼进 URL 的斜杠之类）、是一次发版（不带 `+` 构建信息，
/// GitHub 上才有这个 Release），且不比自己旧。比新旧不看构建信息。
///
/// 返回 `None`：自己就出自这次发版，只是 tag 之后还有提交（如 `2026.1027.0-rc.1+g32c3172`
/// 被要求升到 `2026.1027.0-rc.1`）。这时下载那个 Release 其实是退回去，当成已经是这个版本。
fn check_target(requested: &str, current: &str) -> Result<Option<semver::Version>, String> {
    let target = semver::Version::parse(requested.trim())
        .map_err(|error| format!("版本号不合法（{requested}）：{error}"))?;
    if !target.build.is_empty() {
        return Err(format!("v{target} 不是发版的版本号，没有对应的 Release"));
    }
    if let Ok(current) = semver::Version::parse(current) {
        match target.cmp_precedence(&current) {
            std::cmp::Ordering::Less => {
                return Err(format!("只升不降：运行时 v{current} 比要的 v{target} 新"));
            }
            std::cmp::Ordering::Equal if !current.build.is_empty() => return Ok(None),
            _ => {}
        }
    }
    Ok(Some(target))
}

/// 守护启动后在后台调一次，见 `linux::ensure_smelt_pi`。只有装在 `~/.smelt/bin` 的映像
/// 才这样补：开发、测试里直接跑的 smeltd 不去 Release 下载。macOS 上由桌面从 App 里发布。
#[cfg(target_os = "linux")]
pub(crate) fn ensure_smelt_pi() {
    let managed = std::env::current_exe().ok().is_some_and(|exe| {
        exe.file_name()
            .and_then(|name| name.to_str())
            .is_some_and(smelt_core::daemon_image::is_immutable_image_name)
    });
    if !managed {
        return;
    }
    let Some(Ok(version)) = smelt_version::RELEASE.map(semver::Version::parse) else {
        return;
    };
    if let Err(error) = linux::ensure_smelt_pi(&version) {
        smelt_core::app_log::error("smeltd", &format!("补齐 smelt-pi 失败：{error}"));
    }
}

#[cfg(not(target_os = "linux"))]
fn install_release(_version: &semver::Version) -> RuntimeUpgradeReply {
    RuntimeUpgradeReply::Failed {
        error: "这台运行时随这台机器上的 Smelt 一起更新，不能远程升级：到这台机器上更新 Smelt"
            .to_string(),
    }
}

#[cfg(target_os = "linux")]
fn install_release(version: &semver::Version) -> RuntimeUpgradeReply {
    use crate::service::ImageInstall;

    let result = linux::Staging::new("runtime-upgrade").and_then(|staging| {
        let package = linux::fetch_package(&staging, version, &release_url(version))?;
        linux::install_helpers(&package)?;
        crate::service::install_image(&package.join("smeltd"))
    });
    match result {
        // 重启成功时这个进程已经退出，一般走不到 `Upgraded`；真走到了，跑着的也已是新版。
        Ok(ImageInstall::AlreadyInstalled | ImageInstall::Upgraded) => RuntimeUpgradeReply::Current,
        // 问自己没问着，当成没在跑：启动名已拨到新版，下次拉起就是它。
        Ok(ImageInstall::Launched) => RuntimeUpgradeReply::Staged {
            reason: "新版本已装好，运行时下次重启时换过去".to_string(),
        },
        Err(error) => RuntimeUpgradeReply::Failed { error },
    }
}

/// 和一键安装脚本取的是同一个 Release。
#[cfg(any(target_os = "linux", test))]
const RELEASE_REPO: &str = "smelt-ai/smelt";

#[cfg(any(target_os = "linux", test))]
fn release_url(version: &semver::Version) -> String {
    format!("https://github.com/{RELEASE_REPO}/releases/download/v{version}")
}

/// 包名和 `scripts/package-linux.sh` 打出来的一致。架构名同 `uname -m`。
#[cfg(any(target_os = "linux", test))]
fn package_name(version: &semver::Version, arch: &str) -> String {
    format!("smeltd-{version}-linux-{arch}")
}

#[cfg(target_os = "linux")]
mod linux {
    use std::fs;
    use std::os::unix::fs::PermissionsExt as _;
    use std::path::{Path, PathBuf};
    use std::process::Command;

    /// 包里随 smeltd 一起装进 `~/.smelt/bin` 的东西，同安装脚本。smeltd 自己另走映像。
    const HELPERS: [&str; 2] = ["smelt-notify", "smelt-up"];

    /// 下载、解包用的临时目录，用完删掉。放在 `~/.smelt` 下：和 `~/.smelt/bin` 同一个
    /// 文件系统，`/tmp` 常是容量很小的 tmpfs。按用途分名字、每次先清空：交接成功时旧进程
    /// 直接退出，`Drop` 来不及跑，留下的这份由下一次同一用途清掉。同一用途同一时间只有
    /// 一次（升级有 `IN_PROGRESS`，补 smelt-pi 只在启动时跑一次），不会互相清掉。
    pub(super) struct Staging(PathBuf);

    impl Staging {
        pub(super) fn new(purpose: &str) -> Result<Self, String> {
            let dir = smelt_paths::smelt_home()
                .ok_or_else(|| "找不到 $HOME".to_string())?
                .join("staging")
                .join(purpose);
            let _ = fs::remove_dir_all(&dir);
            fs::create_dir_all(&dir)
                .map_err(|error| format!("创建 {} 失败：{error}", dir.display()))?;
            Ok(Self(dir))
        }
    }

    impl Drop for Staging {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    /// 下载、校验、解包，验证包里的 smeltd 在这台机器上跑得起来、版本对得上，返回解开的
    /// 包目录。`base` 是 Release 下载地址的前缀（测试里指向本地文件）。
    pub(super) fn fetch_package(
        staging: &Staging,
        version: &semver::Version,
        base: &str,
    ) -> Result<PathBuf, String> {
        let name = super::package_name(version, std::env::consts::ARCH);
        let archive_name = format!("{name}.tar.gz");
        let sums_path = staging.0.join("SHA256SUMS");
        download(&format!("{base}/SHA256SUMS"), &sums_path).map_err(|error| {
            format!("取不到 v{version} 的校验和清单（GitHub 上可能没有这个版本）：{error}")
        })?;
        let sums =
            fs::read_to_string(&sums_path).map_err(|error| format!("读校验和清单失败：{error}"))?;
        let expected = smelt_version::checksum_for(&sums, &archive_name)
            .ok_or_else(|| {
                format!(
                    "v{version} 没有这台机器（linux-{}）的包",
                    std::env::consts::ARCH
                )
            })?
            .to_string();
        let archive = staging.0.join(&archive_name);
        download(&format!("{base}/{archive_name}"), &archive)
            .map_err(|error| format!("下载 {archive_name} 失败：{error}"))?;
        let actual = smelt_core::daemon_image::content_fingerprint(&archive)
            .map_err(|error| format!("读取下载的包失败：{error}"))?;
        if actual != expected {
            return Err(format!(
                "{archive_name} 校验失败（期望 {expected}，实际 {actual}）"
            ));
        }
        let output = Command::new("tar")
            .arg("-xzf")
            .arg(&archive)
            .arg("-C")
            .arg(&staging.0)
            .output()
            .map_err(|error| format!("无法执行 tar：{error}"))?;
        if !output.status.success() {
            return Err(format!(
                "解包失败：{}",
                String::from_utf8_lossy(&output.stderr).trim()
            ));
        }
        let package = staging.0.join(&name);
        verify_smeltd(&package.join("smeltd"), version)?;
        for helper in HELPERS {
            if !package.join(helper).is_file() {
                return Err(format!("包里缺少 {helper}"));
            }
        }
        Ok(package)
    }

    /// 连不上、或者连上了却半天不动都要放弃，不能一直等：桌面那边一直显示「升级中」。
    fn download(url: &str, to: &Path) -> Result<(), String> {
        let output = Command::new("curl")
            .args(["-fsSL", "--retry", "2", "--connect-timeout", "20"])
            // 连续 60 秒低于 1 KB/s 就当卡住了。
            .args(["--speed-limit", "1024", "--speed-time", "60", "-o"])
            .arg(to)
            .arg(url)
            .output()
            .map_err(|error| format!("无法执行 curl：{error}"))?;
        if output.status.success() {
            Ok(())
        } else {
            Err(String::from_utf8_lossy(&output.stderr).trim().to_string())
        }
    }

    /// 包里的 smeltd 在这台机器上能跑，而且就是要的版本。架构不对、glibc 太旧都在这一步
    /// 暴露，不能等交接时才失败。
    fn verify_smeltd(smeltd: &Path, version: &semver::Version) -> Result<(), String> {
        let output = Command::new(smeltd)
            .arg("--version")
            .output()
            .map_err(|error| format!("新版 smeltd 在这台机器上跑不起来：{error}"))?;
        let printed = String::from_utf8_lossy(&output.stdout);
        let expected = format!("smeltd {version}");
        if !output.status.success() || printed.trim() != expected {
            return Err(format!(
                "新版 smeltd 在这台机器上跑不起来（{}）",
                [
                    printed.trim(),
                    String::from_utf8_lossy(&output.stderr).trim()
                ]
                .into_iter()
                .filter(|text| !text.is_empty())
                .collect::<Vec<_>>()
                .join("；")
            ));
        }
        Ok(())
    }

    /// 同安装脚本的 `put`：先写到旁边再改名盖过去，正在跑的旧版本不受影响。
    /// Linux 上没有桌面替运行时发布 smelt-pi，运行时自己保证它在位。链接已经指向能执行的
    /// 一份就不动；缺了（旧版运行时代为升级时还不认识 smelt-pi、文件被删、从没有 smelt-pi
    /// 的目录 install）就按自己的版本取 Release 包，校验后发布。顺带回收没人用的旧版本。
    pub(super) fn ensure_smelt_pi(version: &semver::Version) -> Result<(), String> {
        let bin = crate::service::bin_dir()?;
        if smelt_core::pi_image::current(&bin).is_none() {
            let staging = Staging::new("smelt-pi")?;
            let package = fetch_package(&staging, version, &super::release_url(version))?;
            smelt_core::pi_image::publish(&bin, &package)
                .map_err(|error| format!("发布 smelt-pi 失败：{error}"))?;
        }
        smelt_core::pi_image::reap_unused(&bin)
            .map_err(|error| format!("回收旧 smelt-pi 失败：{error}"))
    }

    pub(super) fn install_helpers(package: &Path) -> Result<(), String> {
        let bin = crate::service::bin_dir()?;
        fs::create_dir_all(&bin)
            .map_err(|error| format!("创建 {} 失败：{error}", bin.display()))?;
        for helper in HELPERS {
            let to = bin.join(helper);
            let temporary = bin.join(format!(".{helper}.new-{}", std::process::id()));
            let result = fs::copy(package.join(helper), &temporary)
                .and_then(|_| fs::set_permissions(&temporary, fs::Permissions::from_mode(0o755)))
                .and_then(|()| fs::rename(&temporary, &to));
            if let Err(error) = result {
                let _ = fs::remove_file(&temporary);
                return Err(format!("装 {} 失败：{error}", to.display()));
            }
        }
        Ok(())
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        /// 下载回来的包和清单对不上就停，不解包、不装。
        #[test]
        fn a_tampered_package_is_rejected() {
            let release = std::env::temp_dir()
                .join(format!("smelt-release-{}", uuid::Uuid::new_v4().simple()));
            fs::create_dir_all(&release).unwrap();
            let version = semver::Version::parse("9.9.9").unwrap();
            let archive = format!(
                "{}.tar.gz",
                super::super::package_name(&version, std::env::consts::ARCH)
            );
            fs::write(release.join(&archive), b"not the real package").unwrap();
            fs::write(
                release.join("SHA256SUMS"),
                format!("{}  {archive}\n", "0".repeat(64)),
            )
            .unwrap();

            let staging = Staging(release.join("staging"));
            fs::create_dir_all(&staging.0).unwrap();
            let error = fetch_package(&staging, &version, &format!("file://{}", release.display()))
                .unwrap_err();
            assert!(error.contains("校验失败"), "{error}");
            drop(staging);
            let _ = fs::remove_dir_all(release);
        }

        /// Release 里没有这个版本：说清楚，而不是只甩一句 curl 的报错。
        #[test]
        fn a_missing_release_says_so() {
            let staging = Staging::new_in_temp();
            let version = semver::Version::parse("9.9.9").unwrap();
            let error =
                fetch_package(&staging, &version, "file:///nonexistent-smelt-release").unwrap_err();
            assert!(error.contains("没有这个版本"), "{error}");
        }

        impl Staging {
            fn new_in_temp() -> Self {
                let dir = std::env::temp_dir()
                    .join(format!("smelt-staging-{}", uuid::Uuid::new_v4().simple()));
                fs::create_dir_all(&dir).unwrap();
                Self(dir)
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_upgrades_never_downgrades() {
        assert!(check_target("0.9.1", "0.9.0").is_ok());
        assert!(check_target("0.9.0", "0.9.0").is_ok());
        assert!(check_target("0.9.0", "0.9.0-rc.2").is_ok());
        assert!(check_target("0.9.0-rc.3", "0.9.0-rc.2").is_ok());
        let error = check_target("0.8.9", "0.9.0").unwrap_err();
        assert!(error.contains("只升不降"), "{error}");
        assert!(check_target("0.9.0-rc.1", "0.9.0").is_err());
    }

    /// v0.9.1 之后按日期取号（`YYYY.MDD.N`，见 docs/release.md），从旧号升上来不能被当成降级。
    #[test]
    fn date_versions_upgrade_from_old_scheme_and_sort_by_date() {
        assert!(check_target("2026.1023.0", "0.9.1").is_ok());
        assert!(check_target("2026.1023.1", "2026.1023.0").is_ok());
        assert!(check_target("2026.1023.0", "2026.1023.0-rc.1").is_ok());
        assert!(check_target("2027.103.0", "2026.1231.5").is_ok());
        assert!(check_target("2026.1003.0", "2026.1031.0").is_err());
        assert!(
            check_target("2026.0103.0", "0.9.1").is_err(),
            "前导零不是合法 semver"
        );
    }

    /// tag 之后的构建带构建信息：比新旧时不算它，但要升到的版本不能带（没有这样的 Release）。
    #[test]
    fn build_metadata_is_ignored_when_comparing_and_rejected_as_target() {
        assert!(matches!(
            check_target("2026.1027.0", "2026.1027.0-rc.1+g32c3172"),
            Ok(Some(_))
        ));
        assert!(check_target("2026.1026.0", "2026.1027.0-rc.1+g32c3172").is_err());
        let error = check_target("2026.1027.0-rc.1+g32c3172", "2026.1026.0").unwrap_err();
        assert!(error.contains("不是发版"), "{error}");
    }

    /// 出自同一次发版、只是 tag 之后多了提交的运行时，不能被「升」回那个 Release。
    #[test]
    fn same_release_with_later_commits_is_already_current() {
        assert_eq!(
            check_target("2026.1027.0-rc.1", "2026.1027.0-rc.1+g32c3172"),
            Ok(None)
        );
        assert!(matches!(
            check_target("2026.1027.0-rc.1", "2026.1027.0-rc.1"),
            Ok(Some(_))
        ));
    }

    /// 版本号会拼进下载地址，不是 semver 的一律拒绝。
    #[test]
    fn version_must_be_semver() {
        assert!(check_target("../../evil", "0.9.0").is_err());
        assert!(check_target("1.0.0/x", "0.9.0").is_err());
        assert!(check_target("", "0.9.0").is_err());
    }

    #[test]
    fn release_paths_match_the_published_package() {
        let version = semver::Version::parse("0.9.1-rc.1").unwrap();
        assert_eq!(
            release_url(&version),
            "https://github.com/smelt-ai/smelt/releases/download/v0.9.1-rc.1"
        );
        assert_eq!(
            package_name(&version, "x86_64"),
            "smeltd-0.9.1-rc.1-linux-x86_64"
        );
    }
}
