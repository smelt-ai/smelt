//! App 自带的 JS 插件：仓库根 `plugins/` 下每个子目录一个，跟着 App 更新。
//!
//! - 随包分发的自带插件由 `scripts/stage-plugins.sh` 预编译进安装包（macOS 是
//!   `Smelt.app/Contents/Resources/plugins/`，Windows 是安装目录下的 `plugins\`），并直接加载预编译
//!   QuickJS 字节码，无需运行时现打包、也不挂热重载监听；
//! - 仅调试构建（`cfg!(debug_assertions)`）找不到安装包插件目录时回退到仓库 `plugins/` 方便跑测。
//!
//! 用户可在 `~/.smelt/plugins/<id>/` 放置同 id 插件覆写自带版（若用户版编译或运行失败，宿主会
//! 自动回退到自带版本）；也可通过 `smelt plugin fork` 拷贝源码原地覆写或 `--as` 改名并存。

use std::path::{Path, PathBuf};
use std::sync::OnceLock;

/// 自带插件所在的目录，下面每个子目录一个插件。找不到时 `None`，原因记日志。
pub(super) fn dir() -> Option<&'static Path> {
    static DIR: OnceLock<Option<PathBuf>> = OnceLock::new();
    DIR.get_or_init(locate).as_deref()
}

fn locate() -> Option<PathBuf> {
    if let Some(dir) = std::env::current_exe()
        .ok()
        .and_then(|exe| packaged_dir(&exe))
    {
        return Some(dir);
    }
    if cfg!(debug_assertions) {
        return Some(repo_dir());
    }
    if let Ok(exe) = std::env::current_exe() {
        eprintln!(
            "[plugins] {} 旁边没有安装包里的自带插件目录，自带插件不加载",
            exe.display()
        );
    }
    None
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
