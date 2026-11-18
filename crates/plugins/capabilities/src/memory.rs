//! 设置页：记忆的向量检索服务（OpenAI 兼容 `/embeddings`）。
//!
//! 配置属于运行时：读写走 `settings.get` / `settings.update`，连接测试走
//! `memory.test_embedding`——服务地址常是 `localhost`，说的是运行时那台机器。表单配的是
//! 能力库眼下选中的那台机器，调用都发给它。这里只管表单。表单状态挂在页面自己的 keyed
//! state 上而不是 `CapabilitiesPage`，按机器分开：它只在这一页可见时活着，每次打开页面、换一台
//! 机器都重新读一次。
//!
//! 已存的 API Key 客户端看不到，只知道设没设：输入框留空表示「保持不变」，要删得点
//! 「清除密钥」。

use std::collections::BTreeMap;

use api::client::RuntimeClient;
use api::client::protocol::control::{ControlCall, EmptyParams};
use api::model::appearance_settings::{SettingsPatch, SettingsResult};
use api::model::control_api::{MemoryTestEmbedding, MemoryTestParams, SettingsGet, SettingsUpdate};
use api::model::memory_settings::{
    EMBEDDING_API_KEY, EMBEDDING_BASE_URL, EMBEDDING_MODEL, RERANK_MODEL_ID, RERANK_MODEL_PROVIDER,
    RerankModel,
};
use api::serde_json::Value;
use std::borrow::Cow;

use gpui::{AppContext, Context, Entity, Subscription, Window};
use rust_i18n::t;
use smelt_plugin_api::ui::field::InputState;
use smelt_plugin_api::{self as api, RuntimeKey};

/// 连接测试要等对方的嵌入服务，运行时那边自己限 10 秒，这里多留一点余量。
const TEST_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(20);

/// 运行时里当前的配置。密钥本身客户端看不到，只知道设没设。
#[derive(Clone, Debug, Default, PartialEq, Eq)]
struct SavedMemory {
    base_url: String,
    model: String,
    api_key_set: bool,
}

impl SavedMemory {
    /// 地址和模型都填了才算配置；密钥可空（本地服务不需要）。
    fn is_configured(&self) -> bool {
        !self.base_url.is_empty() && !self.model.is_empty()
    }
}

fn setting_text(settings: &BTreeMap<String, Value>, key: &str) -> String {
    settings
        .get(key)
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string()
}

fn saved_from(result: &SettingsResult) -> (SavedMemory, Option<RerankModel>) {
    let settings = &result.settings;
    let saved = SavedMemory {
        base_url: setting_text(settings, EMBEDDING_BASE_URL),
        model: setting_text(settings, EMBEDDING_MODEL),
        api_key_set: !setting_text(settings, EMBEDDING_API_KEY).is_empty(),
    };
    let provider = setting_text(settings, RERANK_MODEL_PROVIDER);
    let id = setting_text(settings, RERANK_MODEL_ID);
    let rerank = (!provider.is_empty() && !id.is_empty()).then_some(RerankModel { provider, id });
    (saved, rerank)
}

fn patch_of(entries: &[(&str, String)]) -> SettingsPatch {
    SettingsPatch {
        settings: entries
            .iter()
            .map(|(key, value)| ((*key).to_string(), Value::from(value.as_str())))
            .collect(),
    }
}

pub(crate) struct MemorySettingsForm {
    /// 发给那台机器的客户端；机器已经移除时是给人看的原因。
    client: Result<RuntimeClient, String>,
    pub base_url: Entity<InputState>,
    pub model: Entity<InputState>,
    pub api_key: Entity<InputState>,
    saved: SavedMemory,
    /// 还在向运行时读当前配置。
    loading: bool,
    pub error: Option<String>,
    pub notice: Option<Cow<'static, str>>,
    /// 用户指定的重排模型；`None` 表示自动挑。
    pub rerank_model: Option<RerankModel>,
    /// 正在跑连接测试。
    pub testing: bool,
    /// 上一次连接测试的结果：成功时是一句"可用：…"，失败时是原因。
    pub test_result: Option<Result<String, String>>,
    /// 换了语言要换输入框的占位文字。
    _locale: Subscription,
}

