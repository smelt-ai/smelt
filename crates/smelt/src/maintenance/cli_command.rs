//! 终端里的 `smelt` 命令（macOS，纯底层无界面操作）。
//!
//! - `~/.smelt/bin/smelt`：每次启动都指向正在跑的这个 App。运行时给智能体的 PATH 最前面就是
//!   `~/.smelt/bin`（见 `smelt_core::login_env`），内置的 `smelt` 技能靠它。
//! - `/usr/local/bin/smelt`：按需安装，指向上面那个链接，App 换了位置也跟着走。目录一般归
//!   root，写不进去就经 osascript 弹系统的管理员授权框。
//!
//! 这里全是阻塞的文件 IO 和子进程，只在后台线程上调。

use std::path::{Path, PathBuf};

use smelt_paths::smelt_home;

pub(crate) const SYSTEM_LINK: &str = "/usr/local/bin/smelt";

/// `/usr/local/bin/smelt` 现在是什么样。
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) enum SystemCommand {
    /// 还没查过。
    #[default]
    Unknown,
    /// 指向这个 App。
    Installed,
    Missing,
    /// 是个软链接但指向别处（旧位置的 App、已经删掉的东西），装的时候直接换掉。
    Stale,
    /// 是个普通文件，不是我们放的，不覆盖。
    Taken,
}

impl SystemCommand {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::Unknown => "unknown",
            Self::Installed => "installed",
            Self::Missing => "missing",
            Self::Stale => "stale",
            Self::Taken => "taken",
        }
    }
}

/// 安装 `/usr/local/bin/smelt` 的结构化错误，具体多语言文案由 JS 插件按 code 渲染。
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum InstallError {
    Taken,
    Cancelled,
    NoHome,
    LinkFailed(String),
    OsascriptFailed(String),
    Failed(String),
}

impl InstallError {
    pub(crate) fn code(&self) -> &'static str {
        match self {
            Self::Taken => "taken",
            Self::Cancelled => "cancelled",
            Self::NoHome => "no_home",
            Self::LinkFailed(_) => "link_failed",
            Self::OsascriptFailed(_) => "osascript_failed",
            Self::Failed(_) => "failed",
        }
    }

    pub(crate) fn detail(&self) -> &str {
        match self {
            Self::Taken | Self::Cancelled | Self::NoHome => "",
            Self::LinkFailed(msg) | Self::OsascriptFailed(msg) | Self::Failed(msg) => msg,
        }
    }
}

fn managed_link() -> Option<PathBuf> {
    smelt_home().map(|home| home.join("bin").join("smelt"))
}

/// `~/.smelt/bin` 归我们管：不管原来是什么都换成指向当前 App 的软链接。先建临时链接再
/// rename，任何时候去找都有一份能用的。
pub(crate) fn sync_managed_link() -> Result<(), String> {
    sync_managed_link_io().map_err(|error| error.to_string())
}

fn sync_managed_link_io() -> std::io::Result<()> {
    let exe = std::env::current_exe()?;
    let link = managed_link().ok_or_else(|| std::io::Error::other("missing $HOME"))?;
    if std::fs::read_link(&link).is_ok_and(|target| target == exe) {
        return Ok(());
    }
    let dir = link.parent().expect("managed link has a parent");
    std::fs::create_dir_all(dir)?;
    let staged = dir.join(format!(".smelt.link-{}", std::process::id()));
    let _ = std::fs::remove_file(&staged);
    std::os::unix::fs::symlink(&exe, &staged)?;
    std::fs::rename(&staged, &link).inspect_err(|_| {
        let _ = std::fs::remove_file(&staged);
    })
}

/// `/usr/local/bin/smelt` 现在是什么样。
pub(crate) fn system_command() -> SystemCommand {
    let Ok(exe) = std::env::current_exe() else {
        return SystemCommand::Unknown;
    };
    classify(Path::new(SYSTEM_LINK), &exe)
}

fn classify(link: &Path, exe: &Path) -> SystemCommand {
    match std::fs::symlink_metadata(link) {
        Err(_) => SystemCommand::Missing,
        Ok(meta) if !meta.file_type().is_symlink() => SystemCommand::Taken,
        Ok(_) => match (link.canonicalize(), exe.canonicalize()) {
            (Ok(target), Ok(exe)) if target == exe => SystemCommand::Installed,
            _ => SystemCommand::Stale,
        },
    }
}

