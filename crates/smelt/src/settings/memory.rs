//! 设置页：记忆的向量检索服务（OpenAI 兼容 `/embeddings`）。
//!
//! 存取全部走 [`smelt_core::memory_settings`]，这里只管表单。表单状态挂在页面自己的
//! keyed state 上而不是 `Workspace`：它只在这一页可见时活着，每次打开页面都重新读库，
//! 不需要跨页缓存，也不该让别的页面为它多一份快照字段。
//!
//! 已存的 API Key 永远不回填进输入框：输入框留空表示「保持不变」，要删得点「清除密钥」。

use gpui::{AppContext, Context, Entity, Window};
use gpui_component::input::InputState;
use serde_json::Value;
use smelt_core::memory_settings::{
    EMBEDDING_API_KEY, EMBEDDING_BASE_URL, EMBEDDING_MODEL, EmbeddingSettings, RERANK_MODEL_ID,
    RERANK_MODEL_PROVIDER, RerankModel,
};

const KEY_SET_PLACEHOLDER: &str = "已设置";
const KEY_EMPTY_PLACEHOLDER: &str = "可选，本地服务一般不需要";

pub(crate) struct MemorySettingsForm {
    pub base_url: Entity<InputState>,
    pub model: Entity<InputState>,
    pub api_key: Entity<InputState>,
    /// 库里当前的配置；`api_key` 字段不外露，只通过 [`Self::api_key_set`] 说明设没设。
    saved: EmbeddingSettings,
    pub error: Option<String>,
    pub notice: Option<&'static str>,
    /// 用户指定的重排模型；`None` 表示自动挑。
    pub rerank_model: Option<RerankModel>,
    /// 正在跑连接测试。
    pub testing: bool,
    /// 上一次连接测试的结果：成功时是一句"可用：…"，失败时是原因。
    pub test_result: Option<Result<String, String>>,
}

impl MemorySettingsForm {
    pub fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let (saved, error) = match load() {
            Ok(saved) => (saved, None),
            Err(error) => (EmbeddingSettings::default(), Some(error)),
        };
        let base_url = cx.new(|cx| {
            InputState::new(window, cx)
                .placeholder("如 http://localhost:11434/v1")
                .default_value(saved.base_url.clone())
        });
        let model = cx.new(|cx| {
            InputState::new(window, cx)
                .placeholder("如 bge-m3")
                .default_value(saved.model.clone())
        });
        let api_key = cx.new(|cx| {
            InputState::new(window, cx)
                .masked(true)
                .placeholder(key_placeholder(&saved))
        });
        Self {
            base_url,
            model,
            api_key,
            saved,
            error,
            notice: None,
            rerank_model: load_rerank_model(),
            testing: false,
            test_result: None,
        }
    }

    /// 选重排模型：点了立刻写库。两项都空表示改回自动。
    pub fn set_rerank_model(&mut self, model: Option<RerankModel>, cx: &mut Context<Self>) {
        let (provider, id) = model
            .as_ref()
            .map(|model| (model.provider.clone(), model.id.clone()))
            .unwrap_or_default();
        match apply(&[(RERANK_MODEL_PROVIDER, provider), (RERANK_MODEL_ID, id)]) {
            Ok(()) => {
                self.rerank_model = load_rerank_model();
                self.error = None;
                self.notice = Some("已更新重排模型");
            }
            Err(error) => {
                self.error = Some(error);
                self.notice = None;
            }
        }
        cx.notify();
    }

    /// 用表单里当前的值（不必先保存）真发一次 `/embeddings` 请求。密钥框留空时用已存的密钥。
    /// 网络请求放后台执行器，不卡界面。
    pub fn test(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.testing {
            return;
        }
        let settings = match test_settings(
            &self.base_url.read(cx).value(),
            &self.model.read(cx).value(),
            &self.api_key.read(cx).value(),
            &self.saved,
        ) {
            Ok(settings) => settings,
            Err(error) => {
                self.test_result = Some(Err(error));
                cx.notify();
                return;
            }
        };
        self.testing = true;
        self.test_result = None;
        cx.notify();
        cx.spawn_in(window, async move |this, cx| {
            let outcome = cx
                .background_spawn(async move {
                    smelt_core::block_on::block_on_tokio(smelt_core::memory_settings::probe(
                        &settings,
                    ))
                    .map_err(|error| error.to_string())
                    .and_then(|result| result)
                })
                .await;
            let _ = this.update(cx, |form, cx| {
                form.testing = false;
                form.test_result = Some(outcome.map(|probe| {
                    format!(
                        "可用：向量维度 {}，耗时 {} ms",
                        probe.dimensions, probe.elapsed_ms
                    )
                }));
                cx.notify();
            });
        })
        .detach();
    }

    pub fn api_key_set(&self) -> bool {
        !self.saved.api_key.is_empty()
    }

    pub fn status_line(&self) -> String {
        status_line(&self.saved)
    }

    pub fn save(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let base_url = self.base_url.read(cx).value().to_string();
        let model = self.model.read(cx).value().to_string();
        let api_key = self.api_key.read(cx).value().to_string();
        let result = save_patch(&base_url, &model, &api_key).and_then(|patch| apply(&patch));
        self.finish(result, "已保存", window, cx);
    }

    pub fn clear_api_key(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let result = apply(&[(EMBEDDING_API_KEY, String::new())]);
        self.finish(result, "已清除密钥", window, cx);
    }

    pub fn clear_all(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let result = apply(&[
            (EMBEDDING_BASE_URL, String::new()),
            (EMBEDDING_MODEL, String::new()),
            (EMBEDDING_API_KEY, String::new()),
        ]);
        self.finish(result, "已清空配置", window, cx);
    }

    /// 写库成功后以库为准重填表单：地址和模型显示规整后的值，密钥框清空。
    /// 失败时保留用户的输入，让他改了再存。
    fn finish(
        &mut self,
        result: Result<(), String>,
        notice: &'static str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if let Err(error) = result {
            self.error = Some(error);
            self.notice = None;
            cx.notify();
            return;
        }
        match load() {
            Ok(saved) => {
                self.saved = saved;
                self.error = None;
                self.notice = Some(notice);
            }
            Err(error) => {
                self.error = Some(error);
                self.notice = None;
            }
        }
        let saved = self.saved.clone();
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
        cx.notify();
    }
}

