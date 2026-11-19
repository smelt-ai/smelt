//! App 自带的 JS 插件：仓库根 `plugins/` 下每个子目录一个，编进二进制，跟着 App 更新。
//!
//! gpui-shell 从目录加载脚本，所以正式构建启动时把编进来的文件写到
//! `~/.smelt/builtin-plugins/<内容摘要>/`，从那里加载：摘要相同的目录已经在就不再写，
//! 别的摘要是旧版本留下的，删掉。开发构建直接读仓库里的 `plugins/`，改了脚本即重挂（见 `watch`）。
//!
//! 自带插件只读：用户想改，复制一份、换个 id，再在布局里换上自己那份。和自带插件同 id 的
//! 用户插件不加载，报重复（见 `super::discover`）。

use std::path::{Path, PathBuf};
use std::sync::OnceLock;

/// 编进二进制的自带插件。加载时 gpui-shell 写给编辑器的文件不是插件内容，不带。
/// 开发构建从仓库读，用不上它，只在测试里核对。还没有自带插件时目录不在（空目录进不了
/// git），编进去的就是空的。
#[cfg(any(not(debug_assertions), test))]
#[derive(rust_embed::RustEmbed)]
#[folder = "../../plugins"]
#[allow_missing = true]
#[exclude = "*/gpui-kit.d.ts"]
#[exclude = "*/jsconfig.json"]
#[exclude = "*/node_modules/*"]
struct Embedded;

/// 写出来的自带插件放在 `~/.smelt` 下的这个目录。
#[cfg(not(debug_assertions))]
const DIR: &str = "builtin-plugins";

/// 自带插件所在的目录，下面每个子目录一个插件。写不出来时 `None`，原因记日志。
pub(super) fn dir() -> Option<&'static Path> {
    static DIR: OnceLock<Option<PathBuf>> = OnceLock::new();
    DIR.get_or_init(locate).as_deref()
}

/// 加载时要不要往插件目录里写给编辑器的文件（`gpui-kit.d.ts`、`jsconfig.json`）：
/// 写出来的那份没人在那里改，不写；开发构建读的仓库目录要写，改脚本时有补全。
pub(super) fn editor_files(root: &Path) -> bool {
    !(cfg!(not(debug_assertions)) && dir().is_some_and(|dir| root.starts_with(dir)))
}

#[cfg(debug_assertions)]
fn locate() -> Option<PathBuf> {
    Some(repo_dir())
}

#[cfg(not(debug_assertions))]
fn locate() -> Option<PathBuf> {
    let parent = smelt_paths::smelt_home()?.join(DIR);
    let files = Embedded::iter().filter_map(|path| {
        let file = Embedded::get(&path)?;
        Some((path.into_owned(), file.data))
    });
    match write_out(&parent, files.collect()) {
        Ok(dir) => Some(dir),
        Err(error) => {
            eprintln!(
                "[plugins] 写出自带插件到 {} 失败：{error}",
                parent.display()
            );
            None
        }
    }
}

/// 仓库里的自带插件。
#[cfg(any(debug_assertions, test))]
pub(super) fn repo_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../plugins")
}

