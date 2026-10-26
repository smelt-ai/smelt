//! 用户插件的导入和删除：只动 `~/.smelt/plugins/`，App 自带的插件不归这里管。
//!
//! 导入是把一个插件目录复制进来，之后和原目录无关。删除是挪进回收目录，不直接抹掉。
//! 两者都只改磁盘；改完由调用方 `mount_all` 重新加载。都是阻塞 IO，放后台线程调。

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

/// 把 `source` 目录里的插件复制进用户插件目录。返回插件名。
pub(crate) fn import(source: &Path) -> Result<String, String> {
    let plugins = user_plugin_dir().ok_or("找不到 smelt 数据目录")?;
    import_into(source, &plugins).map(|manifest| manifest.name().to_string())
}

/// 把用户插件目录里的 `root` 挪进回收目录。
pub(crate) fn uninstall(root: &Path) -> Result<(), String> {
    let plugins = user_plugin_dir().ok_or("找不到 smelt 数据目录")?;
    let trash = trash_dir().ok_or("找不到 smelt 数据目录")?;
    uninstall_into(root, &plugins, &trash).map(|_| ())
}

fn import_into(source: &Path, plugins: &Path) -> Result<PluginManifest, String> {
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
    if let Some(existing) = installed_root(plugins, manifest.id()) {
        return Err(format!(
            "已经装了 id 相同的插件（{}），先删掉它再导入",
            existing.display()
        ));
    }

    // id 只含小写字母、数字和 `.-_`，不以点开头，直接当目录名。
    let target = plugins.join(manifest.id());
    if fs::symlink_metadata(&target).is_ok() {
        return Err(format!("{} 已经存在", target.display()));
    }
    // 先复制到点开头的暂存目录（发现插件时跳过它），完整了再一次改名到位：
    // 复制到一半失败或重新加载，都不会出现一个半截的插件。
    let staging = plugins.join(format!(".importing-{}", manifest.id()));
    if fs::symlink_metadata(&staging).is_ok() {
        fs::remove_dir_all(&staging).map_err(|error| format!("清理上次没导完的目录失败：{error}"))?;
    }
    let copied = copy_tree(source, &staging, true).and_then(|()| fs::rename(&staging, &target));
    if let Err(error) = copied {
        let _ = fs::remove_dir_all(&staging);
        return Err(format!("复制插件失败：{error}"));
    }
    Ok(manifest)
}

/// 用户插件目录里 id 为 `id` 的插件。
fn installed_root(plugins: &Path, id: &str) -> Option<PathBuf> {
    fs::read_dir(plugins)
        .ok()?
        .flatten()
        .map(|entry| entry.path())
        .filter(|root| !root.file_name().is_some_and(|name| name.to_string_lossy().starts_with('.')))
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
    let name = root.file_name().ok_or("插件路径不对")?.to_string_lossy().into_owned();
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

        let manifest = import_into(&source, &plugins).unwrap();

        assert_eq!(manifest.id(), "com.example.demo");
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

        import_into(&source, &plugins).unwrap();

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

        let missing = import_into(&scratch.path("src/empty"), &plugins).unwrap_err();
        assert!(missing.contains("不是 smelt 插件"), "{missing}");
        let broken = import_into(&scratch.path("src/broken"), &plugins).unwrap_err();
        assert!(broken.contains("清单有问题"), "{broken}");
        let no_entry = import_into(&scratch.path("src/no-entry"), &plugins).unwrap_err();
        assert!(no_entry.contains("main.js"), "{no_entry}");
    }

    #[test]
    fn import_refuses_an_id_that_is_already_installed() {
        let scratch = Scratch::new();
        // 手动 clone 进来的，目录名和 id 不一样。
        scratch.plugin("plugins/project-stats", "com.example.demo");
        let source = scratch.plugin("src/demo", "com.example.demo");

        let error = import_into(&source, &scratch.path("plugins")).unwrap_err();
        assert!(error.contains("project-stats"), "{error}");
    }

    #[test]
    fn import_refuses_a_plugin_already_in_the_plugin_dir() {
        let scratch = Scratch::new();
        let installed = scratch.plugin("plugins/demo", "com.example.demo");

        let error = import_into(&installed, &scratch.path("plugins")).unwrap_err();
        assert!(error.contains("已经在插件目录里"), "{error}");
    }

    #[test]
    fn uninstall_moves_the_plugin_into_the_trash() {
        let scratch = Scratch::new();
        let root = scratch.plugin("plugins/demo", "com.example.demo");

        let moved = uninstall_into(&root, &scratch.path("plugins"), &scratch.path("trash")).unwrap();

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
            let error =
                uninstall_into(&root, &scratch.path("plugins"), &scratch.path("trash")).unwrap_err();
            assert!(error.contains("只能删除"), "{error}");
            assert!(root.join(MANIFEST_FILE).is_file());
        }
    }
}
