//! 用户插件的导入和删除：只动 `~/.smelt/plugins/`，App 自带的插件不归这里管。
//!
//! 导入是把一个插件目录复制进来，之后和原目录无关。已经装了同 id 的插件时先交回
//! [`Import::Conflict`] 不动磁盘，用户确认覆盖后再导一次：旧的挪进回收目录，新的换上。
//! 删除也是挪进回收目录，不直接抹掉。都只改磁盘；改完由调用方 `mount_all` 重新加载。
//! 都是阻塞 IO，放后台线程调。

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use gpui_shell::plugin::{MANIFEST_FILE, PluginManifest};

use super::PLUGIN_DIR;

/// 复制时不带的条目：版本库，以及 smelt 加载插件时给编辑器生成的文件（到新位置会重新生成）。
const SKIPPED_AT_ROOT: &[&str] = &[".git", "gpui-kit.d.ts", "jsconfig.json"];

/// 用户插件目录 `~/.smelt/plugins/`。
pub(crate) fn user_plugin_dir() -> Option<PathBuf> {
    smelt_paths::smelt_home().map(|home| home.join(PLUGIN_DIR))
}

/// 删除的插件挪到这里。
fn trash_dir() -> Option<PathBuf> {
    smelt_paths::smelt_home().map(|home| home.join("trash").join(PLUGIN_DIR))
}

/// 一次导入的结果。
#[derive(Debug)]
pub(crate) enum Import {
    Imported {
        id: String,
        name: String,
    },
    /// 已经装了同 id 的插件，磁盘没动。用户确认后带上 `overwrite` 再导一次。
    Conflict(Conflict),
}

/// 要导入的插件和已经装着的那个同 id。
#[derive(Clone, Debug)]
pub(crate) struct Conflict {
    pub(crate) name: String,
    /// 要导入的版本；清单里没写是空。
    pub(crate) version: String,
    /// 装着的那个的版本；清单里没写是空。
    pub(crate) installed_version: String,
}

/// 把 `source` 目录里的插件复制进用户插件目录。`overwrite` 为 false 时碰到同 id 的就交回
/// [`Import::Conflict`]；为 true 时把装着的挪进回收目录，换成这个。
pub(crate) fn import(source: &Path, overwrite: bool) -> Result<Import, String> {
    let plugins = user_plugin_dir().ok_or("找不到 smelt 数据目录")?;
    let trash = trash_dir().ok_or("找不到 smelt 数据目录")?;
    import_into(source, &plugins, &trash, overwrite)
}

/// 把用户插件目录里的 `root` 挪进回收目录。
pub(crate) fn uninstall(root: &Path) -> Result<(), String> {
    let plugins = user_plugin_dir().ok_or("找不到 smelt 数据目录")?;
    let trash = trash_dir().ok_or("找不到 smelt 数据目录")?;
    uninstall_into(root, &plugins, &trash).map(|_| ())
}

