//! 「这份模型目录由 Smelt 维护」这个标记本身。
//!
//! dsh 和 Pi 各有一份自定义 provider 配置，但「用户没手填模型 = 目录交给我们照
//! 端点刷新」这个意图是同一个。标记的存法、改名要跟着走、清一个从没设过的不算
//! 错——这些规则没有 harness 之分，所以只写一遍；两边的差别只在存哪个文件、
//! 以及怎么去问端点。

use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;

/// 一份「哪些 provider 的目录是自动的」名单。
///
/// 用 `BTreeSet` 是为了写盘顺序稳定，避免每次启动都产生一次无意义的 diff。
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct AutoModelStore {
    #[serde(default)]
    providers: BTreeSet<String>,
}

impl AutoModelStore {
    pub fn is_auto(&self, provider: &str) -> bool {
        self.providers.contains(provider)
    }

    /// 所有自动维护目录的 provider，按 id 排序。
    pub fn auto_providers(&self) -> Vec<String> {
        self.providers.iter().cloned().collect()
    }

    pub fn set_auto(&mut self, provider: &str, auto: bool) {
        if auto {
            self.providers.insert(provider.to_string());
        } else {
            self.providers.remove(provider);
        }
    }

    /// 改名时把标记带过去。设置页允许改 provider id，标记不跟着走的话，用户会得到
    /// 一个「以为自动、其实再也不刷新」的 provider——比不支持改名更糟。
    pub fn rename(&mut self, from: &str, to: &str) {
        if from == to || !self.is_auto(from) {
            return;
        }
        self.set_auto(from, false);
        self.set_auto(to, true);
    }
}

pub fn load(harness: smelt_store::AutoModelHarness) -> AutoModelStore {
    let Ok(store) = crate::sqlite_state::default_sqlite_store() else {
        return AutoModelStore::default();
    };
    AutoModelStore {
        providers: store
            .auto_model_providers(harness)
            .unwrap_or_default()
            .into_iter()
            .collect(),
    }
}

pub fn save(harness: smelt_store::AutoModelHarness, store: &AutoModelStore) {
    if let Ok(sqlite) = crate::sqlite_state::default_sqlite_store() {
        let _ = sqlite.put_auto_model_providers(harness, &store.auto_providers());
    }
}

/// 读改写一次。调用点都是「翻一个标记」这种小改动，各自 load/save 容易漏掉其中一半。
/// 没有实际变化就不落盘，免得每次启动都白写一遍。
pub fn update(harness: smelt_store::AutoModelHarness, mutate: impl FnOnce(&mut AutoModelStore)) {
    let mut store = load(harness);
    let before = store.clone();
    mutate(&mut store);
    if store != before {
        save(harness, &store);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn remembers_marked_providers() {
        let mut store = AutoModelStore::default();
        store.set_auto("sub2api", true);

        assert!(store.is_auto("sub2api"));
        assert!(!store.is_auto("other"));
        assert_eq!(store.auto_providers(), vec!["sub2api".to_string()]);
    }

    #[test]
    fn clearing_something_never_set_is_not_an_error() {
        let mut store = AutoModelStore::default();
        store.set_auto("ghost", false);

        assert_eq!(store, AutoModelStore::default());
    }

    #[test]
    fn rename_carries_the_mark() {
        let mut store = AutoModelStore::default();
        store.set_auto("old", true);
        store.rename("old", "new");

        assert!(!store.is_auto("old"));
        assert!(store.is_auto("new"));
    }

    #[test]
    fn renaming_a_manual_provider_does_not_make_it_auto() {
        let mut store = AutoModelStore::default();
        store.rename("old", "new");

        assert!(!store.is_auto("new"));
    }
}
