//! App 自带的 JS 插件：仓库根 `plugins/` 下每个子目录一个，跟着 App 更新。
//!
//! - 发版的包从安装包里只读加载：macOS 是 `Smelt.app/Contents/Resources/plugins/`，Windows 是
//!   安装目录下的 `plugins\`，打包时由 `scripts/stage-plugins.sh` 放进去。插件和 App 一起签名、
//!   一起整体替换，不往用户目录写副本。
//! - 开发用的包（调试构建，和 `make dev` 编的 release）直接读仓库里的 `plugins/`，改了脚本即
//!   重挂（见 `watch`），只有改 Rust 才要重编。
//!
//! 自带插件只读：用户想改，复制一份、换个 id，再在布局里换上自己那份。和自带插件同 id 的
//! 用户插件不加载，报重复（见 `super::discover`）。

use std::path::{Path, PathBuf};
use std::sync::OnceLock;

/// 自带插件直接读仓库、改了即重挂：调试构建，和编译时设了 `SMELT_LIVE_PLUGINS` 的 release
/// （`make dev`）。发版的包不设，读安装包里的那份。
pub(super) const LIVE: bool = cfg!(debug_assertions) || option_env!("SMELT_LIVE_PLUGINS").is_some();

/// 自带插件所在的目录，下面每个子目录一个插件。找不到时 `None`，原因记日志。
pub(super) fn dir() -> Option<&'static Path> {
    static DIR: OnceLock<Option<PathBuf>> = OnceLock::new();
    DIR.get_or_init(locate).as_deref()
}

/// 加载时要不要往插件目录里写给编辑器的文件（`gpui-kit.d.ts`、`jsconfig.json`）：安装包里的
/// 那份只读、签过名，不写；读仓库时要写，改脚本时有补全。
pub(super) fn editor_files(root: &Path) -> bool {
    LIVE || !dir().is_some_and(|dir| root.starts_with(dir))
}

fn locate() -> Option<PathBuf> {
    if LIVE {
        return Some(repo_dir());
    }
    let exe = std::env::current_exe().ok()?;
    let dir = packaged_dir(&exe);
    if dir.is_none() {
        eprintln!(
            "[plugins] {} 旁边没有安装包里的自带插件目录，自带插件不加载",
            exe.display()
        );
    }
    dir
}

/// 仓库里的自带插件。
pub(super) fn repo_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../plugins")
}

/// 安装包里放自带插件的目录：macOS 在 `<App>.app/Contents/Resources/plugins`（可执行文件在
/// `Contents/MacOS/`），别的系统在可执行文件旁边的 `plugins`。不在时 `None`。
fn packaged_dir(exe: &Path) -> Option<PathBuf> {
    let parent = exe.parent()?;
    let dir = if cfg!(target_os = "macos") {
        parent.parent()?.join("Resources").join("plugins")
    } else {
        parent.join("plugins")
    };
    dir.is_dir().then_some(dir)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds_the_plugins_the_package_ships() {
        let root = std::env::temp_dir().join(format!("smelt-package-{}", uuid::Uuid::new_v4()));
        let (exe, plugins) = if cfg!(target_os = "macos") {
            let contents = root.join("Smelt.app/Contents");
            (
                contents.join("MacOS/smelt"),
                contents.join("Resources/plugins"),
            )
        } else {
            (root.join("smelt.exe"), root.join("plugins"))
        };
        std::fs::create_dir_all(exe.parent().unwrap()).unwrap();
        assert_eq!(packaged_dir(&exe), None, "没放插件的包");
        std::fs::create_dir_all(&plugins).unwrap();
        assert_eq!(packaged_dir(&exe), Some(plugins));
        std::fs::remove_dir_all(root).unwrap();
    }
}