fn import_into(
    source: &Path,
    plugins: &Path,
    trash: &Path,
    overwrite: bool,
) -> Result<Import, String> {
    let manifest = PluginManifest::read(source).map_err(|error| {
        if source.join(MANIFEST_FILE).is_file() {
            format!("插件清单有问题：{error}")
        } else {
            format!("这个目录里没有 {MANIFEST_FILE}，不是 smelt 插件")
        }
    })?;
    if !source.join(manifest.entry()).is_file() {
        return Err(format!("插件入口 {} 不存在", manifest.entry()));
    }

    fs::create_dir_all(plugins).map_err(|error| format!("创建插件目录失败：{error}"))?;
    let canonical_plugins = fs::canonicalize(plugins).map_err(|error| error.to_string())?;
    let canonical_source = fs::canonicalize(source).map_err(|error| error.to_string())?;
    if canonical_source.starts_with(&canonical_plugins) {
        return Err("这个插件已经在插件目录里了".to_string());
    }
    let existing = installed_root(plugins, manifest.id());
    if let Some(existing) = &existing
        && !overwrite
    {
        let installed_version = PluginManifest::read(existing)
            .map(|installed| installed.version().to_string())
            .unwrap_or_default();
        return Ok(Import::Conflict(Conflict {
            name: manifest.name().to_string(),
            version: manifest.version().to_string(),
            installed_version,
        }));
    }

    // id 只含小写字母、数字和 `.-_`，不以点开头，直接当目录名。装着的那个可能是手动放进来、
    // 目录名不是 id 的；它会被挪走，所以只有占着这个名字的不是它时才算冲突。
    let target = plugins.join(manifest.id());
    if fs::symlink_metadata(&target).is_ok() && existing.as_deref() != Some(target.as_path()) {
        return Err(format!("{} 已经存在", target.display()));
    }
    // 先复制到点开头的暂存目录（发现插件时跳过它），完整了再一次改名到位：
    // 复制到一半失败或重新加载，都不会出现一个半截的插件。
    let staging = plugins.join(format!(".importing-{}", manifest.id()));
    if fs::symlink_metadata(&staging).is_ok() {
        fs::remove_dir_all(&staging)
            .map_err(|error| format!("清理上次没导完的目录失败：{error}"))?;
    }
    if let Err(error) = copy_tree(source, &staging, true) {
        let _ = fs::remove_dir_all(&staging);
        return Err(format!("复制插件失败：{error}"));
    }
    // 新的完整复制好了才挪走旧的，复制失败时装着的那个原样不动。
    if let Some(existing) = &existing
        && let Err(error) = uninstall_into(existing, plugins, trash)
    {
        let _ = fs::remove_dir_all(&staging);
        return Err(format!("挪走旧版本失败：{error}"));
    }
    if let Err(error) = fs::rename(&staging, &target) {
        let _ = fs::remove_dir_all(&staging);
        return Err(match existing {
            Some(_) => format!("换上新版本失败：{error}；旧版本在 {} 里", trash.display()),
            None => format!("复制插件失败：{error}"),
        });
    }
    Ok(Import::Imported {
        id: manifest.id().to_string(),
        name: manifest.name().to_string(),
    })
}

/// 用户插件目录里 id 为 `id` 的插件。
fn installed_root(plugins: &Path, id: &str) -> Option<PathBuf> {
    fs::read_dir(plugins)
        .ok()?
        .flatten()
        .map(|entry| entry.path())
        .filter(|root| {
            !root
                .file_name()
                .is_some_and(|name| name.to_string_lossy().starts_with('.'))
        })
        .find(|root| PluginManifest::read(root).is_ok_and(|manifest| manifest.id() == id))
}

/// 复制目录树。软链不复制：插件只能读主目录，带进一条指向别处的链接，不是坏掉就是越界。
fn copy_tree(from: &Path, to: &Path, root: bool) -> io::Result<()> {
    fs::create_dir(to)?;
    for entry in fs::read_dir(from)? {
        let entry = entry?;
        let name = entry.file_name();
        if root && SKIPPED_AT_ROOT.iter().any(|skipped| name == *skipped) {
            continue;
        }
        // `DirEntry::file_type` 不跟随软链。
        let file_type = entry.file_type()?;
        let target = to.join(&name);
        if file_type.is_dir() {
            copy_tree(&entry.path(), &target, false)?;
        } else if file_type.is_file() {
            fs::copy(entry.path(), &target)?;
        }
    }
    Ok(())
}

