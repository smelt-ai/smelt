//! smelt 的 JS/TSX 插件管理：目录发现、首帧前主题登记、用户插件导入/删除、命令行 `smelt plugin`、
//! 运行时状态快照和日志接入。
//!
//! 实际的编译、隔离 VM 执行与热重载由 `crates/smelt-nsh` 的 `MainShell` 承担：
//! 1. App 自带插件：仓库根 `plugins/`，打包时放进安装包（见 `builtin`）。
//! 2. 用户插件：`~/.smelt/plugins/`，设置「面板插件」页往这里导入、从这里删除（见 `manage`）。

mod assets;
mod builtin;
mod check;
mod cli;
mod log;
mod manage;
pub(crate) mod status;
mod themes;

use std::cell::RefCell;
use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use gpui::App;

pub(crate) use assets::Assets;
pub(crate) use cli::maybe_run as maybe_run_cli;
pub(crate) use log::install as install_log;
pub(crate) use manage::{import, uninstall, user_plugin_dir};

/// 用户安装插件的目录（`~/.smelt` 下）。
const PLUGIN_DIR: &str = "plugins";

thread_local! {
    static REGISTRY: RefCell<Registry> = RefCell::new(Registry::default());
}

#[derive(Default)]
struct Registry {
    /// 首帧前同步登记的主题文件来源（应用退出时清理）。
    theme_owners: Vec<String>,
    /// 由 `MainShell` 同步来的用户插件列表，供设置「面板插件」页读取。
    installed: Vec<InstalledPlugin>,
}

/// App 自带插件目录（开发态读仓库 `plugins/`，安装包读 `Resources/plugins/`）。
pub(crate) fn builtin_dir() -> Option<&'static Path> {
    builtin::dir()
}

/// 自带插件目录是否开启开发态热重载。
pub(crate) fn builtin_live() -> bool {
    builtin::LIVE
}

/// 更新设置「面板插件」页读取的用户插件列表。
pub(crate) fn set_installed(plugins: Vec<InstalledPlugin>) {
    REGISTRY.with(|registry| registry.borrow_mut().installed = plugins);
}

/// 设置页列出的一个已加载用户插件。
#[derive(Clone, Debug)]
pub(crate) struct InstalledPlugin {
    pub(crate) id: String,
    pub(crate) name: String,
    pub(crate) version: String,
    pub(crate) root: PathBuf,
    /// 加载失败的原因。
    pub(crate) error: Option<String>,
}

/// 用户装的插件，按加载顺序。自带的是 App 的一部分，不在这里列。只读内存。
pub(crate) fn installed() -> Vec<InstalledPlugin> {
    REGISTRY.with(|registry| registry.borrow().installed.clone())
}

/// 先于窗口和外观恢复，同步登记已安装插件里的主题文件；纯主题包与带主题的插件首帧即可生效。
pub(crate) fn register_initial_themes(cx: &mut App) {
    let dirs = [builtin::dir().map(Path::to_path_buf), user_plugin_dir()];
    let mut seen = BTreeSet::new();
    let mut owners = Vec::new();
    for dir in dirs.into_iter().flatten() {
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        let mut roots: Vec<PathBuf> = entries
            .flatten()
            .filter(|entry| !entry.file_name().to_string_lossy().starts_with('.'))
            .map(|entry| entry.path())
            .filter(|path| path.join(smelt_nsh::MANIFEST_FILE).is_file())
            .collect();
        roots.sort();
        for root in roots {
            let Ok(manifest) = smelt_nsh::PluginManifest::read(&root) else {
                continue;
            };
            if !seen.insert(manifest.id.clone()) {
                continue;
            }
            let owner = format!("files:{}", manifest.id);
            match themes::read(&root, &manifest.entry) {
                Ok(sources) if !sources.is_empty() => {
                    let refs: Vec<&str> = sources.iter().map(String::as_str).collect();
                    if let Err(error) =
                        smelt_ui::theme_catalog::register_theme_sets(&owner, &refs, cx)
                    {
                        eprintln!("[plugins] {} 主题加载失败：{error}", manifest.id);
                    } else {
                        owners.push(owner);
                    }
                }
                Ok(_) => {}
                Err(error) => eprintln!("[plugins] {} 主题加载失败：{error}", manifest.id),
            }
        }
    }
    REGISTRY.with(|registry| registry.borrow_mut().theme_owners = owners);
}

/// 应用退出时清理初始主题登记与运行状态快照。
pub(crate) fn shutdown(cx: &mut App) {
    let owners = REGISTRY.with(|registry| {
        let mut reg = registry.borrow_mut();
        reg.installed.clear();
        std::mem::take(&mut reg.theme_owners)
    });
    for owner in owners {
        smelt_ui::theme_catalog::unregister_themes(&owner, cx);
    }
    status::retract();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[gpui::test]
    fn initial_themes_load_builtin_theme_plugin(cx: &mut gpui::TestAppContext) {
        cx.update(|cx| {
            register_initial_themes(cx);
            assert!(
                smelt_ui::theme_catalog::contains("Ayu Dark", cx),
                "首帧前应从内置 plugins/themes 登记上 Ayu Dark 等主题"
            );
            assert!(smelt_ui::theme_catalog::contains("Catppuccin Mocha", cx));
            shutdown(cx);
            assert!(
                !smelt_ui::theme_catalog::contains("Ayu Dark", cx),
                "退出清理后插件主题应卸下"
            );
        });
    }
}
