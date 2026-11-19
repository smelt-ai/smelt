//! 插件目录里文件一变，就重挂改动的那个插件：写插件的人（和智能体）保存即生效，导入、删除
//! 插件也靠它挂上、撤下。
//!
//! 盯的是用户插件目录 `~/.smelt/plugins/`，开发构建还盯仓库里的自带插件 `plugins/`。靠文件系统事件（macOS 是 FSEvents），不轮询。
//!
//! 加载插件时 gpui-shell 会往插件目录里写给编辑器的文件（`gpui-kit.d.ts`、`jsconfig.json`、
//! `node_modules/` 里的依赖链接），这些变化不算，否则每次重挂都会再触发一次。点开头的也
//! 不算：编辑器的交换文件、`.git`、导入时的暂存目录。

use std::collections::BTreeSet;
use std::ffi::OsStr;
use std::path::{Component, Path, PathBuf};
use std::time::Duration;

use gpui::{Context, Window};
use notify::Watcher as _;

/// 一次保存常是一串事件（写临时文件、改名、改属性），也可能一次改好几个文件：静下来这么久
/// 才重挂，一串只挂一次。
const QUIET: Duration = Duration::from_millis(200);

/// 加载插件时写进插件目录的、给编辑器看的文件和目录。
const GENERATED: &[&str] = &["gpui-kit.d.ts", "jsconfig.json", "node_modules"];

/// 开始盯插件目录，返回的监听器丢掉就停。`T` 是挂插件的那扇窗口（工作台）的视图。
pub(crate) fn watch<T: 'static>(
    window: &mut Window,
    cx: &mut Context<T>,
) -> Option<notify::RecommendedWatcher> {
    let dirs = watched_dirs();
    if dirs.is_empty() {
        return None;
    }
    let (changed_tx, changed_rx) = smol::channel::unbounded::<PathBuf>();
    let matcher = dirs.clone();
    let watcher = notify::recommended_watcher(move |event: notify::Result<notify::Event>| {
        let Ok(event) = event else {
            return;
        };
        if matches!(event.kind, notify::EventKind::Access(_)) {
            return;
        }
        for path in &event.paths {
            if let Some(root) = plugin_root(path, &matcher) {
                let _ = changed_tx.try_send(root);
            }
        }
    });
    let mut watcher = match watcher {
        Ok(watcher) => watcher,
        Err(error) => {
            eprintln!("[plugins] 监听插件目录失败：{error}");
            return None;
        }
    };
    for dir in &dirs {
        if let Err(error) = watcher.watch(&dir.original, notify::RecursiveMode::Recursive) {
            eprintln!("[plugins] 监听 {} 失败：{error}", dir.original.display());
        }
    }
    // 监听器丢掉时发送端跟着没了，循环随之结束。
    cx.spawn_in(window, async move |_, cx| {
        while let Ok(first) = changed_rx.recv().await {
            let mut roots = BTreeSet::from([first]);
            loop {
                let quiet = cx.background_executor().timer(QUIET);
                let more = async { changed_rx.recv().await.ok() };
                let settled = async {
                    quiet.await;
                    None
                };
                match smol::future::or(more, settled).await {
                    Some(root) => {
                        roots.insert(root);
                    }
                    None => break,
                }
            }
            if cx
                .update(|window, cx| super::reload_changed(&roots, window, cx))
                .is_err()
            {
                break;
            }
        }
    })
    .detach();
    Some(watcher)
}

/// 盯着的一个插件目录。事件里的路径可能是解开软链后的（macOS 的 FSEvents 报真实路径），
/// 两种写法都认；交给重挂的一律用发现插件时的写法（`original`）。
#[derive(Clone, Debug)]
struct WatchedDir {
    original: PathBuf,
    canonical: PathBuf,
}

fn watched_dirs() -> Vec<WatchedDir> {
    let mut dirs = Vec::new();
    if let Some(user) = super::user_plugin_dir() {
        // 还没有插件时目录可能不在；先建好，第一个插件写进来就能看见。
        if let Err(error) = std::fs::create_dir_all(&user) {
            eprintln!("[plugins] 创建 {} 失败：{error}", user.display());
        } else {
            dirs.push(user);
        }
    }
    // 正式构建的自带插件是 App 写出来的，不会有人改。
    #[cfg(debug_assertions)]
    dirs.push(super::builtin::repo_dir());
    dirs.into_iter()
        .filter(|dir| dir.is_dir())
        .map(|original| WatchedDir {
            canonical: std::fs::canonicalize(&original).unwrap_or_else(|_| original.clone()),
            original,
        })
        .collect()
}

/// 事件路径属于哪个插件目录（`<插件目录>/<名字>`）；不算数的变化是 `None`。
fn plugin_root(path: &Path, dirs: &[WatchedDir]) -> Option<PathBuf> {
    let (dir, relative) = dirs.iter().find_map(|dir| {
        path.strip_prefix(&dir.original)
            .or_else(|_| path.strip_prefix(&dir.canonical))
            .ok()
            .map(|relative| (dir, relative))
    })?;
    let mut components = relative.components();
    let Some(Component::Normal(name)) = components.next() else {
        return None;
    };
    if is_hidden(name) {
        return None;
    }
    let ignored = components.any(|component| match component {
        Component::Normal(part) => is_hidden(part) || GENERATED.iter().any(|&g| part == g),
        _ => false,
    });
    (!ignored).then(|| dir.original.join(name))
}

fn is_hidden(name: &OsStr) -> bool {
    name.to_string_lossy().starts_with('.')
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dirs() -> Vec<WatchedDir> {
        vec![WatchedDir {
            original: PathBuf::from("/home/me/.smelt/plugins"),
            canonical: PathBuf::from("/data/me/.smelt/plugins"),
        }]
    }

    fn root(path: &str) -> Option<PathBuf> {
        plugin_root(Path::new(path), &dirs())
    }

    #[test]
    fn a_change_inside_a_plugin_names_that_plugin() {
        let hello = Some(PathBuf::from("/home/me/.smelt/plugins/hello"));
        assert_eq!(root("/home/me/.smelt/plugins/hello/main.js"), hello);
        assert_eq!(root("/home/me/.smelt/plugins/hello/src/ui.js"), hello);
        // 新建的插件目录本身。
        assert_eq!(root("/home/me/.smelt/plugins/hello"), hello);
        // 事件报的是解开软链后的路径，交回去的仍是发现插件时的写法。
        assert_eq!(root("/data/me/.smelt/plugins/hello/main.js"), hello);
    }

    #[test]
    fn generated_and_hidden_files_do_not_count() {
        assert_eq!(root("/home/me/.smelt/plugins/hello/gpui-kit.d.ts"), None);
        assert_eq!(
            root("/home/me/.smelt/plugins/hello/src/gpui-kit.d.ts"),
            None
        );
        assert_eq!(root("/home/me/.smelt/plugins/hello/jsconfig.json"), None);
        assert_eq!(
            root("/home/me/.smelt/plugins/hello/node_modules/dep/index.js"),
            None
        );
        assert_eq!(root("/home/me/.smelt/plugins/hello/.main.js.swp"), None);
        assert_eq!(root("/home/me/.smelt/plugins/hello/.git/index"), None);
        // 导入时的暂存目录。
        assert_eq!(
            root("/home/me/.smelt/plugins/.importing-hello/main.js"),
            None
        );
        // 插件目录本身和别处的变化。
        assert_eq!(root("/home/me/.smelt/plugins"), None);
        assert_eq!(
            root("/home/me/.smelt/plugin-data/hello/store.sqlite3"),
            None
        );
    }
}
