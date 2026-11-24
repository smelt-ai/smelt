//! 界面语言设置与持久化（沿用 `smelt.language` 以保留 `~/.smelt/plugin-data/smelt.language/store.sqlite3`）：
//! 在主窗口创建之前同步恢复用户选择的界面语言，避免启动首帧按系统语言闪烁；设置「通用 → 语言」
//! 页提供切换，改选即时切换全局语言并异步落盘。

mod page;
mod store;

use std::sync::Arc;

use gpui::{App, AppContext as _, Global};
use smelt_plugin_api::{self as api};

use store::LanguageStore;

const OWNER: &str = "smelt.language";

#[derive(Clone)]
pub(crate) struct LanguageChoice {
    entity: gpui::Entity<Option<&'static str>>,
}

impl Global for LanguageChoice {}

impl LanguageChoice {
    pub(crate) fn get(&self, cx: &App) -> Option<&'static str> {
        *self.entity.read(cx)
    }

    pub(crate) fn set(&self, next: Option<&'static str>, cx: &mut App) {
        if self.get(cx) == next {
            return;
        }
        self.entity.update(cx, |choice, cx| {
            *choice = next;
            cx.notify();
        });
    }
}

#[derive(Clone)]
struct StoredChoice {
    choice: Option<&'static str>,
    store: Arc<LanguageStore>,
}

impl Global for StoredChoice {}

/// 在主窗口打开之前同步读取用户保存的语言并应用（未选则跟随系统）。
pub(crate) fn restore_saved_locale(cx: &mut App) {
    let store = Arc::new(LanguageStore::open());
    let choice = store.choice();
    api::set_locale(choice.unwrap_or_else(|| api::system_locale()), cx);
    cx.set_global(StoredChoice { choice, store });
}

/// 登记「通用 → 语言」设置页并绑定语言持久化观察者。
pub(crate) fn register(cx: &mut App) {
    let StoredChoice { choice, store } =
        cx.try_global::<StoredChoice>().cloned().unwrap_or_else(|| {
            let store = Arc::new(LanguageStore::open());
            StoredChoice {
                choice: store.choice(),
                store,
            }
        });
    let entity = cx.new(|_| choice);
    let handle = LanguageChoice {
        entity: entity.clone(),
    };
    cx.set_global(handle.clone());
    cx.observe(&entity, move |entity, cx| {
        let next = *entity.read(cx);
        let code = next.unwrap_or_else(|| api::system_locale());
        api::set_locale(code, cx);
        let writer = store.clone();
        cx.background_executor()
            .spawn(async move { writer.save(next) })
            .detach();
    })
    .detach();
    if let Err(error) = api::register_views(OWNER, vec![page::spec(handle)], cx) {
        eprintln!("[language] 语言页登记失败：{error}");
    }
}
