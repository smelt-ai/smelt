//! `project.watch`：监听客户端关注的项目目录，有文件变化就发 `project.changed`。
//!
//! 文件在运行时那台机器上，客户端监听不到，只能由运行时来看。滤掉构建产物、依赖目录：
//! rust-analyzer、cargo 持续写 `target/`，不滤的话每次触碰都会让客户端重跑一次 git
//! status。一批事件合并成一次通知。监听一旦建立就保持到运行时退出。
//!
//! 递归监听的代价各平台不同。macOS 的 FSEvents 一条流管整棵树，直接递归。Linux 的
//! inotify 每个目录占一个 watch，受 `fs.inotify.max_user_watches` 限制，带
//! `node_modules` 的仓库一递归就能用光：噪音目录只在事件层滤掉没用，watch 照样占着。
//! 所以别的平台自己遍历，只给非噪音目录逐个加非递归 watch，新建的目录在事件里补上；
//! `.git` 只看它本身（index、HEAD）和 `refs`。上限用完就只监听已经加上的那部分。

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;

use notify::{RecursiveMode, Watcher as _};

use crate::event_hub::EventHubHandle;

/// 一批文件事件攒这么久再通知一次。
const DEBOUNCE: Duration = Duration::from_millis(200);

type SharedWatcher = Arc<Mutex<notify::RecommendedWatcher>>;

fn watches() -> &'static Mutex<HashMap<String, SharedWatcher>> {
    static WATCHES: OnceLock<Mutex<HashMap<String, SharedWatcher>>> = OnceLock::new();
    WATCHES.get_or_init(|| Mutex::new(HashMap::new()))
}

const NOISE_DIRS: [&str; 6] = ["target", "node_modules", "dist", "build", ".next", ".cache"];

/// 构建产物 / 依赖目录变化极频繁，监听它们只会让 git status 反复空跑。
fn is_noise_path(path: &Path) -> bool {
    path.components().any(|component| {
        component
            .as_os_str()
            .to_str()
            .is_some_and(|name| NOISE_DIRS.contains(&name))
    })
}

/// `.git` 里只有这些变化影响 git status / 分支显示。
fn git_dir_targets(git_dir: &Path) -> Vec<(PathBuf, RecursiveMode)> {
    vec![
        (git_dir.to_path_buf(), RecursiveMode::NonRecursive),
        (git_dir.join("refs"), RecursiveMode::Recursive),
    ]
}

/// 非 macOS 上要加 watch 的目录：`dir` 本身和它下面所有非噪音目录，`.git` 只取关键处。
fn watch_targets(dir: &Path) -> Vec<(PathBuf, RecursiveMode)> {
    let mut targets = Vec::new();
    let mut pending = vec![dir.to_path_buf()];
    while let Some(dir) = pending.pop() {
        if dir.file_name().is_some_and(|name| name == ".git") {
            targets.extend(git_dir_targets(&dir));
            continue;
        }
        targets.push((dir.clone(), RecursiveMode::NonRecursive));
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            // 不跟符号链接：链出去的目录不属于这个项目，还可能成环。
            let Ok(file_type) = entry.file_type() else {
                continue;
            };
            let name = entry.file_name();
            if file_type.is_dir() && !name.to_str().is_some_and(|name| NOISE_DIRS.contains(&name)) {
                pending.push(entry.path());
            }
        }
    }
    targets
}

/// 加 watch。inotify 上限用完时停下并记一次日志，已经加上的照常工作。
fn add_watches(watcher: &SharedWatcher, root: &str, targets: Vec<(PathBuf, RecursiveMode)>) {
    let mut watcher = watcher.lock().unwrap_or_else(|e| e.into_inner());
    for (path, mode) in targets {
        match watcher.watch(&path, mode) {
            Ok(()) => {}
            Err(error) if matches!(error.kind, notify::ErrorKind::MaxFilesWatch) => {
                crate::dlog(&format!(
                    "[project-watch] inotify watch 用完了（fs.inotify.max_user_watches），{root} 只监听了一部分目录"
                ));
                return;
            }
            // 遍历和加 watch 之间目录可能已被删掉，跳过即可。
            Err(_) => {}
        }
    }
}

