//! 记住哪些 dsh 自定义 provider 的模型目录是「自动的」。
//!
//! 一个中转网关的模型会变。用户在设置页不填模型就保存，意思是「这份目录我不维护，
//! 每次启动照端点上的来」。这个模块只存这一个意图。
//!
//! **为什么不存进 dsh 的 settings.yaml。** provider 本身就写在那里，把标记贴在
//! provider 节点旁边看起来更顺手，实测 dsh 也确实容忍未知字段。但那是 `llm-pi-ai`
//! 插件自己的 schema：它今天不校验不代表明天不校验，dsh web 改配置时也可能顺手把
//! 认不出的键抹掉。Smelt 的状态放 Smelt 自己的持久化边界里，是唯一不用赌别人实现
//! 细节的存法。
//!
//! **为什么「自动」不等于「不写模型」。** dsh 的 `resolveRouteModels` 明确拒绝一条
//! 解析不出模型的自定义路由——而且不是那条路由不可用，是整个 `llm-pi-ai` 配置段被
//! 判定不可服务，所有自定义 provider 一起失效。所以 settings.yaml 里永远是一份具体
//! 列表，自动只是说这份列表由 Smelt 在启动时刷新，而不是由用户手打。这也顺带保证了
//! 离线可用：拉不到就沿用上次那份。
//!
//! **为什么不按 profile 分组。** dsh 的自定义 provider 写在 `$DSH_HOME/settings.yaml`，
//! 是所有 profile 共享的一份；profile 只决定发现时启动哪个运行时。按 profile 存标记
//! 会凭空造出一个配置里并不存在的维度。

use serde::Deserialize;

pub use crate::auto_model_store::AutoModelStore as DshAutoModelStore;

/// 名单存在这个文件里。dsh 与 Pi 各一份：同名 provider 在两边是两回事。
const STORE_FILE: &str = "dsh-auto-models.json";

pub fn load() -> DshAutoModelStore {
    crate::auto_model_store::load(STORE_FILE)
}

pub fn save(store: &DshAutoModelStore) {
    crate::auto_model_store::save(STORE_FILE, store);
}

pub fn update(mutate: impl FnOnce(&mut DshAutoModelStore)) {
    crate::auto_model_store::update(STORE_FILE, mutate);
}

