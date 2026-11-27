//! `agent.memory.watch`：监听客户端关注的智能体的记忆目录，有文件变化就发
//! `agent_memory.changed`。
//!
//! 记忆文件在运行时那台机器上，客户端监听不到，只能由运行时来看。一个智能体读两层：它自己的
//! （`~/.smelt/agents/<id>/memory`）和用户全局的（`~/.smelt/memory`，所有智能体共用，只监听
//! 一份）。目录还没有就先建好，不然监听不了，智能体第一次写记忆时客户端就看不到。一批事件
//! 合并成一次通知。监听一旦建立就保持到运行时退出，和 `project.watch` 一样。

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Condvar, Mutex, OnceLock};
use std::time::Duration;

use notify::{RecursiveMode, Watcher as _};

use crate::event_hub::EventHubHandle;

/// 一批文件事件攒这么久再通知一次：智能体写一条记忆会连着改好几次文件。
const DEBOUNCE: Duration = Duration::from_millis(200);

struct RegisteredLayer {
    raw: PathBuf,
    canonical: PathBuf,
}

impl RegisteredLayer {
    fn matches(&self, path: &Path) -> bool {
        path.starts_with(&self.raw) || path.starts_with(&self.canonical)
    }
}

/// 键是层：`Some(智能体 id)` 是那个智能体自己的，`None` 是用户全局的。
struct MemoryWatchState {
    layers: HashMap<Option<String>, RegisteredLayer>,
    dirty: HashSet<Option<String>>,
    hub: Option<EventHubHandle>,
}

struct MemoryWatchManager {
    watcher: Mutex<notify::RecommendedWatcher>,
    state: Arc<(Mutex<MemoryWatchState>, Condvar)>,
}

fn manager() -> &'static Mutex<Option<MemoryWatchManager>> {
    static MANAGER: OnceLock<Mutex<Option<MemoryWatchManager>>> = OnceLock::new();
    MANAGER.get_or_init(|| Mutex::new(None))
}

fn init_manager(event_hub: &EventHubHandle) -> Result<MemoryWatchManager, String> {
    let state = Arc::new((
        Mutex::new(MemoryWatchState {
            layers: HashMap::new(),
            dirty: HashSet::new(),
            hub: Some(Arc::clone(event_hub)),
        }),
        Condvar::new(),
    ));
    let cb_state = Arc::clone(&state);
    let watcher = notify::recommended_watcher(move |res: notify::Result<notify::Event>| {
        let Ok(event) = res else {
            return;
        };
        let (lock, cv) = &*cb_state;
        let mut guard = lock.lock().unwrap_or_else(|e| e.into_inner());
        let MemoryWatchState { layers, dirty, .. } = &mut *guard;
        let mut matched_any = false;
        for (layer, reg) in layers.iter() {
            if event.paths.iter().any(|path| reg.matches(path)) {
                dirty.insert(layer.clone());
                matched_any = true;
            }
        }
        if matched_any {
            cv.notify_one();
        }
    })
    .map_err(|error| format!("Couldn't start memory watcher: {error}"))?;
    let notifier_state = Arc::clone(&state);
    std::thread::Builder::new()
        .name("smeltd-memory-watch".into())
        .spawn(move || {
            let (lock, cv) = &*notifier_state;
            loop {
                let mut guard = lock.lock().unwrap_or_else(|e| e.into_inner());
                guard = cv
                    .wait_while(guard, |st| st.dirty.is_empty())
                    .unwrap_or_else(|e| e.into_inner());
                drop(guard);
                std::thread::sleep(DEBOUNCE);
                let (layers_to_notify, hub) = {
                    let mut guard = lock.lock().unwrap_or_else(|e| e.into_inner());
                    let dirty: Vec<Option<String>> = guard.dirty.drain().collect();
                    (dirty, guard.hub.clone())
                };
                if let Some(hub) = hub {
                    for layer in layers_to_notify {
                        if let Err(error) = hub.publish_agent_memory_changed(layer.as_deref()) {
                            eprintln!("[memory-watch] 通知记忆变化失败：{error}");
                        }
                    }
                }
            }
        })
        .map_err(|error| format!("Couldn't start the watcher thread: {error}"))?;
    Ok(MemoryWatchManager {
        watcher: Mutex::new(watcher),
        state,
    })
}

/// 关注 `agent_id` 读的两层记忆。幂等；所有层复用单一 `RecommendedWatcher` 与去抖线程。
pub(crate) fn watch(agent_id: &str, event_hub: &EventHubHandle) -> Result<(), String> {
    let agent_dir = smelt_core::agent_definition_store::agent_memory_dir(agent_id)
        .ok_or_else(|| format!("The agent id can't be used as a directory name: {agent_id}"))?;
    let user_dir = smelt_core::agent_definition_store::user_memory_dir()
        .ok_or_else(|| "Couldn't find the user's global memory directory".to_string())?;
    watch_layer(Some(agent_id.to_string()), &agent_dir, event_hub)?;
    watch_layer(None, &user_dir, event_hub)
}

fn watch_layer(
    layer: Option<String>,
    dir: &Path,
    event_hub: &EventHubHandle,
) -> Result<(), String> {
    let mut slot = manager().lock().map_err(|error| error.to_string())?;
    if slot.is_none() {
        *slot = Some(init_manager(event_hub)?);
    }
    let mgr = slot.as_ref().expect("initialized above");
    {
        let (lock, _) = &*mgr.state;
        let mut guard = lock.lock().unwrap_or_else(|e| e.into_inner());
        guard.hub = Some(Arc::clone(event_hub));
        if guard.layers.contains_key(&layer) {
            return Ok(());
        }
    }
    std::fs::create_dir_all(dir).map_err(|error| {
        format!(
            "Couldn't create the memory directory {}: {error}",
            dir.display()
        )
    })?;
    let canonical = std::fs::canonicalize(dir).unwrap_or_else(|_| dir.to_path_buf());
    mgr.watcher
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .watch(dir, RecursiveMode::Recursive)
        .map_err(|error| format!("Couldn't watch {}: {error}", dir.display()))?;
    let (lock, _) = &*mgr.state;
    lock.lock()
        .unwrap_or_else(|e| e.into_inner())
        .layers
        .insert(
            layer,
            RegisteredLayer {
                raw: dir.to_path_buf(),
                canonical,
            },
        );
    Ok(())
}

/// 注销指定智能体的记忆目录监听。
#[allow(dead_code)]
pub(crate) fn unwatch(agent_id: &str) {
    let Ok(slot) = manager().lock() else {
        return;
    };
    let Some(mgr) = slot.as_ref() else {
        return;
    };
    let key = Some(agent_id.to_string());
    let removed = {
        let (lock, _) = &*mgr.state;
        let mut guard = lock.lock().unwrap_or_else(|e| e.into_inner());
        guard.dirty.remove(&key);
        guard.layers.remove(&key)
    };
    if let Some(reg) = removed {
        let _ = mgr
            .watcher
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .unwatch(&reg.raw);
    }
}
