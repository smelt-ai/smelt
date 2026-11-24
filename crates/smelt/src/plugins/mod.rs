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
mod fork;
mod init;
mod log;
mod manage;
pub(crate) mod status;
mod themes;

use std::cell::RefCell;
use std::collections::BTreeMap;
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

/// App 自带插件目录（打包版读 `Resources/plugins/`，未打包的调试构建回退仓库 `plugins/`）。
pub(crate) fn builtin_dir() -> Option<&'static Path> {
    builtin::dir()
}

/// 供 `check` / `types` / `init` / `fork` 发现可用共享 JS 模块插件的源目录（用户覆写优先，自带兜底）。
pub(super) fn module_sources() -> Vec<smelt_nsh::SourceDir> {
    let mut out = Vec::new();
    if let Some(dir) = builtin::dir() {
        out.push(smelt_nsh::SourceDir {
            path: dir.to_path_buf(),
            source: smelt_nsh::PluginSource::Builtin,
            watch: false,
        });
    }
    if let Some(dir) = user_plugin_dir() {
        out.push(smelt_nsh::SourceDir {
            path: dir,
            source: smelt_nsh::PluginSource::User,
            watch: false,
        });
    }
    out
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
    pub(crate) overrides_builtin: bool,
    pub(crate) fallback_to_builtin: bool,
    /// 加载失败的原因。
    pub(crate) error: Option<String>,
}

/// 用户装的插件，按加载顺序。自带的是 App 的一部分，不在这里列。只读内存。
pub(crate) fn installed() -> Vec<InstalledPlugin> {
    REGISTRY.with(|registry| registry.borrow().installed.clone())
}

fn scan_theme_manifests(
    dir: Option<&Path>,
) -> BTreeMap<String, (PathBuf, smelt_nsh::PluginManifest)> {
    let mut found = BTreeMap::new();
    let Some(dir) = dir else {
        return found;
    };
    let Ok(entries) = std::fs::read_dir(dir) else {
        return found;
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
        found.entry(manifest.id.clone()).or_insert((root, manifest));
    }
    found
}

fn try_register_plugin_themes(
    id: &str,
    root: &Path,
    manifest: &smelt_nsh::PluginManifest,
    cx: &mut App,
) -> Result<Option<String>, String> {
    let sources = themes::read(root, &manifest.entry)?;
    if sources.is_empty() {
        return Ok(None);
    }
    let owner = format!("files:{id}");
    let refs: Vec<&str> = sources.iter().map(String::as_str).collect();
    smelt_ui::theme_catalog::register_theme_sets(&owner, &refs, cx)
        .map_err(|error| error.to_string())?;
    Ok(Some(owner))
}

fn register_themes_from_dirs(
    user_dir: Option<&Path>,
    builtin_dir: Option<&Path>,
    cx: &mut App,
) -> Vec<String> {
    let mut users = scan_theme_manifests(user_dir);
    let builtins = scan_theme_manifests(builtin_dir);
    let mut owners = Vec::new();

    for (id, (b_root, b_manifest)) in &builtins {
        if let Some((u_root, u_manifest)) = users.remove(id) {
            match try_register_plugin_themes(id, &u_root, &u_manifest, cx) {
                Ok(Some(owner)) => {
                    owners.push(owner);
                    continue;
                }
                Ok(None) => continue,
                Err(error) => {
                    eprintln!("[plugins] {id} 用户主题加载失败，回退自带版本：{error}");
                }
            }
        }
        match try_register_plugin_themes(id, b_root, b_manifest, cx) {
            Ok(Some(owner)) => owners.push(owner),
            Ok(None) => {}
            Err(error) => eprintln!("[plugins] {id} 主题加载失败：{error}"),
        }
    }

    for (id, (u_root, u_manifest)) in &users {
        match try_register_plugin_themes(id, u_root, u_manifest, cx) {
            Ok(Some(owner)) => owners.push(owner),
            Ok(None) => {}
            Err(error) => eprintln!("[plugins] {id} 主题加载失败：{error}"),
        }
    }

    owners
}

/// 先于窗口和外观恢复，同步登记已安装插件里的主题文件（用户同 id 覆写自带，失败自动回退自带）。
pub(crate) fn register_initial_themes(cx: &mut App) {
    let user_dir = user_plugin_dir();
    let owners = register_themes_from_dirs(user_dir.as_deref(), builtin::dir(), cx);
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

    #[gpui::test]
    fn register_initial_themes_prefers_user_override_and_falls_back_to_builtin_on_error(
        cx: &mut gpui::TestAppContext,
    ) {
        let scratch =
            std::env::temp_dir().join(format!("smelt-init-themes-{}", uuid::Uuid::new_v4()));
        let builtin = scratch.join("builtin/smelt.themes");
        let user = scratch.join("user/smelt.themes");
        std::fs::create_dir_all(builtin.join("themes")).unwrap();
        std::fs::create_dir_all(user.join("themes")).unwrap();
        std::fs::write(
            builtin.join(smelt_nsh::MANIFEST_FILE),
            r#"{"id":"smelt.themes","name":"Themes","version":"1.0.0","entry":"themes/t.json"}"#,
        )
        .unwrap();
        std::fs::write(
            builtin.join("themes/t.json"),
            r##"{"version":1,"name":"Builtin","themes":[{"name":"Builtin Dark","mode":"dark","colors":{"bg_base":"#101010","text_base":"#fafafa"}}]}"##,
        )
        .unwrap();
        std::fs::write(
            user.join(smelt_nsh::MANIFEST_FILE),
            r#"{"id":"smelt.themes","name":"Themes","version":"1.0.0","entry":"themes/t.json"}"#,
        )
        .unwrap();
        std::fs::write(
            user.join("themes/t.json"),
            r##"{"version":1,"name":"User","themes":[{"name":"User Dark","mode":"dark","colors":{"bg_base":"#202020","text_base":"#fafafa"}}]}"##,
        )
        .unwrap();

        cx.update(|cx| {
            let owners = register_themes_from_dirs(
                Some(&scratch.join("user")),
                Some(&scratch.join("builtin")),
                cx,
            );
            assert_eq!(owners, vec!["files:smelt.themes"]);
            assert!(smelt_ui::theme_catalog::contains("User Dark", cx));
            assert!(!smelt_ui::theme_catalog::contains("Builtin Dark", cx));
            for owner in owners {
                smelt_ui::theme_catalog::unregister_themes(&owner, cx);
            }

            // 用户主题非法（保留名）：回退登记自带 Builtin Dark。
            std::fs::write(
                user.join("themes/t.json"),
                r##"{"version":1,"name":"Copy","themes":[{"name":"smelt Dark","mode":"dark","colors":{"bg_base":"#202020","text_base":"#fafafa"}}]}"##,
            )
            .unwrap();
            let fallback_owners = register_themes_from_dirs(
                Some(&scratch.join("user")),
                Some(&scratch.join("builtin")),
                cx,
            );
            assert_eq!(fallback_owners, vec!["files:smelt.themes"]);
            assert!(smelt_ui::theme_catalog::contains("Builtin Dark", cx));
            for owner in fallback_owners {
                smelt_ui::theme_catalog::unregister_themes(&owner, cx);
            }
        });
        let _ = std::fs::remove_dir_all(scratch);
    }
}
