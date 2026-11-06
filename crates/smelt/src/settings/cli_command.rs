//! 终端里的 `smelt` 命令（macOS）。
//!
//! GUI 的 `smelt` 自己就是命令行（启动时先拦截子命令，见 `smelt_cli::maybe_run`），这里只管
//! 让人和智能体敲得到它：
//!
//! - `~/.smelt/bin/smelt`：每次启动都指向正在跑的这个 App。运行时给智能体的 PATH 最前面就是
//!   `~/.smelt/bin`（见 `smelt_core::login_env`），内置的 `smelt` 技能靠它。
//! - `/usr/local/bin/smelt`：在设置里点一下才装，指向上面那个链接，App 换了位置也跟着走。
//!   `/usr/local/bin` 在 macOS 默认的 PATH 里（`/etc/paths`），不用改 shell 配置。目录一般归
//!   root，写不进去就经 osascript 弹系统的管理员授权框。
//!
//! Linux 由运行时安装包放 `~/.smelt/bin/smelt`、链到 `~/.local/bin`；Windows 由安装程序把安装
//! 目录（里面有 `smelt.com`）加进 PATH。

use gpui::{App, Global};
use std::path::{Path, PathBuf};

const SYSTEM_LINK: &str = "/usr/local/bin/smelt";

/// `/usr/local/bin/smelt` 现在是什么样。
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum SystemCommand {
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

#[derive(Clone, Default)]
pub struct CliCommandState {
    pub system: SystemCommand,
    pub installing: bool,
}

impl Global for CliCommandState {}

fn managed_link() -> Option<PathBuf> {
    smelt_paths::smelt_home().map(|home| home.join("bin").join("smelt"))
}

/// 启动时调：拨好 `~/.smelt/bin/smelt`，顺带查一次 `/usr/local/bin/smelt` 给设置页用。
pub fn sync_cli_command(cx: &mut App) {
    if let Err(error) = sync_managed_link() {
        eprintln!("[workspace] 放 ~/.smelt/bin/smelt 失败：{error}");
    }
    cx.set_global(CliCommandState {
        system: system_command(),
        installing: false,
    });
}

/// `~/.smelt/bin` 归我们管：不管原来是什么都换成指向当前 App 的软链接。先建临时链接再
/// rename，任何时候去找都有一份能用的。
fn sync_managed_link() -> std::io::Result<()> {
    let exe = std::env::current_exe()?;
    let link = managed_link().ok_or_else(|| std::io::Error::other("找不到 $HOME"))?;
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

fn system_command() -> SystemCommand {
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

/// 设置页的按钮。要等授权框，放后台；结束后重新查一次状态。
pub fn install_system_command(cx: &mut App) {
    if cx.default_global::<CliCommandState>().installing {
        return;
    }
    cx.default_global::<CliCommandState>().installing = true;
    cx.refresh_windows();
    let task = cx.background_executor().spawn(async { install() });
    cx.spawn(async move |cx| {
        let result = task.await;
        cx.update(|cx| {
            cx.set_global(CliCommandState {
                system: system_command(),
                installing: false,
            });
            match result {
                Ok(()) => {
                    crate::status_item::notify_success("已装好 smelt 命令，新开的终端里就能用")
                }
                Err(error) => {
                    eprintln!("[workspace] 装 smelt 命令失败：{error}");
                    crate::status_item::notify_error(format!("装 smelt 命令失败：{error}"));
                }
            }
            cx.refresh_windows();
        });
    })
    .detach();
}

fn install() -> Result<(), String> {
    if system_command() == SystemCommand::Taken {
        return Err(format!("{SYSTEM_LINK} 已经有别的程序，没有覆盖"));
    }
    sync_managed_link().map_err(|error| format!("放 ~/.smelt/bin/smelt 失败：{error}"))?;
    let target = managed_link().ok_or("找不到 $HOME")?;
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

fn link_with_administrator(target: &Path) -> Result<(), String> {
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
        .map_err(|error| format!("无法执行 osascript：{error}"))?;
    if output.status.success() {
        return Ok(());
    }
    let stderr = String::from_utf8_lossy(&output.stderr);
    // 授权框点了取消。
    if stderr.contains("(-128)") {
        return Err("已取消授权".to_string());
    }
    Err(stderr.trim().to_string())
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
            "smelt-cli-command-{}",
            uuid::Uuid::new_v4().simple()
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
