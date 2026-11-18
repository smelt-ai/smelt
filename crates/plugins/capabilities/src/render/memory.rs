//! 设置页：记忆的向量检索服务。表单逻辑见 [`crate::memory`]。

use super::*;
use crate::memory::{MemorySettingsForm, rerank_model_label};
use api::model::memory_settings::RerankModel;
use smelt_plugin_api::ui::kit::TYPE;

/// 配的是能力库眼下选中的那台机器。
pub(super) fn pi_memory_page(
    entity: Entity<CapabilitiesPage>,
    page: &CapabilitiesPage,
    window: &mut Window,
    cx: &mut App,
) -> SettingsPage {
    let form = memory_form(&page.machine, window, cx);
    SettingsPage::new("capabilities-pane:memory")
        .child(page_intro(t!("memory.description")))
        .child(
            SettingsSection::new()
                .title(t!("memory.rerank_title"))
                .child(render_rerank_model(entity, form.clone(), cx)),
        )
        .child(
            SettingsSection::new()
                .title(t!("memory.vector_title"))
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
    div()
        .flex()
        .flex_col()
        .w_full()
        .gap_2()
        .child(picker)
        .child(
            div()
                .type_style(TYPE.text_xsmall)
                .text_color(muted)
                .child(t!("memory.rerank_description").into_owned()),
        )
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
    let notice = state.notice.clone();
    let api_key_set = state.api_key_set();
    let testing = state.testing;
    let test_result = state.test_result.clone();
    let (base_url, model, api_key) = (
        state.base_url.clone(),
        state.model.clone(),
        state.api_key.clone(),
    );

    let field = |label: std::borrow::Cow<'static, str>, input: &Entity<InputState>| {
        div()
            .flex()
            .flex_col()
            .w_full()
            .gap_1()
            .child(
                div()
                    .type_style(TYPE.text_xsmall)
                    .text_color(fg)
                    .child(SharedString::from(label)),
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
                    t!("memory.testing")
                } else {
                    t!("memory.test")
                })
                .disabled(testing)
                .tooltip(t!("memory.test_tooltip"))
                .on_click(move |_, window, cx| {
                    test_form.update(cx, |form, cx| form.test(window, cx));
                }),
        )
        .child(
            Button::new("save-pi-memory")
                .primary()
                .small()
                .label(t!("common.save"))
                .on_click(move |_, window, cx| {
                    save_form.update(cx, |form, cx| form.save(window, cx));
                }),
        );
    if api_key_set {
        actions = actions.child(
            Button::new("clear-pi-memory-key")
                .quiet()
                .small()
                .label(t!("memory.clear_key"))
                .on_click(move |_, window, cx| {
                    clear_key_form.update(cx, |form, cx| form.clear_api_key(window, cx));
                }),
        );
    }
    actions = actions.child(
        Button::new("clear-pi-memory")
            .quiet()
            .small()
            .label(t!("memory.clear_all"))
            .on_click(move |_, window, cx| {
                clear_all_form.update(cx, |form, cx| form.clear_all(window, cx));
            }),
    );
    if let Some(notice) = notice {
        actions = actions.child(
            div()
                .type_style(TYPE.text_xsmall)
                .text_color(muted)
                .child(SharedString::from(notice)),
        );
    }

    let mut content = div()
        .flex()
        .flex_col()
        .w_full()
        .gap_3()
        .child(
            div()
                .type_style(TYPE.text_xsmall)
                .text_color(muted)
                .child(t!("memory.form_description").into_owned()),
        )
        .child(div().type_style(TYPE.ui).text_color(fg).child(status))
        .child(field(t!("memory.base_url"), &base_url))
        .child(field(t!("memory.model"), &model))
        .child(field(t!("memory.api_key"), &api_key))
        .child(
            div()
                .type_style(TYPE.text_xsmall)
                .text_color(muted)
                .child(t!("memory.key_hint").into_owned()),
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
                    .child(t!("memory.test_failed", message = message).into_owned()),
            );
        }
        None => {}
    }
    content.child(actions).into_any_element()
}
