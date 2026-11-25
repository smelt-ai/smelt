//! 哪些 Pi 自定义 provider 的模型目录由 Smelt 照端点维护。
//!
//! 「用户没手填模型 = 目录交给我们照端点刷新」：名单存在运行时的库里，问端点走
//! [`crate::pi_model_discovery`] 的直连 HTTP，不需要任何 Node 运行时。
//!
//! **为什么「自动」不等于「不写模型」。** Pi 从 `models.json` 里解析 provider
//! 的模型列表，写空等于这个 provider 一个模型都选不了。所以文件里永远是一份具
//! 体列表，自动只是说这份列表由 Smelt 在启动时刷新，而不是由用户手打。这也顺带
//! 保证离线可用：拉不到就沿用上次那份。

use std::collections::BTreeSet;

/// 一份「哪些 provider 的目录是自动的」名单。
///
/// 用 `BTreeSet` 是为了写盘顺序稳定，避免每次启动都产生一次无意义的 diff。
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct PiAutoModelStore {
    providers: BTreeSet<String>,
}

impl PiAutoModelStore {
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

pub fn load() -> PiAutoModelStore {
    let Ok(store) = crate::sqlite_state::default_sqlite_store() else {
        return PiAutoModelStore::default();
    };
    PiAutoModelStore {
        providers: store
            .auto_model_providers()
            .unwrap_or_default()
            .into_iter()
            .collect(),
    }
}

pub fn save(store: &PiAutoModelStore) {
    if let Ok(sqlite) = crate::sqlite_state::default_sqlite_store() {
        let _ = sqlite.put_auto_model_providers(&store.auto_providers());
    }
}

/// 读改写一次。调用点都是「翻一个标记」这种小改动，各自 load/save 容易漏掉其中一半。
/// 没有实际变化就不落盘，免得每次启动都白写一遍。
pub fn update(mutate: impl FnOnce(&mut PiAutoModelStore)) {
    let mut store = load();
    let before = store.clone();
    mutate(&mut store);
    if store != before {
        save(&store);
    }
}

/// 一个 provider 的刷新结果。
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum AutoRefreshOutcome {
    /// 目录已按端点更新，附上模型个数。
    Refreshed { provider: String, models: usize },
    /// 没问到。**沿用上次那份目录**——离线、端点抖动、key 过期都会走到这里，
    /// 而把一份能用的目录清掉换来一条错误，比留着旧的更糟。
    Kept { provider: String, reason: String },
    /// 标记还在，provider 已经不在 `models.json` 里了（多半是用户自己删的），
    /// 标记已清理。
    Forgotten { provider: String },
}

/// 按端点重新拉取所有「自动」provider 的模型目录。
///
/// 在应用启动的后台线程里跑：每个 provider 一次网络往返。没有任何 provider 被
/// 标记为自动时立刻返回，不发任何请求。
///
/// 返回每个 provider 的结果供调用方打日志；单个失败不影响其余，也不改动它的目录。
pub fn refresh_auto_model_catalogs() -> Vec<AutoRefreshOutcome> {
    refresh_auto_model_catalogs_at(&crate::pi_model_settings::pi_agent_dir())
}

pub fn refresh_auto_model_catalogs_at(agent_dir: &std::path::Path) -> Vec<AutoRefreshOutcome> {
    let marked = load().auto_providers();
    if marked.is_empty() {
        return Vec::new();
    }
    let configured = match crate::pi_model_settings::load_pi_custom_providers_at(agent_dir) {
        Ok(providers) => providers,
        Err(reason) => {
            return marked
                .into_iter()
                .map(|provider| AutoRefreshOutcome::Kept {
                    provider,
                    reason: reason.clone(),
                })
                .collect();
        }
    };

    let mut outcomes = Vec::with_capacity(marked.len());
    let mut forgotten = Vec::new();
    for provider in marked {
        let Some(entry) = configured.iter().find(|entry| entry.id == provider) else {
            forgotten.push(provider.clone());
            outcomes.push(AutoRefreshOutcome::Forgotten { provider });
            continue;
        };
        outcomes.push(refresh_one(agent_dir, entry));
    }
    if !forgotten.is_empty() {
        update(|store| {
            for provider in &forgotten {
                store.set_auto(provider, false);
            }
        });
    }
    outcomes
}

fn refresh_one(
    agent_dir: &std::path::Path,
    entry: &crate::pi_model_settings::PiCustomProvider,
) -> AutoRefreshOutcome {
    let provider = entry.id.clone();
    let api_key = crate::pi_model_settings::read_pi_provider_api_key_at(agent_dir, &provider)
        .unwrap_or_default();
    let request = crate::pi_model_discovery::ModelDiscoveryRequest {
        base_url: entry.base_url.clone(),
        api: entry.api.clone(),
        api_key,
    };
    let models = match crate::pi_model_discovery::discover_models(&request) {
        Ok(models) => models,
        Err(reason) => return AutoRefreshOutcome::Kept { provider, reason },
    };
    if models.is_empty() {
        // 端点回了个空列表，不代表这条路由真的一个模型都不剩——更常见的是网关在
        // 抽风。写空会让这个 provider 变得一个模型都选不了。
        return AutoRefreshOutcome::Kept {
            provider,
            reason: "the endpoint reported no models".to_string(),
        };
    }
    let count = models.len();
    match crate::pi_model_settings::replace_pi_provider_models_at(agent_dir, &provider, &models) {
        Ok(()) => AutoRefreshOutcome::Refreshed {
            provider,
            models: count,
        },
        Err(reason) => AutoRefreshOutcome::Kept { provider, reason },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn remembers_marked_providers() {
        let mut store = PiAutoModelStore::default();
        store.set_auto("sub2api", true);

        assert!(store.is_auto("sub2api"));
        assert!(!store.is_auto("other"));
        assert_eq!(store.auto_providers(), vec!["sub2api".to_string()]);
    }

    #[test]
    fn clearing_something_never_set_is_not_an_error() {
        let mut store = PiAutoModelStore::default();
        store.set_auto("ghost", false);

        assert_eq!(store, PiAutoModelStore::default());
    }

    #[test]
    fn rename_carries_the_mark() {
        let mut store = PiAutoModelStore::default();
        store.set_auto("old", true);
        store.rename("old", "new");

        assert!(!store.is_auto("old"));
        assert!(store.is_auto("new"));
    }

    #[test]
    fn renaming_a_manual_provider_does_not_make_it_auto() {
        let mut store = PiAutoModelStore::default();
        store.rename("old", "new");

        assert!(!store.is_auto("new"));
    }

    /// 没有任何标记时不该去碰配置文件，更不该发请求。
    #[test]
    fn nothing_marked_means_nothing_to_do() {
        assert!(PiAutoModelStore::default().auto_providers().is_empty());
        let dir = std::env::temp_dir().join(format!(
            "smelt-pi-auto-none-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|duration| duration.as_nanos())
                .unwrap_or(0)
        ));
        assert!(refresh_auto_model_catalogs_at(&dir).is_empty());
    }
}