/// 放好 `~/.smelt/bin/smelt`，再在 `/usr/local/bin` 放一个指向它的链接，写不进去就弹系统的
/// 管理员授权框，要等人点，所以只在后台调。
pub(crate) fn install() -> Result<(), InstallError> {
    if system_command() == SystemCommand::Taken {
        return Err(InstallError::Taken);
    }
    sync_managed_link().map_err(InstallError::LinkFailed)?;
    let target = managed_link().ok_or(InstallError::NoHome)?;
    // /usr/local/bin 归当前用户（装过 Intel 版 Homebrew 的机器常见）时不用提权。
    if link_without_privileges(&target).is_ok() {
        return Ok(());
    }
    link_with_administrator(&target)
}

fn link_without_privileges(target: &Path) -> std::io::Result<()> {
    let link = Path::new(SYSTEM_LINK);
    let dir = link.parent().expect("system link has a parent");
    std::fs::create_dir_all(dir)?;
    let staged = dir.join(format!(".smelt.link-{}", std::process::id()));
    let _ = std::fs::remove_file(&staged);
    std::os::unix::fs::symlink(target, &staged)?;
    std::fs::rename(&staged, link).inspect_err(|_| {
        let _ = std::fs::remove_file(&staged);
    })
}

fn link_with_administrator(target: &Path) -> Result<(), InstallError> {
    let command = format!(
        "mkdir -p /usr/local/bin && ln -sfn {} {SYSTEM_LINK}",
        shell_quote(&target.to_string_lossy())
    );
    let script = format!(
        "do shell script {} with administrator privileges",
        applescript_string(&command)
    );
    let output = std::process::Command::new("/usr/bin/osascript")
        .args(["-e", &script])
        .output()
        .map_err(|error| InstallError::OsascriptFailed(error.to_string()))?;
    if output.status.success() {
        return Ok(());
    }
    let stderr = String::from_utf8_lossy(&output.stderr);
    // 授权框点了取消。
    if stderr.contains("(-128)") {
        return Err(InstallError::Cancelled);
    }
    Err(InstallError::Failed(stderr.trim().to_string()))
}

/// 放进 `sh -c` 的单引号字符串。
fn shell_quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', r"'\''"))
}

/// AppleScript 的字符串字面量。
fn applescript_string(value: &str) -> String {
    format!("\"{}\"", value.replace('\\', r"\\").replace('"', "\\\""))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn quoting_survives_spaces_and_quotes() {
        assert_eq!(shell_quote("/Users/a b/it's"), r"'/Users/a b/it'\''s'");
        assert_eq!(
            applescript_string(r#"say "hi" \ ok"#),
            r#""say \"hi\" \\ ok""#
        );
    }

    #[test]
    fn classify_tells_ours_from_others() {
        let root = std::env::temp_dir().join(format!(
            "smelt-cli-command-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|duration| duration.as_nanos())
                .unwrap_or_default(),
        ));
        std::fs::create_dir_all(&root).unwrap();
        let exe = root.join("smelt-app");
        let other = root.join("other");
        std::fs::write(&exe, "").unwrap();
        std::fs::write(&other, "").unwrap();
        let link = root.join("smelt");
        assert_eq!(classify(&link, &exe), SystemCommand::Missing);

        // 隔着 ~/.smelt/bin/smelt 那一层也认得出是自己。
        let managed = root.join("managed");
        std::os::unix::fs::symlink(&exe, &managed).unwrap();
        std::os::unix::fs::symlink(&managed, &link).unwrap();
        assert_eq!(classify(&link, &exe), SystemCommand::Installed);

        std::fs::remove_file(&link).unwrap();
        std::os::unix::fs::symlink(&other, &link).unwrap();
        assert_eq!(classify(&link, &exe), SystemCommand::Stale);
        std::fs::remove_file(&link).unwrap();
        std::os::unix::fs::symlink(root.join("gone"), &link).unwrap();
        assert_eq!(classify(&link, &exe), SystemCommand::Stale);

        std::fs::remove_file(&link).unwrap();
        std::fs::write(&link, "").unwrap();
        assert_eq!(classify(&link, &exe), SystemCommand::Taken);
        let _ = std::fs::remove_dir_all(&root);
    }
}