impl MemorySettingsForm {
    pub fn new(runtime: &RuntimeKey, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let client = api::host(cx)
            .client(runtime, cx)
            .ok_or_else(api::machine_removed);
        let base_url = cx.new(|cx| InputState::new(window, cx));
        let model = cx.new(|cx| InputState::new(window, cx));
        let api_key = cx.new(|cx| InputState::new(window, cx).masked(true));
        let events = api::events(cx);
        let locale = cx.subscribe_in(
            &events,
            window,
            |form, _, event: &api::HostEvent, window, cx| {
                if matches!(event, api::HostEvent::LocaleChanged) {
                    form.label(window, cx);
                }
            },
        );
        let load = send::<SettingsGet>(&client, EmptyParams::default(), api::CALL_TIMEOUT);
        cx.spawn_in(window, async move |this, cx| {
            let loaded = cx.background_spawn(load).await;
            let _ = this.update_in(cx, |form, window, cx| {
                form.loading = false;
                form.apply_result(loaded, None, true, window, cx);
            });
        })
        .detach();
        let form = Self {
            client,
            base_url,
            model,
            api_key,
            saved: SavedMemory::default(),
            loading: true,
            error: None,
            notice: None,
            rerank_model: None,
            testing: false,
            test_result: None,
            _locale: locale,
        };
        form.label(window, cx);
        form
    }

    /// 输入框的占位文字：建好时、换语言时都从这里取。密钥框的随「设没设」变，存完也会再取。
    fn label(&self, window: &mut Window, cx: &mut gpui::App) {
        let key = key_placeholder(&self.saved);
        self.base_url.update(cx, |input, cx| {
            input.set_placeholder(t!("memory.base_url_placeholder"), window, cx)
        });
        self.model.update(cx, |input, cx| {
            input.set_placeholder(t!("memory.model_placeholder"), window, cx)
        });
        self.api_key
            .update(cx, |input, cx| input.set_placeholder(key, window, cx));
    }