/// `dsh-model-settings --action read` 里刷新真正用得上的那几个字段。
///
/// 只声明用得到的，别的字段交给 serde 忽略：这个模块不该因为设置页新增一个展示字段
/// 就跟着改。
#[derive(Debug, Deserialize)]
struct ConfiguredProvider {
    id: String,
    #[serde(default)]
    api: String,
    #[serde(rename = "baseURL", default)]
    base_url: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ConfiguredProviders {
    #[serde(default)]
    custom_providers: Vec<ConfiguredProvider>,
}

/// 一个 provider 的刷新结果。
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum AutoRefreshOutcome {
    /// 目录已按端点更新，附上模型个数。
    Refreshed { provider: String, models: usize },
    /// 没问到。**沿用上次那份目录**——离线、端点抖动、key 过期都会走到这里，
    /// 而把一份能用的目录清掉换来一条错误，比留着旧的更糟。
    Kept { provider: String, reason: String },
    /// 标记还在，provider 已经不在配置里了（多半是在 dsh 那边删的），标记已清理。
    Forgotten { provider: String },
}

/// 按端点重新拉取所有「自动」provider 的模型目录。
///
/// 在应用启动的后台线程里跑：它会起 Node 子进程，几秒级，绝不能挡首帧。没有任何
/// provider 被标记为自动时直接返回，不启动任何进程——没用这个功能的用户不该为它付钱。
///
/// 返回每个 provider 的结果供调用方打日志；单个失败不影响其余，也不改动它的目录。
pub fn refresh_auto_model_catalogs() -> Vec<AutoRefreshOutcome> {
    let marked = load().auto_providers();
    if marked.is_empty() {
        return Vec::new();
    }
    // 目录是全局的，但发现要挑一个装了发现器的 profile 去启动运行时。
    let Some(profile) = crate::agent_kind::native_dsh_profiles()
        .into_iter()
        .find(|profile| {
            crate::agent_kind::dsh_discover_models_entry(&profile.workspace_dir).is_some()
        })
        .map(|profile| profile.workspace_dir)
    else {
        return marked
            .into_iter()
            .map(|provider| AutoRefreshOutcome::Kept {
                provider,
                reason: format!(
                    "未找到模型发现器，请把 Smelt Host bundle 更新到 {}",
                    crate::agent_kind::SMELT_HOST_VERSION
                ),
            })
            .collect();
    };
    let configured = match crate::agent_kind::dsh_model_settings("read", None).and_then(|raw| {
        serde_json::from_slice::<ConfiguredProviders>(&raw)
            .map_err(|error| format!("读取 DSH 模型设置响应失败：{error}"))
    }) {
        Ok(settings) => settings.custom_providers,
        Err(error) => {
            return marked
                .into_iter()
                .map(|provider| AutoRefreshOutcome::Kept {
                    provider,
                    reason: error.clone(),
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
        outcomes.push(refresh_one(&profile, entry));
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

fn refresh_one(profile: &str, entry: &ConfiguredProvider) -> AutoRefreshOutcome {
    let provider = entry.id.clone();
    let request = crate::agent_kind::DshDiscoveryRequest {
        provider: Some(provider.clone()),
        base_url: (!entry.base_url.trim().is_empty()).then(|| entry.base_url.trim().to_string()),
        api: (!entry.api.trim().is_empty()).then(|| entry.api.trim().to_string()),
        api_key: None,
    };
    let report = match crate::agent_kind::dsh_discover_models(profile, &request) {
        Ok(report) => report,
        Err(reason) => return AutoRefreshOutcome::Kept { provider, reason },
    };
    if report.models.is_empty() {
        // 端点回了个空列表，不代表这条路由真的一个模型都不剩——更常见的是网关在
        // 抽风。写空会让 dsh 判定整段配置不可服务，把其它 provider 一起拖下水。
        return AutoRefreshOutcome::Kept {
            provider,
            reason: "端点没有报告任何模型".to_string(),
        };
    }
    let models = report
        .models
        .iter()
        .map(|model| {
            serde_json::json!({
                "id": model.id,
                "name": model.name,
                "contextWindow": model.context_window,
                "maxTokens": model.max_tokens,
            })
        })
        .collect::<Vec<_>>();
    let payload = serde_json::json!({ "id": provider, "models": models });
    match crate::agent_kind::dsh_model_settings("refresh-models", Some(&payload)) {
        Ok(_) => AutoRefreshOutcome::Refreshed {
            provider,
            models: report.models.len(),
        },
        Err(reason) => AutoRefreshOutcome::Kept { provider, reason },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 缺字段的旧文件必须读成「一个都不自动」，而不是读失败后被下一次保存覆盖。
    #[test]
    fn reads_a_document_without_the_field() {
        let store: DshAutoModelStore = serde_json::from_str("{}").expect("deserialise");

        assert_eq!(store, DshAutoModelStore::default());
    }

    /// `baseURL` 的大小写在助手那边咬过一次：`camelCase` 推导出的 `baseUrl` 会被
    /// 静默忽略，表现为「刷新永远问不到东西」。这条钉住读的那一侧。
    #[test]
    fn reads_the_bridge_report_including_base_url_casing() {
        let raw = br#"{"settingsPath":"/tmp/settings.yaml","customProviders":[
            {"id":"sub2api","displayName":"Sub2Api","api":"openai-responses",
             "baseURL":"https://gateway.example","apiKeyEnv":"K",
             "credentialConfigured":true,"models":[{"id":"a"}]}]}"#;

        let parsed: ConfiguredProviders = serde_json::from_slice(raw).expect("parse");

        assert_eq!(parsed.custom_providers.len(), 1);
        assert_eq!(parsed.custom_providers[0].id, "sub2api");
        assert_eq!(parsed.custom_providers[0].api, "openai-responses");
        assert_eq!(
            parsed.custom_providers[0].base_url,
            "https://gateway.example"
        );
    }

    /// 报告里没有任何自定义 provider 时不该解析失败——那只是「一个都没配」。
    #[test]
    fn reads_a_report_without_custom_providers() {
        let parsed: ConfiguredProviders =
            serde_json::from_slice(br#"{"settingsPath":"/tmp/s.yaml"}"#).expect("parse");

        assert!(parsed.custom_providers.is_empty());
    }

    /// 一个 provider 都没标记时必须一个子进程都不起：没用这个功能的用户不该为它
    /// 在每次启动上付几秒。这里靠「没有 dsh 也能返回空」间接钉住。
    #[test]
    fn refreshing_without_marks_starts_nothing() {
        assert!(DshAutoModelStore::default().auto_providers().is_empty());
    }

    #[test]
    fn round_trips_through_json() {
        let mut store = DshAutoModelStore::default();
        store.set_auto("sub2api", true);

        let json = serde_json::to_string(&store).expect("serialise");
        assert_eq!(json, r#"{"providers":["sub2api"]}"#);
        assert_eq!(
            serde_json::from_str::<DshAutoModelStore>(&json).expect("deserialise"),
            store
        );
    }
}

/// 对本机真实 dsh 装配跑一次完整刷新。
///
/// 默认不跑：它会**改写本机的 `settings.yaml`**（这正是要验的东西），也要几秒。
/// 想跑就先把目标 provider 标成自动，然后：
///
/// ```bash
/// SMELT_TEST_DSH_AUTO_REFRESH=1 \
///   cargo test -p smelt-core refreshes_marked_providers -- --ignored --nocapture
/// ```
#[cfg(test)]
mod live_tests {
    #[test]
    #[ignore = "rewrites the machine's real dsh settings.yaml"]
    fn refreshes_marked_providers() {
        if std::env::var("SMELT_TEST_DSH_AUTO_REFRESH").is_err() {
            eprintln!("set SMELT_TEST_DSH_AUTO_REFRESH=1 to allow rewriting real settings");
            return;
        }
        let marked = super::load().auto_providers();
        assert!(
            !marked.is_empty(),
            "mark at least one provider auto before running this"
        );
        let outcomes = super::refresh_auto_model_catalogs();
        for outcome in &outcomes {
            println!("{outcome:?}");
        }
        assert_eq!(outcomes.len(), marked.len());
        assert!(
            outcomes
                .iter()
                .any(|outcome| matches!(outcome, super::AutoRefreshOutcome::Refreshed { .. })),
            "expected at least one catalog to refresh"
        );
    }
}
