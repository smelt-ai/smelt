//! `project.watch`：监听客户端关注的项目目录，有文件变化就发 `project.changed`。
//!
//! 文件在运行时那台机器上，客户端监听不到，只能由运行时来看。递归监听整棵目录树
//! （含 `.git/` 内部），但滤掉构建产物、依赖目录：rust-analyzer、cargo 持续写
//! `target/`，不滤的话每次触碰都会让客户端重跑一次 git status。一批事件合并成一次通知。
//! 监听一旦建立就保持到运行时退出。

use std::collections::HashMap;
use std::path::Path;
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;

use notify::{RecursiveMode, Watcher as _};

use crate::event_hub::EventHubHandle;

/// 一批文件事件攒这么久再通知一次。
const DEBOUNCE: Duration = Duration::from_millis(200);

fn watches() -> &'static Mutex<HashMap<String, notify::RecommendedWatcher>> {
    static WATCHES: OnceLock<Mutex<HashMap<String, notify::RecommendedWatcher>>> = OnceLock::new();
    WATCHES.get_or_init(|| Mutex::new(HashMap::new()))
}

/// 构建产物 / 依赖目录变化极频繁，监听它们只会让 git status 反复空跑。
fn is_noise_path(path: &Path) -> bool {
    path.components().any(|component| {
        component.as_os_str().to_str().is_some_and(|name| {
            matches!(
                name,
                "target" | "node_modules" | "dist" | "build" | ".next" | ".cache"
            )
        })
    })
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
    let mut watcher = notify::recommended_watcher(move |res: notify::Result<notify::Event>| {
        if let Ok(event) = res
            && !event.paths.iter().any(|path| is_noise_path(path))
        {
            let _ = dirty_tx.try_send(());
        }
    })
    .map_err(|error| format!("监听 {root} 失败：{error}"))?;
    watcher
        .watch(Path::new(root), RecursiveMode::Recursive)
        .map_err(|error| format!("监听 {root} 失败：{error}"))?;
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
    use super::is_noise_path;
    use std::path::Path;

    #[test]
    fn build_output_and_dependencies_are_noise() {
        assert!(is_noise_path(Path::new("/repo/target/debug/smelt")));
        assert!(is_noise_path(Path::new(
            "/repo/web/node_modules/x/index.js"
        )));
        assert!(!is_noise_path(Path::new("/repo/src/main.rs")));
        assert!(!is_noise_path(Path::new("/repo/.git/index")));
    }
}
