//! 用户给 agent 历史对话起的名字，在 GUI 内的只读缓存（全局单例）。
//!
//! 权威副本在 `smelt_core::session_metadata`（历史页、移动端读的是同一份）。
//! 侧栏每帧都要问「这条终端当前那段对话叫什么」，不能每次都去访问存储，因此
//! 这里做一层进程内缓存：后台节流刷新 + 重命名时写穿，保证改完立刻可见。

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use gpui::{App, BorrowAppContext, Global};

/// (agent id, profile id, provider 对话 id) → 用户命名。
type TitleKey = (String, Option<String>, String);

#[derive(Clone, Default)]
pub struct HistoryTitles {
    map: Arc<Mutex<HashMap<TitleKey, String>>>,
    revision: u64,
}

impl HistoryTitles {
    fn signal(cx: &mut App) {
        cx.update_global::<Self, _>(|global, _| {
            global.revision = global.revision.wrapping_add(1);
        });
    }

    /// 后台重新读完整份覆盖层后替换缓存。内容没变就不打扰渲染。
    pub fn replace_all(titles: HashMap<TitleKey, String>, cx: &mut App) {
        let Some(map) = cx.try_global::<Self>().map(|global| global.map.clone()) else {
            return;
        };
        {
            let mut current = map.lock().unwrap();
            if *current == titles {
                return;
            }
            *current = titles;
        }
        Self::signal(cx);
    }

    /// 重命名后的写穿：不等下一次后台刷新，侧栏立即用新名字。
    pub fn set(
        agent_id: &str,
        profile_id: Option<&str>,
        conversation_id: &str,
        title: Option<&str>,
        cx: &mut App,
    ) {
        let Some(map) = cx.try_global::<Self>().map(|global| global.map.clone()) else {
            return;
        };
        let key = (
            agent_id.to_string(),
            profile_id.map(str::to_string),
            conversation_id.to_string(),
        );
        {
            let mut map = map.lock().unwrap();
            match title.map(str::trim).filter(|title| !title.is_empty()) {
                Some(title) => map.insert(key, title.to_string()),
                None => map.remove(&key),
            };
        }
        Self::signal(cx);
    }

    pub fn get(
        agent_id: &str,
        profile_id: Option<&str>,
        conversation_id: &str,
        cx: &App,
    ) -> Option<String> {
        let key = (
            agent_id.to_string(),
            profile_id.map(str::to_string),
            conversation_id.to_string(),
        );
        cx.try_global::<Self>()?.map.lock().ok()?.get(&key).cloned()
    }
}

impl Global for HistoryTitles {}