/// 把 `files`（相对路径和内容）写到 `parent/<摘要>/`，返回那个目录。已经有就不写；
/// `parent` 下别的条目是旧版本的，删掉，删不掉不算失败。
#[cfg(any(not(debug_assertions), test))]
fn write_out(
    parent: &Path,
    mut files: Vec<(String, std::borrow::Cow<'static, [u8]>)>,
) -> std::io::Result<PathBuf> {
    files.sort_by(|a, b| a.0.cmp(&b.0));
    let dir = parent.join(digest(&files));
    if !dir.is_dir() {
        std::fs::create_dir_all(parent)?;
        // 写进暂存目录再整个改名：中途退出不会留下半份被当成写好了。
        let staging = parent.join(format!(".staging-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&staging);
        for (path, data) in &files {
            let target = staging.join(path);
            if let Some(parent) = target.parent() {
                std::fs::create_dir_all(parent)?;
            }
            std::fs::write(target, data)?;
        }
        if let Err(error) = std::fs::rename(&staging, &dir) {
            let _ = std::fs::remove_dir_all(&staging);
            // 另一个进程同时写好了同一份。
            if !dir.is_dir() {
                return Err(error);
            }
        }
    }
    for entry in std::fs::read_dir(parent)?.flatten() {
        let path = entry.path();
        let name = entry.file_name();
        // 别的进程正在写的暂存目录不碰。
        if path != dir && !name.to_string_lossy().starts_with('.') {
            let _ = std::fs::remove_dir_all(&path);
        }
    }
    Ok(dir)
}

/// 全部文件路径和内容的摘要。只用来分辨内容变没变，不必跨 Rust 版本稳定：
/// 摘要变了只是多写一份。
#[cfg(any(not(debug_assertions), test))]
fn digest(files: &[(String, std::borrow::Cow<'static, [u8]>)]) -> String {
    use std::hash::Hasher as _;
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    for (path, data) in files {
        hasher.write(path.as_bytes());
        hasher.write_u8(0);
        hasher.write(data);
        hasher.write_usize(data.len());
    }
    format!("{:016x}", hasher.finish())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::borrow::Cow;

    fn files(main: &'static str) -> Vec<(String, Cow<'static, [u8]>)> {
        vec![
            ("a/gpui-shell.json".into(), Cow::Borrowed(b"{}")),
            ("a/main.js".into(), Cow::Borrowed(main.as_bytes())),
        ]
    }

    #[test]
    fn writes_once_and_replaces_older_versions() {
        let parent = std::env::temp_dir().join(format!("smelt-builtin-{}", uuid::Uuid::new_v4()));

        let first = write_out(&parent, files("1")).unwrap();
        assert_eq!(
            std::fs::read_to_string(first.join("a/main.js")).unwrap(),
            "1"
        );
        // 同一份内容不重写：写在已有目录里的东西还在。
        std::fs::write(first.join("marker"), "").unwrap();
        assert_eq!(write_out(&parent, files("1")).unwrap(), first);
        assert!(first.join("marker").is_file());

        let second = write_out(&parent, files("2")).unwrap();
        assert_ne!(second, first);
        assert_eq!(
            std::fs::read_to_string(second.join("a/main.js")).unwrap(),
            "2"
        );
        assert!(!first.exists(), "旧版本的目录要删掉");
        let left: Vec<_> = std::fs::read_dir(&parent).unwrap().flatten().collect();
        assert_eq!(left.len(), 1, "暂存目录不能留下");

        std::fs::remove_dir_all(parent).unwrap();
    }

    /// 编进去的就是仓库里的：每个自带插件的清单都能读，入口在。
    #[test]
    fn embedded_plugins_match_the_repository() {
        let mut embedded: Vec<String> = Embedded::iter().map(|path| path.into_owned()).collect();
        embedded.sort();
        assert!(
            embedded.iter().all(|path| !path.ends_with("gpui-kit.d.ts")
                && !path.ends_with("jsconfig.json")
                && !path.contains("node_modules/")),
            "编辑器文件不该编进去：{embedded:?}"
        );
        let Ok(roots) = std::fs::read_dir(repo_dir()) else {
            assert!(
                embedded.is_empty(),
                "仓库里没有 plugins/，却编进去了 {embedded:?}"
            );
            return;
        };
        for root in roots.flatten() {
            let root = root.path();
            if !root.join(gpui_shell::plugin::MANIFEST_FILE).is_file() {
                continue;
            }
            let manifest = gpui_shell::plugin::PluginManifest::read(&root)
                .unwrap_or_else(|error| panic!("{}：{error}", root.display()));
            let name = root.file_name().unwrap().to_string_lossy();
            let entry = format!("{name}/{}", manifest.entry());
            assert!(embedded.contains(&entry), "{entry} 没编进去");
        }
    }
}
