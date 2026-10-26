//! `agent.memory.watch`：监听客户端关注的智能体的记忆目录，有文件变化就发
//! `agent_memory.changed`。
//!
//! 记忆文件在运行时那台机器上，客户端监听不到，只能由运行时来看。一个智能体读两层：它自己的
//! （`~/.smelt/agents/<id>/memory`）和用户全局的（`~/.smelt/memory`，所有智能体共用，只监听
//! 一份）。目录还没有就先建好，不然监听不了，智能体第一次写记忆时客户端就看不到。一批事件
//! 合并成一次通知。监听一旦建立就保持到运行时退出，和 `project.watch` 一样。

use std::collections::HashMap;
use std::path::Path;
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;

use notify::{RecursiveMode, Watcher as _};

use crate::event_hub::EventHubHandle;

/// 一批文件事件攒这么久再通知一次：智能体写一条记忆会连着改好几次文件。
const DEBOUNCE: Duration = Duration::from_millis(200);

/// 键是层：`Some(智能体 id)` 是那个智能体自己的，`None` 是用户全局的。
type Watches = HashMap<Option<String>, notify::RecommendedWatcher>;

fn watches() -> &'static Mutex<Watches> {
    static WATCHES: OnceLock<Mutex<Watches>> = OnceLock::new();
    WATCHES.get_or_init(|| Mutex::new(HashMap::new()))
}

/// 关注 `agent_id` 读的两层记忆。幂等。
pub(crate) fn watch(agent_id: &str, event_hub: &EventHubHandle) -> Result<(), String> {
    let agent_dir = smelt_core::agent_definition_store::agent_memory_dir(agent_id)
        .ok_or_else(|| format!("智能体 id 不能当目录名：{agent_id}"))?;
    let user_dir = smelt_core::agent_definition_store::user_memory_dir()
        .ok_or_else(|| "找不到用户全局记忆目录".to_string())?;
    watch_layer(Some(agent_id.to_string()), &agent_dir, event_hub)?;
    watch_layer(None, &user_dir, event_hub)
}

fn watch_layer(
    layer: Option<String>,
    dir: &Path,
    event_hub: &EventHubHandle,
) -> Result<(), String> {
    let mut watches = watches().lock().map_err(|error| error.to_string())?;
    if watches.contains_key(&layer) {
        return Ok(());
    }
    std::fs::create_dir_all(dir)
        .map_err(|error| format!("无法创建记忆目录 {}：{error}", dir.display()))?;
    // 容量 1：一批事件只需要一次唤醒，不在队列里堆积。
    let (dirty_tx, dirty_rx) = std::sync::mpsc::sync_channel::<()>(1);
    let mut watcher = notify::recommended_watcher(move |res: notify::Result<notify::Event>| {
        if res.is_ok() {
            let _ = dirty_tx.try_send(());
        }
    })
    .map_err(|error| format!("监听 {} 失败：{error}", dir.display()))?;
    watcher
        .watch(dir, RecursiveMode::Recursive)
        .map_err(|error| format!("监听 {} 失败：{error}", dir.display()))?;
    watches.insert(layer.clone(), watcher);
    let hub = Arc::clone(event_hub);
    std::thread::Builder::new()
        .name("smeltd-memory-watch".into())
        .spawn(move || {
            while dirty_rx.recv().is_ok() {
                std::thread::sleep(DEBOUNCE);
                while dirty_rx.try_recv().is_ok() {}
                if let Err(error) = hub.publish_agent_memory_changed(layer.as_deref()) {
                    eprintln!("[memory-watch] 通知记忆变化失败：{error}");
                }
            }
        })
        .map_err(|error| format!("启动监听线程失败：{error}"))?;
    Ok(())
}
