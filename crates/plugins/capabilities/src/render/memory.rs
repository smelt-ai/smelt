//! 设置页：记忆的向量检索服务。表单逻辑见 [`crate::memory`]。

use super::*;
use crate::memory::{MemorySettingsForm, rerank_model_label};
use api::model::memory_settings::RerankModel;
use smelt_plugin_api::ui::kit::TYPE;

const DESCRIPTION: &str = "按问题把相关的记忆附到对话里。关键词检索总是开着；重排模型和向量检索都可选，不可用时自动退回。";

/// 配的是能力库眼下选中的那台机器。
pub(super) fn pi_memory_page(
    entity: Entity<CapabilitiesPage>,
    page: &CapabilitiesPage,
    window: &mut Window,
    cx: &mut App,
) -> SettingsPage {
    let form = memory_form(&page.machine, window, cx);
    SettingsPage::new("capabilities-pane:memory")
        .child(page_intro(DESCRIPTION))
        .child(
            SettingsSection::new()
                .title("重排模型")
                .child(render_rerank_model(entity, form.clone(), cx)),
        )
        .child(
            SettingsSection::new()
                .title("向量检索服务（可选）")
                .child(render_form(form, cx)),
        )
}

/// 那台机器的记忆表单。状态跟着这一页活，按机器分开：离开再回来、重新打开窗口、换一台
/// 机器都会重新读那台机器的配置。
fn memory_form(
    runtime: &RuntimeKey,
    window: &mut Window,
    cx: &mut App,
) -> Entity<MemorySettingsForm> {
    let key = match runtime {
        RuntimeKey::Local => "pi-memory-form".to_string(),
        RuntimeKey::Remote(id) => format!("pi-memory-form-{id}"),
    };
    window.use_keyed_state(key, cx, |window, cx| {
        MemorySettingsForm::new(runtime, window, cx)
    })
}

/// 召回第一轮、压缩后和 memory_search 时，用这个模型从候选里挑相关的记忆。
fn render_rerank_model(
    page: Entity<CapabilitiesPage>,
    form: Entity<MemorySettingsForm>,
    cx: &mut App,
) -> AnyElement {
    let muted = rgb(ui_theme::muted());
    let current = form.read(cx).rerank_model.clone();
    let label = rerank_model_label(current.as_ref());
    let trigger = Button::new("pi-memory-rerank-model").small().label(label);
    let picker = MenuTrigger::dropdown("pi-memory-rerank-model-menu", trigger, move |_, cx| {
        let auto_form = form.clone();
        let mut menu = Menu::new().item(
            MenuItem::new(rerank_model_label(None))
                .checked(current.is_none())
                .on_click(move |_, _, cx| {
                    auto_form.update(cx, |form, cx| form.set_rerank_model(None, cx));
                }),
        );
        // 重排在那台机器的运行时上跑，模型也从那台机器的里选。表单按机器分开，和页面
        // 在配的是同一台。
        let groups = page.read(cx).pi_model_choice_groups();
        if !groups.is_empty() {
            menu = menu.separator();
        }
        for group in groups {
            let mut sub = Menu::new();
            for choice in &group.models {
                let checked = current.as_ref().is_some_and(|model| {
                    model.provider == choice.provider && model.id == choice.id
                });
                let item_form = form.clone();
                let model = RerankModel {
                    provider: choice.provider.clone(),
                    id: choice.id.clone(),
                };
                sub = sub.item(
                    MenuItem::new(choice.short_label().to_string())
                        .checked(checked)
                        .on_click(move |_, _, cx| {
                            let model = model.clone();
                            item_form.update(cx, |form, cx| form.set_rerank_model(Some(model), cx));
                        }),
                );
            }
            menu = menu.item(MenuItem::new(group.provider_name.clone()).submenu(sub));
        }
        menu
    });
    div().flex().flex_col()
        .w_full()
        .gap_2()
        .child(picker)
        .child(div().type_style(TYPE.text_xsmall).text_color(muted).child(
            "会话第一轮、压缩之后和 memory_search 时，用它从候选里挑相关的记忆。选小而快的模型即可；\
             指定的模型不可用时这一轮只用检索结果，不会换成别的模型。推理自动压到最低档。",
        ))
        .into_any_element()
}