/// 返回插件在回收目录里的新位置。
fn uninstall_into(root: &Path, plugins: &Path, trash: &Path) -> Result<PathBuf, String> {
    // 只认用户插件目录的直接子项：App 自带的、仓库里的插件都不能从这里删。
    if root.parent() != Some(plugins) {
        return Err("只能删除自己装的插件".to_string());
    }
    let name = root
        .file_name()
        .ok_or("插件路径不对")?
        .to_string_lossy()
        .into_owned();
    fs::create_dir_all(trash).map_err(|error| format!("创建回收目录失败：{error}"))?;
    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|elapsed| elapsed.as_millis())
        .unwrap_or_default();
    let target = trash.join(format!("{name}-{stamp}"));
    // 改名作用于路径本身：手动放进来的软链只挪走链接，链接指向的目录原样不动。
    fs::rename(root, &target).map_err(|error| format!("删除失败：{error}"))?;
    Ok(target)
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Scratch(PathBuf);

    impl Scratch {
        fn new() -> Self {
            let dir = std::env::temp_dir().join(format!(
                "smelt-plugin-manage-{}-{}",
                std::process::id(),
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap()
                    .as_nanos()
            ));
            fs::create_dir_all(&dir).unwrap();
            Self(fs::canonicalize(dir).unwrap())
        }

        fn path(&self, relative: &str) -> PathBuf {
            self.0.join(relative)
        }

        fn write(&self, relative: &str, contents: &str) {
            let path = self.path(relative);
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(path, contents).unwrap();
        }

        fn plugin(&self, relative: &str, id: &str) -> PathBuf {
            self.write(
                &format!("{relative}/{MANIFEST_FILE}"),
                &format!(r#"{{"id":"{id}","name":"Demo","version":"0.1.0","entry":"main.js"}}"#),
            );
            self.write(&format!("{relative}/main.js"), "export default class {}");
            self.path(relative)
        }
    }

    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn import_copies_the_plugin_under_its_id() {
        let scratch = Scratch::new();
        let source = scratch.plugin("src/demo", "com.example.demo");
        scratch.write("src/demo/lib/util.js", "export const x = 1;");
        let plugins = scratch.path("plugins");

        let imported = import_into(&source, &plugins, &scratch.path("trash"), false).unwrap();

        assert!(
            matches!(&imported, Import::Imported { id, .. } if id == "com.example.demo"),
            "{imported:?}"
        );
        let target = plugins.join("com.example.demo");
        assert!(target.join(MANIFEST_FILE).is_file());
        assert!(target.join("lib/util.js").is_file());
        assert!(source.join("main.js").is_file(), "原目录不动");
        assert!(!plugins.join(".importing-com.example.demo").exists());
    }

    #[test]
    fn import_leaves_out_git_editor_files_and_symlinks() {
        let scratch = Scratch::new();
        let source = scratch.plugin("src/demo", "com.example.demo");
        scratch.write("src/demo/.git/HEAD", "ref: refs/heads/main");
        scratch.write("src/demo/gpui-kit.d.ts", "declare module 'x' {}");
        scratch.write("src/demo/jsconfig.json", "{}");
        scratch.write("outside/secret.txt", "secret");
        #[cfg(unix)]
        std::os::unix::fs::symlink(scratch.path("outside"), source.join("linked")).unwrap();
        let plugins = scratch.path("plugins");

        import_into(&source, &plugins, &scratch.path("trash"), false).unwrap();

        let target = plugins.join("com.example.demo");
        assert!(!target.join(".git").exists());
        assert!(!target.join("gpui-kit.d.ts").exists());
        assert!(!target.join("jsconfig.json").exists());
        assert!(fs::symlink_metadata(target.join("linked")).is_err());
    }

    #[test]
    fn import_refuses_a_directory_without_a_valid_manifest() {
        let scratch = Scratch::new();
        scratch.write("src/empty/readme.md", "hi");
        scratch.write(&format!("src/broken/{MANIFEST_FILE}"), "{");
        scratch.write(
            &format!("src/no-entry/{MANIFEST_FILE}"),
            r#"{"id":"com.example.x","name":"X","version":"0.1.0","entry":"main.js"}"#,
        );
        let plugins = scratch.path("plugins");
        let trash = scratch.path("trash");

        let missing = import_into(&scratch.path("src/empty"), &plugins, &trash, false).unwrap_err();
        assert!(missing.contains("不是 smelt 插件"), "{missing}");
        let broken = import_into(&scratch.path("src/broken"), &plugins, &trash, false).unwrap_err();
        assert!(broken.contains("清单有问题"), "{broken}");
        let no_entry =
            import_into(&scratch.path("src/no-entry"), &plugins, &trash, false).unwrap_err();
        assert!(no_entry.contains("main.js"), "{no_entry}");
    }

    #[test]
    fn import_of_an_installed_id_asks_first_and_leaves_the_disk_alone() {
        let scratch = Scratch::new();
        let installed = scratch.plugin("plugins/com.example.demo", "com.example.demo");
        let source = scratch.plugin("src/demo", "com.example.demo");
        scratch.write("src/demo/new.js", "");

        let outcome = import_into(
            &source,
            &scratch.path("plugins"),
            &scratch.path("trash"),
            false,
        )
        .unwrap();

        let Import::Conflict(conflict) = outcome else {
            panic!("应该先问：{outcome:?}");
        };
        assert_eq!(conflict.name, "Demo");
        assert_eq!(conflict.installed_version, "0.1.0");
        assert!(!installed.join("new.js").exists(), "确认前不动装着的");
        assert!(!scratch.path("trash").exists());
    }

    #[test]
    fn overwrite_moves_the_installed_copy_to_the_trash_and_puts_the_new_one_under_its_id() {
        let scratch = Scratch::new();
        // 手动 clone 进来的，目录名和 id 不一样。
        let installed = scratch.plugin("plugins/project-stats", "com.example.demo");
        scratch.write("plugins/project-stats/old.js", "");
        let source = scratch.plugin("src/demo", "com.example.demo");
        scratch.write("src/demo/new.js", "");
        let plugins = scratch.path("plugins");
        let trash = scratch.path("trash");

        let outcome = import_into(&source, &plugins, &trash, true).unwrap();

        assert!(matches!(outcome, Import::Imported { .. }), "{outcome:?}");
        assert!(!installed.exists());
        let target = plugins.join("com.example.demo");
        assert!(target.join("new.js").is_file());
        assert!(!target.join("old.js").exists(), "不是合并，整个换掉");
        let trashed: Vec<_> = fs::read_dir(&trash).unwrap().flatten().collect();
        assert_eq!(trashed.len(), 1);
        assert!(
            trashed[0].path().join("old.js").is_file(),
            "旧的在回收目录里"
        );
    }

    #[cfg(unix)]
    #[test]
    fn overwrite_keeps_the_installed_copy_when_the_new_one_cannot_be_copied() {
        use std::os::unix::fs::PermissionsExt;
        let scratch = Scratch::new();
        let installed = scratch.plugin("plugins/com.example.demo", "com.example.demo");
        let source = scratch.plugin("src/demo", "com.example.demo");
        // 读不了的文件：复制到一半失败。
        scratch.write("src/demo/locked.js", "");
        let locked = source.join("locked.js");
        fs::set_permissions(&locked, fs::Permissions::from_mode(0o000)).unwrap();

        let outcome = import_into(
            &source,
            &scratch.path("plugins"),
            &scratch.path("trash"),
            true,
        );
        fs::set_permissions(&locked, fs::Permissions::from_mode(0o644)).unwrap();

        assert!(outcome.is_err(), "{outcome:?}");
        assert!(
            installed.join("main.js").is_file(),
            "复制失败时装着的原样不动"
        );
        assert!(!scratch.path("trash").exists());
        assert!(!scratch.path("plugins/.importing-com.example.demo").exists());
    }

    #[test]
    fn import_refuses_a_plugin_already_in_the_plugin_dir() {
        let scratch = Scratch::new();
        let installed = scratch.plugin("plugins/demo", "com.example.demo");

        let error = import_into(
            &installed,
            &scratch.path("plugins"),
            &scratch.path("trash"),
            true,
        )
        .unwrap_err();
        assert!(error.contains("已经在插件目录里"), "{error}");
    }

    #[test]
    fn uninstall_moves_the_plugin_into_the_trash() {
        let scratch = Scratch::new();
        let root = scratch.plugin("plugins/demo", "com.example.demo");

        let moved =
            uninstall_into(&root, &scratch.path("plugins"), &scratch.path("trash")).unwrap();

        assert!(!root.exists());
        assert!(moved.starts_with(scratch.path("trash")));
        assert!(moved.join(MANIFEST_FILE).is_file());
    }

    #[cfg(unix)]
    #[test]
    fn uninstall_of_a_symlinked_plugin_only_moves_the_link() {
        let scratch = Scratch::new();
        let checkout = scratch.plugin("dev/demo", "com.example.demo");
        fs::create_dir_all(scratch.path("plugins")).unwrap();
        let link = scratch.path("plugins/demo");
        std::os::unix::fs::symlink(&checkout, &link).unwrap();

        uninstall_into(&link, &scratch.path("plugins"), &scratch.path("trash")).unwrap();

        assert!(fs::symlink_metadata(&link).is_err());
        assert!(checkout.join(MANIFEST_FILE).is_file(), "开发目录原样不动");
    }

    #[test]
    fn uninstall_refuses_anything_outside_the_user_plugin_dir() {
        let scratch = Scratch::new();
        let bundled = scratch.plugin("bundled/skills", "com.smelt.skills");
        let nested = scratch.plugin("plugins/demo/inner", "com.example.inner");

        for root in [bundled, nested] {
            let error = uninstall_into(&root, &scratch.path("plugins"), &scratch.path("trash"))
                .unwrap_err();
            assert!(error.contains("只能删除"), "{error}");
            assert!(root.join(MANIFEST_FILE).is_file());
        }
    }
}