pub(crate) fn watch(root: &str, event_hub: &EventHubHandle) -> Result<(), String> {
    if !Path::new(root).is_dir() {
        return Err(format!("不是目录：{root}"));
    }
    let mut watches = watches().lock().map_err(|error| error.to_string())?;
    if watches.contains_key(root) {
        return Ok(());
    }
    // 容量 1：一批事件只需要一次唤醒，保存、构建产生的事件不在队列里堆积。
    let (dirty_tx, dirty_rx) = std::sync::mpsc::sync_channel::<()>(1);
    // 新建的目录要补 watch：回调里拿不到 watcher 本身，交给下面的线程去加。
    let (created_tx, created_rx) = std::sync::mpsc::channel::<PathBuf>();
    let watcher = notify::recommended_watcher(move |res: notify::Result<notify::Event>| {
        let Ok(event) = res else {
            return;
        };
        if event.paths.iter().any(|path| is_noise_path(path)) {
            return;
        }
        if !cfg!(target_os = "macos") && matches!(event.kind, notify::EventKind::Create(_)) {
            for path in &event.paths {
                if path.is_dir() {
                    let _ = created_tx.send(path.clone());
                }
            }
        }
        let _ = dirty_tx.try_send(());
    })
    .map_err(|error| format!("监听 {root} 失败：{error}"))?;
    let watcher: SharedWatcher = Arc::new(Mutex::new(watcher));
    if cfg!(target_os = "macos") {
        watcher
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .watch(Path::new(root), RecursiveMode::Recursive)
            .map_err(|error| format!("监听 {root} 失败：{error}"))?;
    } else {
        add_watches(&watcher, root, watch_targets(Path::new(root)));
        let watcher = Arc::clone(&watcher);
        let root = root.to_string();
        std::thread::Builder::new()
            .name("smeltd-project-watch-dirs".into())
            .spawn(move || {
                while let Ok(dir) = created_rx.recv() {
                    add_watches(&watcher, &root, watch_targets(&dir));
                }
            })
            .map_err(|error| format!("启动监听线程失败：{error}"))?;
    }
    watches.insert(root.to_string(), watcher);
    let hub = Arc::clone(event_hub);
    let root = root.to_string();
    std::thread::Builder::new()
        .name("smeltd-project-watch".into())
        .spawn(move || {
            while dirty_rx.recv().is_ok() {
                std::thread::sleep(DEBOUNCE);
                while dirty_rx.try_recv().is_ok() {}
                if let Err(error) = hub.publish_project_changed(&root) {
                    eprintln!("[project-watch] 通知 {root} 变化失败：{error}");
                }
            }
        })
        .map_err(|error| format!("启动监听线程失败：{error}"))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn build_output_and_dependencies_are_noise() {
        assert!(is_noise_path(Path::new("/repo/target/debug/smelt")));
        assert!(is_noise_path(Path::new(
            "/repo/web/node_modules/x/index.js"
        )));
        assert!(!is_noise_path(Path::new("/repo/src/main.rs")));
        assert!(!is_noise_path(Path::new("/repo/.git/index")));
    }

    /// 噪音目录和 `.git/objects` 不占 watch；源码目录逐个非递归监听。
    #[test]
    fn watch_targets_skip_noise_and_git_internals() {
        let root = std::env::temp_dir().join(format!(
            "smelt-project-watch-{}",
            uuid::Uuid::new_v4().simple()
        ));
        for dir in [
            "src/nested",
            "web/node_modules/pkg",
            "target/debug",
            ".git/objects/ab",
            ".git/refs/heads",
        ] {
            std::fs::create_dir_all(root.join(dir)).unwrap();
        }
        let mut targets: Vec<_> = watch_targets(&root)
            .into_iter()
            .map(|(path, mode)| {
                (
                    path.strip_prefix(&root).unwrap().to_path_buf(),
                    mode == RecursiveMode::Recursive,
                )
            })
            .collect();
        targets.sort();
        assert_eq!(
            targets,
            vec![
                (PathBuf::new(), false),
                (PathBuf::from(".git"), false),
                (PathBuf::from(".git/refs"), true),
                (PathBuf::from("src"), false),
                (PathBuf::from("src/nested"), false),
                (PathBuf::from("web"), false),
            ]
        );
        std::fs::remove_dir_all(root).unwrap();
    }
}