    /// 选重排模型：点了立刻提交。两项都空表示改回自动。表单里没保存的输入不动。
    pub fn set_rerank_model(&mut self, model: Option<RerankModel>, cx: &mut Context<Self>) {
        let (provider, id) = model
            .as_ref()
            .map(|model| (model.provider.clone(), model.id.clone()))
            .unwrap_or_default();
        let patch = patch_of(&[(RERANK_MODEL_PROVIDER, provider), (RERANK_MODEL_ID, id)]);
        let update = send::<SettingsUpdate>(&self.client, patch, api::CALL_TIMEOUT);
        cx.spawn(async move |this, cx| {
            let result = cx.background_spawn(update).await;
            let _ = this.update(cx, |form, cx| {
                match result {
                    Ok(result) => {
                        let (saved, rerank) = saved_from(&result);
                        form.saved = saved;
                        form.rerank_model = rerank;
                        form.error = None;
                        form.notice = Some(t!("memory.rerank_updated"));
                    }
                    Err(error) => {
                        form.error = Some(error);
                        form.notice = None;
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }

    /// 用表单里当前的值（不必先保存）让运行时真发一次 `/embeddings` 请求。密钥框
    /// 留空时用已存的密钥。
    pub fn test(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.testing {
            return;
        }
        let params = match test_params(
            &self.base_url.read(cx).value(),
            &self.model.read(cx).value(),
            &self.api_key.read(cx).value(),
        ) {
            Ok(params) => params,
            Err(error) => {
                self.test_result = Some(Err(error));
                cx.notify();
                return;
            }
        };
        self.testing = true;
        self.test_result = None;
        cx.notify();
        let probe = send::<MemoryTestEmbedding>(&self.client, params, TEST_TIMEOUT);
        cx.spawn_in(window, async move |this, cx| {
            let outcome = cx.background_spawn(probe).await;
            let _ = this.update(cx, |form, cx| {
                form.testing = false;
                form.test_result = Some(outcome.map(|probe| {
                    t!(
                        "memory.test_ok",
                        dimensions = probe.dimensions,
                        ms = probe.elapsed_ms
                    )
                    .into_owned()
                }));
                cx.notify();
            });
        })
        .detach();
    }

    pub fn api_key_set(&self) -> bool {
        self.saved.api_key_set
    }

    pub fn status_line(&self) -> String {
        if self.loading {
            return t!("memory.loading").into_owned();
        }
        status_line(&self.saved)
    }

    pub fn save(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let base_url = self.base_url.read(cx).value().to_string();
        let model = self.model.read(cx).value().to_string();
        let api_key = self.api_key.read(cx).value().to_string();
        match save_patch(&base_url, &model, &api_key) {
            Ok(patch) => self.submit(patch, t!("memory.saved"), window, cx),
            Err(error) => {
                self.error = Some(error);
                self.notice = None;
                cx.notify();
            }
        }
    }

    pub fn clear_api_key(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.submit(
            vec![(EMBEDDING_API_KEY, String::new())],
            t!("memory.key_cleared"),
            window,
            cx,
        );
    }

    pub fn clear_all(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.submit(
            vec![
                (EMBEDDING_BASE_URL, String::new()),
                (EMBEDDING_MODEL, String::new()),
                (EMBEDDING_API_KEY, String::new()),
            ],
            t!("memory.cleared"),
            window,
            cx,
        );
    }

    fn submit(
        &mut self,
        patch: Vec<(&'static str, String)>,
        notice: Cow<'static, str>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let patch = patch_of(&patch);
        let update = send::<SettingsUpdate>(&self.client, patch, api::CALL_TIMEOUT);
        cx.spawn_in(window, async move |this, cx| {
            let result = cx.background_spawn(update).await;
            let _ = this.update_in(cx, |form, window, cx| {
                form.apply_result(result, Some(notice), true, window, cx);
            });
        })
        .detach();
    }

    /// 写入成功后以运行时为准重填表单：地址和模型显示规整后的值，密钥框清空。
    /// 失败时保留用户的输入，让他改了再存。
    fn apply_result(
        &mut self,
        result: Result<SettingsResult, String>,
        notice: Option<Cow<'static, str>>,
        refill_inputs: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let result = match result {
            Ok(result) => result,
            Err(error) => {
                self.error = Some(error);
                self.notice = None;
                cx.notify();
                return;
            }
        };
        let (saved, rerank) = saved_from(&result);
        self.saved = saved.clone();
        self.rerank_model = rerank;
        self.error = None;
        self.notice = notice;
        if refill_inputs {
            self.base_url.update(cx, |input, cx| {
                input.set_value(saved.base_url.clone(), window, cx)
            });
            self.model.update(cx, |input, cx| {
                input.set_value(saved.model.clone(), window, cx)
            });
            self.api_key.update(cx, |input, cx| {
                input.set_value("", window, cx);
                input.set_placeholder(key_placeholder(&saved), window, cx);
            });
        }
        cx.notify();
    }
}

/// 发给表单那台机器的一次调用，交给后台执行。机器已经移除时直接给出原因。
fn send<C>(
    client: &Result<RuntimeClient, String>,
    params: C::Params,
    timeout: std::time::Duration,
) -> impl std::future::Future<Output = Result<C::Result, String>> + Send + 'static
where
    C: ControlCall,
    C::Params: Send + 'static,
    C::Result: Send + 'static,
{
    let client = client.clone();
    async move {
        client?
            .call::<C>(&params, timeout)
            .map_err(api::call_error_text)
    }
}

fn key_placeholder(saved: &SavedMemory) -> Cow<'static, str> {
    if saved.api_key_set {
        t!("memory.key_set")
    } else {
        t!("memory.key_empty")
    }
}

fn status_line(saved: &SavedMemory) -> String {
    if saved.is_configured() {
        t!(
            "memory.status_configured",
            model = saved.model,
            url = saved.base_url
        )
        .into_owned()
    } else {
        t!("memory.status_unconfigured").into_owned()
    }
}

/// 把表单翻成要写入的键值。地址放第一个：它是唯一会被拒的项，拒了就什么都没写。
///
/// 地址和模型要么都填、要么都空：只填一半等于没配，存进去只会让人以为已经生效。
/// 密钥留空表示保持不变，不在补丁里。
fn save_patch(
    base_url: &str,
    model: &str,
    api_key: &str,
) -> Result<Vec<(&'static str, String)>, String> {
    let base_url = base_url.trim();
    let model = model.trim();
    let api_key = api_key.trim();
    if base_url.is_empty() != model.is_empty() {
        return Err(t!("memory.need_both").into_owned());
    }
    let mut patch = vec![
        (EMBEDDING_BASE_URL, base_url.to_string()),
        (EMBEDDING_MODEL, model.to_string()),
    ];
    if !api_key.is_empty() {
        patch.push((EMBEDDING_API_KEY, api_key.to_string()));
    }
    Ok(patch)
}

/// 测试用的参数：取表单里的地址和模型；密钥框留空时由运行时沿用已存的密钥。
fn test_params(base_url: &str, model: &str, api_key: &str) -> Result<MemoryTestParams, String> {
    let base_url = base_url.trim();
    let model = model.trim();
    if base_url.is_empty() || model.is_empty() {
        return Err(t!("memory.need_both_to_test").into_owned());
    }
    let api_key = api_key.trim();
    Ok(MemoryTestParams {
        base_url: base_url.to_string(),
        model: model.to_string(),
        api_key: (!api_key.is_empty()).then(|| api_key.to_string()),
    })
}

/// 重排模型显示用的文字。
pub(crate) fn rerank_model_label(model: Option<&RerankModel>) -> String {
    match model {
        Some(model) => format!("{} / {}", model.provider, model.id),
        None => t!("memory.rerank_auto").into_owned(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn status_line_needs_both_url_and_model() {
        let mut settings = SavedMemory::default();
        let unconfigured = "Not set up: keyword search + model reranking only";
        assert_eq!(status_line(&settings), unconfigured);
        settings.base_url = "http://localhost:11434/v1".into();
        assert_eq!(status_line(&settings), unconfigured);
        settings.model = "bge-m3".into();
        assert_eq!(
            status_line(&settings),
            "Set up: bge-m3 @ http://localhost:11434/v1"
        );
    }

    #[test]
    fn empty_key_field_keeps_the_stored_key() {
        let patch = save_patch(" http://localhost:11434/v1 ", "bge-m3", "  ").unwrap();
        assert_eq!(
            patch,
            [
                (EMBEDDING_BASE_URL, "http://localhost:11434/v1".to_string()),
                (EMBEDDING_MODEL, "bge-m3".to_string()),
            ]
        );
        let patch = save_patch("https://api.example.com/v1", "m", "sk-new").unwrap();
        assert_eq!(
            patch[0].0, EMBEDDING_BASE_URL,
            "地址必须先写，被拒时不留半份配置"
        );
        assert_eq!(patch[2], (EMBEDDING_API_KEY, "sk-new".to_string()));
    }

    #[test]
    fn testing_uses_the_form_values_and_leaves_the_stored_key_to_the_runtime() {
        let params = test_params(" http://new/v1 ", "bge-m3", "").unwrap();
        assert_eq!(params.base_url, "http://new/v1");
        assert_eq!(params.model, "bge-m3");
        assert_eq!(params.api_key, None, "密钥框留空，由运行时沿用已存的");
        assert_eq!(
            test_params("http://new/v1", "m", "sk-typed")
                .unwrap()
                .api_key
                .as_deref(),
            Some("sk-typed")
        );
        assert!(test_params("http://new/v1", "", "").is_err());
    }

    #[test]
    fn saved_settings_only_reveal_whether_a_key_is_set() {
        let result = SettingsResult {
            settings: BTreeMap::from([
                (EMBEDDING_BASE_URL.to_string(), Value::from("http://x/v1")),
                (EMBEDDING_MODEL.to_string(), Value::from("bge-m3")),
                (EMBEDDING_API_KEY.to_string(), Value::from("set")),
                (RERANK_MODEL_PROVIDER.to_string(), Value::from("")),
                (RERANK_MODEL_ID.to_string(), Value::from("")),
            ]),
        };
        let (saved, rerank) = saved_from(&result);
        assert!(saved.api_key_set);
        assert!(saved.is_configured());
        assert_eq!(rerank, None);
    }

    #[test]
    fn rerank_model_label_says_auto_when_unset() {
        assert!(rerank_model_label(None).starts_with("Automatic"));
        let model = RerankModel {
            provider: "github-copilot".into(),
            id: "gpt-6-luna".into(),
        };
        assert_eq!(
            rerank_model_label(Some(&model)),
            "github-copilot / gpt-6-luna"
        );
    }

    #[test]
    fn half_filled_form_is_rejected() {
        assert!(save_patch("http://localhost:11434/v1", "", "").is_err());
        assert!(save_patch("", "bge-m3", "").is_err());
        assert_eq!(save_patch("", "", "").unwrap().len(), 2);
    }
}