fn key_placeholder(saved: &EmbeddingSettings) -> &'static str {
    if saved.api_key.is_empty() {
        KEY_EMPTY_PLACEHOLDER
    } else {
        KEY_SET_PLACEHOLDER
    }
}

fn status_line(saved: &EmbeddingSettings) -> String {
    if saved.is_configured() {
        format!("已配置：{} @ {}", saved.model, saved.base_url)
    } else {
        "未配置：只用关键词 + 模型重排".to_string()
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
        return Err("服务地址和模型需要同时填写；都留空表示不用向量检索".to_string());
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

/// 测试用的配置：取表单里的地址和模型；密钥框留空时沿用已存的密钥，填了就用填的。
fn test_settings(
    base_url: &str,
    model: &str,
    api_key: &str,
    saved: &EmbeddingSettings,
) -> Result<EmbeddingSettings, String> {
    let base_url = base_url.trim();
    let model = model.trim();
    if base_url.is_empty() || model.is_empty() {
        return Err("填好服务地址和模型再测试".to_string());
    }
    let api_key = api_key.trim();
    Ok(EmbeddingSettings {
        base_url: base_url.to_string(),
        model: model.to_string(),
        api_key: if api_key.is_empty() {
            saved.api_key.clone()
        } else {
            api_key.to_string()
        },
    })
}

/// 重排模型显示用的文字。
pub(crate) fn rerank_model_label(model: Option<&RerankModel>) -> String {
    match model {
        Some(model) => format!("{} / {}", model.provider, model.id),
        None => "自动（当前对话所用提供方里最便宜的）".to_string(),
    }
}

fn load_rerank_model() -> Option<RerankModel> {
    let store = smelt_core::sqlite_state::default_sqlite_store().ok()?;
    smelt_core::memory_settings::load_rerank_model_on(&store)
        .ok()
        .flatten()
}

fn load() -> Result<EmbeddingSettings, String> {
    let store = smelt_core::sqlite_state::default_sqlite_store()?;
    smelt_core::memory_settings::load_on(&store).map_err(|error| error.to_string())
}

fn apply(patch: &[(&str, String)]) -> Result<(), String> {
    let store = smelt_core::sqlite_state::default_sqlite_store()?;
    for (key, value) in patch {
        smelt_core::memory_settings::apply_on(&store, key, &Value::from(value.as_str()))?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn status_line_needs_both_url_and_model() {
        let mut settings = EmbeddingSettings::default();
        assert_eq!(status_line(&settings), "未配置：只用关键词 + 模型重排");
        settings.base_url = "http://localhost:11434/v1".into();
        assert_eq!(status_line(&settings), "未配置：只用关键词 + 模型重排");
        settings.model = "bge-m3".into();
        assert_eq!(
            status_line(&settings),
            "已配置：bge-m3 @ http://localhost:11434/v1"
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
    fn testing_uses_the_form_values_and_falls_back_to_the_stored_key() {
        let saved = EmbeddingSettings {
            base_url: "http://old/v1".into(),
            model: "old".into(),
            api_key: "sk-stored".into(),
        };
        let unsaved = test_settings(" http://new/v1 ", "bge-m3", "", &saved).unwrap();
        assert_eq!(unsaved.base_url, "http://new/v1");
        assert_eq!(unsaved.model, "bge-m3");
        assert_eq!(unsaved.api_key, "sk-stored", "密钥框留空沿用已存的");
        assert_eq!(
            test_settings("http://new/v1", "m", "sk-typed", &saved)
                .unwrap()
                .api_key,
            "sk-typed"
        );
        assert!(test_settings("http://new/v1", "", "", &saved).is_err());
    }

    #[test]
    fn rerank_model_label_says_auto_when_unset() {
        assert!(rerank_model_label(None).starts_with("自动"));
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
