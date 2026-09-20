//! 哪些 Pi 自定义 provider 的模型目录由 Smelt 照端点维护。
//!
//! 标记的语义、存法和 dsh 那边完全一样（见 [`crate::auto_model_store`]），差别
//! 只有两处：名单存在自己的文件里（同名 provider 在两边是两回事），以及问端点
//! 走 [`crate::pi_model_discovery`] 的直连 HTTP，不需要任何 Node 运行时。
//!
//! **为什么「自动」不等于「不写模型」。** Pi 从 `models.json` 里解析 provider
//! 的模型列表，写空等于这个 provider 一个模型都选不了。所以文件里永远是一份具
//! 体列表，自动只是说这份列表由 Smelt 在启动时刷新，而不是由用户手打。这也顺带
//! 保证离线可用：拉不到就沿用上次那份。

pub use crate::auto_model_store::AutoModelStore as PiAutoModelStore;

const STORE_FILE: &str = "pi-auto-models.json";

pub fn load() -> PiAutoModelStore {
    crate::auto_model_store::load(STORE_FILE)
}

pub fn save(store: &PiAutoModelStore) {
    crate::auto_model_store::save(STORE_FILE, store);
}

pub fn update(mutate: impl FnOnce(&mut PiAutoModelStore)) {
    crate::auto_model_store::update(STORE_FILE, mutate);
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
    let configured = match crate::pi_model_settings::load_pi_model_settings_at(agent_dir) {
        Ok(settings) => settings.custom_providers,
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
            reason: "端点没有报告任何模型".to_string(),
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
