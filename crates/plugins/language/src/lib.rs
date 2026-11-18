//! 语言：设置里选界面语言（跟随系统，或 App 带的某一种），记在本插件的库里，启动时换上。
//!
//! 换语言本身（当前语言、通知各插件重建存着的字）是插件接口的机制（[`api::set_locale`]），
//! 这里只管用户选了什么。内置插件，只经 [`smelt_plugin_api`] 和宿主打交道。

mod page;
mod store;

use std::cell::Cell;
use std::rc::Rc;

use gpui::App;
use smelt_plugin_api as api;

use crate::store::LanguageStore;

rust_i18n::i18n!("locales", fallback = "en");

/// 本插件的来源 id。
pub(crate) const OWNER: &str = "smelt.language";

/// 用户选的语言：[`api::LOCALES`] 里的代码，`None` 是跟随系统。启动时从库里读一次，
/// 设置页改它，render 只读这一份。
pub(crate) struct Choice {
    store: LanguageStore,
    current: Cell<Option<&'static str>>,
}

impl Choice {
    pub(crate) fn get(&self) -> Option<&'static str> {
        self.current.get()
    }

    /// 改选：记进库，换上对应的语言。
    pub(crate) fn set(&self, choice: Option<&'static str>, cx: &mut App) {
        if self.current.get() == choice {
            return;
        }
        self.current.set(choice);
        self.store.save(choice);
        api::set_locale(effective(choice), cx);
    }
}

/// 选的语言实际是哪种：跟随系统时取系统的首选语言。
fn effective(choice: Option<&'static str>) -> &'static str {
    choice.unwrap_or_else(api::system_locale)
}

/// 宿主启动时调一次，越早越好：在这之前登记的字也跟着语言变，但首帧之前换上不闪。
pub fn register(cx: &mut App) {
    let store = LanguageStore::open();
    let current = store.choice();
    api::set_locale(effective(current), cx);
    let choice = Rc::new(Choice {
        store,
        current: Cell::new(current),
    });
    page::register(choice, cx);
}
