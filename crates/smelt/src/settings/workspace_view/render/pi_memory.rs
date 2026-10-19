//! 设置页：记忆的向量检索服务。表单逻辑见 [`crate::settings::memory`]。

use super::*;
use crate::settings::memory::{MemorySettingsForm, rerank_model_label};
use gpui_component::menu::{DropdownMenu, PopupMenuItem};
use smelt_model::memory_settings::RerankModel;

const DESCRIPTION: &str = "按问题把相关的记忆附到对话里。关键词检索总是开着；重排模型和向量检索都可选，不可用时自动退回。";

pub(super) fn pi_memory_page(_cx: &App) -> SettingPage {
    SettingPage::new("记忆 · 召回")
        .description(DESCRIPTION)
        .default_open(true)
        .group(
            SettingGroup::new()
                .title("重排模型")
                .item(SettingItem::render(|_, window, cx: &mut App| {
                    let form =
                        window.use_keyed_state("pi-memory-form", cx, MemorySettingsForm::new);
                    render_rerank_model(form, cx)
                })),
        )
        .group(
            SettingGroup::new()
                .title("向量检索服务（可选）")
                .item(SettingItem::render(|_, window, cx: &mut App| {
                    // 状态跟着这一页活：离开再回来、或重新打开窗口都会重新读库。
                    let form =
                        window.use_keyed_state("pi-memory-form", cx, MemorySettingsForm::new);
                    render_form(form, cx)
                })),
        )
}

/// 召回第一轮、压缩后和 memory_search 时，用这个模型从候选里挑相关的记忆。
fn render_rerank_model(form: Entity<MemorySettingsForm>, cx: &mut App) -> AnyElement {
    let muted = cx.theme().muted_foreground;
    let current = form.read(cx).rerank_model.clone();
    let label = rerank_model_label(current.as_ref());
    let picker = Button::new("pi-memory-rerank-model")
        .small()
        .label(label)
        .dropdown_menu(move |mut menu, window, cx| {
            let auto_form = form.clone();
            menu = menu.item(
                PopupMenuItem::new(rerank_model_label(None))
                    .checked(current.is_none())
                    .on_click(move |_, _, cx| {
                        auto_form.update(cx, |form, cx| form.set_rerank_model(None, cx));
                    }),
            );
            let groups = crate::settings::pi_model_choice_groups(cx);
            if !groups.is_empty() {
                menu = menu.separator();
            }
            for group in groups {
                let group_form = form.clone();
                let current = current.clone();
                let models = group.models;
                menu = menu.submenu(
                    group.provider_name.clone(),
                    window,
                    cx,
                    move |mut sub, _, _| {
                        for choice in &models {
                            let checked = current.as_ref().is_some_and(|model| {
                                model.provider == choice.provider && model.id == choice.id
                            });
                            let item_form = group_form.clone();
                            let model = RerankModel {
                                provider: choice.provider.clone(),
                                id: choice.id.clone(),
                            };
                            sub = sub.item(
                                PopupMenuItem::new(choice.short_label().to_string())
                                    .checked(checked)
                                    .on_click(move |_, _, cx| {
                                        let model = model.clone();
                                        item_form.update(cx, |form, cx| {
                                            form.set_rerank_model(Some(model), cx)
                                        });
                                    }),
                            );
                        }
                        sub.scrollable(true)
                    },
                );
            }
            menu
        });
    v_flex()
        .w_full()
        .gap_2()
        .child(picker)
        .child(div().text_xs().text_color(muted).child(
            "会话第一轮、压缩之后和 memory_search 时，用它从候选里挑相关的记忆。选小而快的模型即可；\
             指定的模型不可用时这一轮只用检索结果，不会换成别的模型。推理自动压到最低档。",
        ))
        .into_any_element()
}

fn render_form(form: Entity<MemorySettingsForm>, cx: &mut App) -> AnyElement {
    let (fg, muted, danger, success) = {
        let theme = cx.theme();
        (
            theme.foreground,
            theme.muted_foreground,
            theme.danger,
            theme.success,
        )
    };
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
        v_flex()
            .w_full()
            .gap_1()
            .child(div().text_xs().text_color(fg).child(label))
            .child(Input::new(input))
    };

    let test_form = form.clone();
    let save_form = form.clone();
    let clear_key_form = form.clone();
    let clear_all_form = form.clone();

    let mut actions = h_flex()
        .gap_2()
        .items_center()
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
                .ghost()
                .small()
                .label("清除密钥")
                .on_click(move |_, window, cx| {
                    clear_key_form.update(cx, |form, cx| form.clear_api_key(window, cx));
                }),
        );
    }
    actions = actions.child(
        Button::new("clear-pi-memory")
            .ghost()
            .small()
            .label("清空配置")
            .on_click(move |_, window, cx| {
                clear_all_form.update(cx, |form, cx| form.clear_all(window, cx));
            }),
    );
    if let Some(notice) = notice {
        actions = actions.child(div().text_xs().text_color(muted).child(notice));
    }

    let mut content = v_flex()
        .w_full()
        .gap_3()
        .child(div().text_xs().text_color(muted).child(
            "填一个 OpenAI 兼容的 /embeddings 服务（含 Ollama 等本地服务，如 http://localhost:11434/v1）。\
             没配或服务不可用时，只用关键词 + 模型重排。",
        ))
        .child(div().text_sm().text_color(fg).child(status))
        .child(field("服务地址", &base_url))
        .child(field("模型", &model))
        .child(field("API Key（可选）", &api_key))
        .child(
            div()
                .text_xs()
                .text_color(muted)
                .child("API Key 留空表示保持不变；已存的密钥不会显示。"),
        );
    if let Some(error) = error {
        content = content.child(div().text_xs().text_color(danger).child(error));
    }
    match test_result {
        Some(Ok(message)) => {
            content = content.child(div().text_xs().text_color(success).child(message));
        }
        Some(Err(message)) => {
            content = content.child(
                div()
                    .text_xs()
                    .text_color(danger)
                    .child(format!("测试失败：{message}")),
            );
        }
        None => {}
    }
    content.child(actions).into_any_element()
}