fn render_form(form: Entity<MemorySettingsForm>, cx: &mut App) -> AnyElement {
    let fg = rgb(ui_theme::text());
    let muted = rgb(ui_theme::muted());
    let danger = rgb(ui_theme::bad());
    let success = rgb(ui_theme::ok());
    let state = form.read(cx);
    let status = state.status_line();
    let error = state.error.clone();
    let notice = state.notice;
    let api_key_set = state.api_key_set();
    let testing = state.testing;
    let test_result = state.test_result.clone();
    let (base_url, model, api_key) = (
        state.base_url.clone(),
        state.model.clone(),
        state.api_key.clone(),
    );

    let field = |label: &'static str, input: &Entity<InputState>| {
        div()
            .flex()
            .flex_col()
            .w_full()
            .gap_1()
            .child(
                div()
                    .type_style(TYPE.text_xsmall)
                    .text_color(fg)
                    .child(label),
            )
            .child(TextInput::new(input))
    };

    let test_form = form.clone();
    let save_form = form.clone();
    let clear_key_form = form.clone();
    let clear_all_form = form.clone();

    let mut actions = div()
        .flex()
        .items_center()
        .gap_2()
        .child(
            Button::new("test-pi-memory")
                .small()
                .label(if testing {
                    "测试中…"
                } else {
                    "测试连接"
                })
                .disabled(testing)
                .tooltip("用上面填的值发一次请求，确认服务可用（不必先保存）")
                .on_click(move |_, window, cx| {
                    test_form.update(cx, |form, cx| form.test(window, cx));
                }),
        )
        .child(
            Button::new("save-pi-memory")
                .primary()
                .small()
                .label("保存")
                .on_click(move |_, window, cx| {
                    save_form.update(cx, |form, cx| form.save(window, cx));
                }),
        );
    if api_key_set {
        actions = actions.child(
            Button::new("clear-pi-memory-key")
                .quiet()
                .small()
                .label("清除密钥")
                .on_click(move |_, window, cx| {
                    clear_key_form.update(cx, |form, cx| form.clear_api_key(window, cx));
                }),
        );
    }
    actions = actions.child(
        Button::new("clear-pi-memory")
            .quiet()
            .small()
            .label("清空配置")
            .on_click(move |_, window, cx| {
                clear_all_form.update(cx, |form, cx| form.clear_all(window, cx));
            }),
    );
    if let Some(notice) = notice {
        actions = actions.child(
            div()
                .type_style(TYPE.text_xsmall)
                .text_color(muted)
                .child(notice),
        );
    }

    let mut content = div().flex().flex_col()
        .w_full()
        .gap_3()
        .child(div().type_style(TYPE.text_xsmall).text_color(muted).child(
            "填一个 OpenAI 兼容的 /embeddings 服务（含 Ollama 等本地服务，如 http://localhost:11434/v1）。\
             没配或服务不可用时，只用关键词 + 模型重排。",
        ))
        .child(div().type_style(TYPE.ui).text_color(fg).child(status))
        .child(field("服务地址", &base_url))
        .child(field("模型", &model))
        .child(field("API Key（可选）", &api_key))
        .child(
            div()
                .type_style(TYPE.text_xsmall)
                .text_color(muted)
                .child("API Key 留空表示保持不变；已存的密钥不会显示。"),
        );
    if let Some(error) = error {
        content = content.child(
            div()
                .type_style(TYPE.text_xsmall)
                .text_color(danger)
                .child(error),
        );
    }
    match test_result {
        Some(Ok(message)) => {
            content = content.child(
                div()
                    .type_style(TYPE.text_xsmall)
                    .text_color(success)
                    .child(message),
            );
        }
        Some(Err(message)) => {
            content = content.child(
                div()
                    .type_style(TYPE.text_xsmall)
                    .text_color(danger)
                    .child(format!("测试失败：{message}")),
            );
        }
        None => {}
    }
    content.child(actions).into_any_element()
}
