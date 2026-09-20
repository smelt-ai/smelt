//! 设置页：Pi 模型配置。

use super::*;
use crate::settings::workspace::PiConfigFileKind;

pub(super) fn pi_model_page(
    entity: Entity<Workspace>,
    snapshot: &SettingsRenderSnapshot,
    _cx: &App,
) -> SettingPage {
    let (pi_settings_path, pi_model_status) = match &snapshot.pi_settings {
        Ok(settings) => {
            let status = match &settings.default_model {
                Some(model) => {
                    let provider = model.provider.as_deref().unwrap_or("未指定提供方");
                    let model_name = model.model.as_deref().unwrap_or("未指定模型");
                    match model.thinking_level.as_deref() {
                        Some(thinking_level) => {
                            format!("{provider} / {model_name} · 推理强度 {thinking_level}")
                        }
                        None => format!("{provider} / {model_name}"),
                    }
                }
                None => "尚未设置默认模型".to_string(),
            };
            (settings.path.display().to_string(), status)
        }
        Err(error) => (
            smelt_core::pi_model_settings::pi_settings_path()
                .display()
                .to_string(),
            format!("无法读取模型设置：{error}"),
        ),
    };

    SettingPage::new("Pi 模型")
        .default_open(true)
        .group(
            SettingGroup::new()
                .title("模型与凭据")
                .description("智能体（Pi）的模型、凭据与自定义 Provider 配置；与 ~/.pi/agent/ 原生配置完全同步。")
                .item(SettingItem::new(
                    "模型设置文件",
                    SettingField::render(move |_, _, _| {
                        div()
                            .text_sm()
                            .child(pi_settings_path.clone())
                            .into_any_element()
                    }),
                ))
                .item(SettingItem::new(
                    "当前默认模型",
                    SettingField::render(move |_, _, _| {
                        div()
                            .text_sm()
                            .child(pi_model_status.clone())
                            .into_any_element()
                    }),
                ))
                .item(SettingItem::render({
                    let manage_entity = entity.clone();
                    let model_editor = snapshot.pi_model_editor.clone();
                    let custom_editor = snapshot.pi_custom_provider_editor.clone();
                    let model_editor_error = snapshot.pi_model_editor_error.clone();
                    let pi_model_settings = snapshot.pi_model_settings.clone();

                    move |_, _window, cx: &mut App| {
                        let (fg, muted, border, danger_fg) = {
                            let theme = cx.theme();
                            (
                                theme.foreground,
                                theme.muted_foreground,
                                theme.border,
                                theme.danger_foreground,
                            )
                        };

                        let configure_model_entity = manage_entity.clone();
                        let add_custom_entity = manage_entity.clone();
                        let settings_file_entity = manage_entity.clone();
                        let models_file_entity = manage_entity.clone();
                        let credentials_entity = manage_entity.clone();
                        let refresh_entity = manage_entity.clone();

                        let default_provider_summary = match pi_model_settings.as_ref() {
                            Err(error) => format!("读取失败：{error}"),
                            Ok(settings) => {
                                let key_status = if settings.credential_configured {
                                    "凭据已配置"
                                } else {
                                    "缺少凭据"
                                };
                                format!(
                                    "{} / {} · {}",
                                    settings.default_model.provider,
                                    settings.default_model.model,
                                    key_status
                                )
                            }
                        };

                        let mut content = v_flex()
                            .w_full()
                            .gap_4()
                            .child(
                                v_flex()
                                    .w_full()
                                    .gap_2()
                                    .child(
                                        h_flex()
                                            .w_full()
                                            .items_center()
                                            .justify_between()
                                            .child(
                                                v_flex()
                                                    .gap_0p5()
                                                    .child(
                                                        h_flex()
                                                            .gap_2()
                                                            .items_center()
                                                            .child(
                                                                div()
                                                                    .text_sm()
                                                                    .font_semibold()
                                                                    .text_color(fg)
                                                                    .child("默认模型配置"),
                                                            )
                                                            .child(
                                                                div()
                                                                    .px_2()
                                                                    .py_0p5()
                                                                    .rounded_full()
                                                                    .bg(crate::ui_theme::tint(
                                                                        crate::ui_theme::green(),
                                                                        0x22,
                                                                    ))
                                                                    .text_xs()
                                                                    .text_color(rgb(
                                                                        crate::ui_theme::green(),
                                                                    ))
                                                                    .child("官方 / 默认"),
                                                            ),
                                                    )
                                                    .child(
                                                        div()
                                                            .text_xs()
                                                            .text_color(muted)
                                                            .child(default_provider_summary),
                                                    ),
                                            )
                                            .child(
                                                Button::new("pi-configure-default-model")
                                                    .small()
                                                    .label(if model_editor.is_some() {
                                                        "配置中"
                                                    } else {
                                                        "配置"
                                                    })
                                                    .disabled(model_editor.is_some())
                                                    .on_click(move |_, window, cx| {
                                                        configure_model_entity.update(
                                                            cx,
                                                            |workspace, cx| {
                                                                workspace
                                                                    .edit_pi_model_settings(window, cx);
                                                            },
                                                        );
                                                    }),
                                            ),
                                    )
                                    .p_3()
                                    .rounded_lg()
                                    .border_1()
                                    .border_color(border),
                            )
                            .child(
                                v_flex()
                                    .w_full()
                                    .gap_2()
                                    .child(
                                        h_flex()
                                            .w_full()
                                            .items_center()
                                            .justify_between()
                                            .child(
                                                v_flex()
                                                    .gap_0p5()
                                                    .child(
                                                        div()
                                                            .text_sm()
                                                            .font_semibold()
                                                            .text_color(fg)
                                                            .child("自定义 Provider"),
                                                    )
                                                    .child(
                                                        div()
                                                            .text_xs()
                                                            .text_color(muted)
                                                            .child(
                                                                "OpenAI / Anthropic 兼容网关、自托管服务或其他模型端点",
                                                            ),
                                                    ),
                                            )
                                            .child(
                                                Button::new("pi-add-custom-provider")
                                                    .secondary()
                                                    .small()
                                                    .icon(IconName::Plus)
                                                    .label("添加")
                                                    .disabled(custom_editor.is_some())
                                                    .on_click(move |_, window, cx| {
                                                        add_custom_entity.update(
                                                            cx,
                                                            |workspace, cx| {
                                                                workspace
                                                                    .edit_pi_custom_provider(
                                                                        None, window, cx,
                                                                    );
                                                            },
                                                        );
                                                    }),
                                            ),
                                    )
                                    .children(
                                        pi_model_settings
                                            .as_ref()
                                            .ok()
                                            .into_iter()
                                            .flat_map(|settings| settings.custom_providers.iter())
                                            .map(|provider| {
                                                let edit_entity = manage_entity.clone();
                                                let delete_entity = manage_entity.clone();
                                                let provider_id = provider.id.clone();
                                                let delete_id = provider.id.clone();
                                                let is_default = pi_model_settings
                                                    .as_ref()
                                                    .is_ok_and(|settings| {
                                                        settings.default_model.provider == provider.id
                                                    });
                                                let credential = if provider.credential_configured {
                                                    "凭据已配置"
                                                } else {
                                                    "缺少凭据"
                                                };
                                                h_flex()
                                                    .w_full()
                                                    .items_center()
                                                    .justify_between()
                                                    .gap_3()
                                                    .p_3()
                                                    .rounded_lg()
                                                    .border_1()
                                                    .border_color(border)
                                                    .child(
                                                        v_flex()
                                                            .min_w_0()
                                                            .gap_0p5()
                                                            .child(
                                                                h_flex()
                                                                    .gap_2()
                                                                    .items_center()
                                                                    .child(
                                                                        div()
                                                                            .text_sm()
                                                                            .font_semibold()
                                                                            .text_color(fg)
                                                                            .child(
                                                                                provider
                                                                                    .display_name
                                                                                    .clone(),
                                                                            ),
                                                                    )
                                                                    .child(
                                                                        div()
                                                                            .px_2()
                                                                            .py_0p5()
                                                                            .rounded_full()
                                                                            .bg(
                                                                                crate::ui_theme::overlay(
                                                                                    0x18,
                                                                                ),
                                                                            )
                                                                            .text_xs()
                                                                            .text_color(muted)
                                                                            .child("自定义"),
                                                                    )
                                                                    .children(is_default.then(
                                                                        || {
                                                                            div()
                                                                                .px_2()
                                                                                .py_0p5()
                                                                                .rounded_full()
                                                                                .bg(
                                                                                    crate::ui_theme::tint(
                                                                                        crate::ui_theme::purple(),
                                                                                        0x22,
                                                                                    ),
                                                                                )
                                                                                .text_xs()
                                                                                .text_color(rgb(
                                                                                    crate::ui_theme::purple(),
                                                                                ))
                                                                                .child("当前默认")
                                                                        },
                                                                    )),
                                                            )
                                                            .child(
                                                                div()
                                                                    .truncate()
                                                                    .text_xs()
                                                                    .text_color(muted)
                                                                    .child(format!(
                                                                        "{} · {} 个模型 · {}",
                                                                        provider.id,
                                                                        provider.models.len(),
                                                                        credential
                                                                    )),
                                                            ),
                                                    )
                                                    .child(
                                                        h_flex()
                                                            .gap_2()
                                                            .child(
                                                                Button::new(format!(
                                                                    "pi-edit-custom-provider-{}",
                                                                    provider.id
                                                                ))
                                                                .secondary()
                                                                .small()
                                                                .label("编辑")
                                                                .disabled(custom_editor.is_some())
                                                                .on_click(move |_, window, cx| {
                                                                    let provider_id =
                                                                        provider_id.clone();
                                                                    edit_entity.update(
                                                                        cx,
                                                                        |workspace, cx| {
                                                                            workspace.edit_pi_custom_provider(
                                                                                Some(provider_id),
                                                                                window,
                                                                                cx,
                                                                            );
                                                                        },
                                                                    );
                                                                }),
                                                            )
                                                            .child(
                                                                Button::new(format!(
                                                                    "pi-delete-custom-provider-{}",
                                                                    delete_id
                                                                ))
                                                                .ghost()
                                                                .small()
                                                                .icon(IconName::Delete)
                                                                .tooltip("删除此 Provider")
                                                                .disabled(custom_editor.is_some())
                                                                .on_click(move |_, window, cx| {
                                                                    let delete_id = delete_id.clone();
                                                                    delete_entity.update(
                                                                        cx,
                                                                        |workspace, cx| {
                                                                            workspace.remove_pi_custom_provider(
                                                                                delete_id,
                                                                                window,
                                                                                cx,
                                                                            );
                                                                        },
                                                                    );
                                                                }),
                                                            ),
                                                    )
                                            }),
                                    ),
                            )
                            .child(
                                h_flex()
                                    .w_full()
                                    .items_center()
                                    .gap_2()
                                    .flex_wrap()
                                    .child(
                                        Button::new("pi-edit-settings-file")
                                            .ghost()
                                            .small()
                                            .label("编辑原生设置文件")
                                            .on_click(move |_, window, cx| {
                                                settings_file_entity.update(cx, |workspace, cx| {
                                                    workspace.open_pi_config(
                                                        PiConfigFileKind::Settings,
                                                        window,
                                                        cx,
                                                    );
                                                });
                                            }),
                                    )
                                    .child(
                                        Button::new("pi-edit-models-file")
                                            .ghost()
                                            .small()
                                            .label("编辑自定义模型文件")
                                            .on_click(move |_, window, cx| {
                                                models_file_entity.update(cx, |workspace, cx| {
                                                    workspace.open_pi_config(
                                                        PiConfigFileKind::Models,
                                                        window,
                                                        cx,
                                                    );
                                                });
                                            }),
                                    )
                                    .child(
                                        Button::new("pi-edit-credentials-file")
                                            .ghost()
                                            .small()
                                            .label("编辑原生凭据文件")
                                            .on_click(move |_, window, cx| {
                                                credentials_entity.update(cx, |workspace, cx| {
                                                    workspace.open_pi_config(
                                                        PiConfigFileKind::Credentials,
                                                        window,
                                                        cx,
                                                    );
                                                });
                                            }),
                                    )
                                    .child(
                                        Button::new("pi-refresh-settings")
                                            .ghost()
                                            .small()
                                            .label("刷新")
                                            .on_click(move |_, _, cx| {
                                                refresh_entity.update(cx, |workspace, cx| {
                                                    workspace.refresh_pi_settings(cx);
                                                });
                                            }),
                                    ),
                            );

                        if let Some(error) = &model_editor_error {
                            content = content.child(
                                div()
                                    .text_xs()
                                    .text_color(danger_fg)
                                    .child(format!("操作失败：{error}")),
                            );
                        }

                        if let Some(editor) = &model_editor {
                            let save_entity = manage_entity.clone();
                            let cancel_entity = manage_entity.clone();
                            let credential_status = if editor.credential_configured {
                                "当前 API key 已配置；留空不会改动它。"
                            } else {
                                "尚未检测到 API key；请在下方输入。"
                            };
                            let thinking_level = editor.thinking_level.clone();

                            content = content.child(
                                v_flex()
                                    .w_full()
                                    .gap_4()
                                    .rounded_lg()
                                    .border_1()
                                    .border_color(border)
                                    .p_4()
                                    .child(
                                        v_flex()
                                            .gap_1()
                                            .child(
                                                div()
                                                    .text_sm()
                                                    .font_semibold()
                                                    .text_color(fg)
                                                    .child("默认模型配置"),
                                            )
                                            .child(
                                                div()
                                                    .text_xs()
                                                    .text_color(muted)
                                                    .child(credential_status),
                                            ),
                                    )
                                    .child(
                                        h_flex()
                                            .w_full()
                                            .gap_3()
                                            .items_start()
                                            .child(
                                                v_flex()
                                                    .flex_1()
                                                    .min_w_0()
                                                    .gap_1()
                                                    .child(
                                                        div()
                                                            .text_xs()
                                                            .font_semibold()
                                                            .text_color(fg)
                                                            .child("提供方 (Provider)"),
                                                    )
                                                    .child(Input::new(&editor.provider)),
                                            )
                                            .child(
                                                v_flex()
                                                    .flex_1()
                                                    .min_w_0()
                                                    .gap_1()
                                                    .child(
                                                        div()
                                                            .text_xs()
                                                            .font_semibold()
                                                            .text_color(fg)
                                                            .child("默认模型 ID"),
                                                    )
                                                    .child(Input::new(&editor.model)),
                                            ),
                                    )
                                    .child(
                                        h_flex()
                                            .w_full()
                                            .gap_3()
                                            .items_start()
                                            .child(
                                                v_flex()
                                                    .flex_1()
                                                    .min_w_0()
                                                    .gap_1()
                                                    .child(
                                                        div()
                                                            .text_xs()
                                                            .font_semibold()
                                                            .text_color(fg)
                                                            .child("API 地址 (可选)"),
                                                    )
                                                    .child(Input::new(&editor.base_url)),
                                            )
                                            .child(
                                                v_flex()
                                                    .flex_1()
                                                    .min_w_0()
                                                    .gap_1()
                                                    .child(
                                                        div()
                                                            .text_xs()
                                                            .font_semibold()
                                                            .text_color(fg)
                                                            .child("API key"),
                                                    )
                                                    .child(Input::new(&editor.api_key))
                                                    .child(
                                                        div()
                                                            .text_xs()
                                                            .text_color(muted)
                                                            .child("凭据将安全存放于 ~/.pi/agent/auth.json"),
                                                    ),
                                            ),
                                    )
                                    .child(
                                        v_flex()
                                            .w_full()
                                            .gap_1()
                                            .child(
                                                div()
                                                    .text_xs()
                                                    .font_semibold()
                                                    .text_color(fg)
                                                    .child("推理强度 (Thinking Level)"),
                                            )
                                            .child(
                                                h_flex()
                                                    .gap_2()
                                                    .children(["off", "low", "medium", "high"].iter().map(|level| {
                                                        let level_str = level.to_string();
                                                        let is_selected = thinking_level == level_str;
                                                        let level_entity = manage_entity.clone();
                                                        let level_val = level_str.clone();
                                                        let btn = Button::new(format!("pi-thinking-{level}"))
                                                            .small()
                                                            .label(level_str);
                                                        if is_selected {
                                                            btn.primary()
                                                        } else {
                                                            btn.secondary()
                                                        }
                                                        .on_click(move |_, _, cx| {
                                                            let level_val = level_val.clone();
                                                            level_entity.update(cx, |workspace, cx| {
                                                                workspace.set_pi_thinking_level(level_val, cx);
                                                            });
                                                        })
                                                    })),
                                            ),
                                    )
                                    .child(
                                        h_flex()
                                            .w_full()
                                            .gap_2()
                                            .justify_end()
                                            .child(
                                                Button::new("pi-cancel-model-settings")
                                                    .ghost()
                                                    .small()
                                                    .label("取消")
                                                    .on_click(move |_, _, cx| {
                                                        cancel_entity.update(cx, |workspace, cx| {
                                                            workspace.cancel_pi_model_edit(cx);
                                                        });
                                                    }),
                                            )
                                            .child(
                                                Button::new("pi-save-model-settings")
                                                    .small()
                                                    .label("保存配置")
                                                    .on_click(move |_, window, cx| {
                                                        save_entity.update(cx, |workspace, cx| {
                                                            workspace.save_pi_model_settings(window, cx);
                                                        });
                                                    }),
                                            ),
                                    ),
                            );
                        }

                        if let Some(editor) = &custom_editor {
                            let api_entity = manage_entity.clone();
                            let add_model_entity = manage_entity.clone();
                            let discover_entity = manage_entity.clone();
                            let discovery_state = editor.discovery.clone();
                            let manual_rows = editor.models.len();
                            let discovery_busy =
                                matches!(discovery_state, Some(ModelDiscoveryState::Loading));
                            let adopted_models = editor
                                .models
                                .iter()
                                .map(|row| row.id.read(cx).value().trim().to_string())
                                .filter(|id| !id.is_empty())
                                .collect::<Vec<_>>();
                            let cancel_entity = manage_entity.clone();
                            let save_entity = manage_entity.clone();
                            let save_default_entity = manage_entity.clone();

                            let credential_status = if editor.credential_configured {
                                "当前凭据已配置；API key 留空不会改动它。"
                            } else {
                                "尚未配置凭据；无鉴权端点可留空 API key。"
                            };

                            let mut model_rows = v_flex().w_full().gap_2();
                            for (index, model) in editor.models.iter().enumerate() {
                                let remove_entity = manage_entity.clone();
                                model_rows = model_rows.child(
                                    v_flex()
                                        .w_full()
                                        .gap_2()
                                        .p_3()
                                        .rounded_lg()
                                        .border_1()
                                        .border_color(border)
                                        .child(
                                            h_flex()
                                                .w_full()
                                                .gap_2()
                                                .items_center()
                                                .child(
                                                    div()
                                                        .text_xs()
                                                        .font_semibold()
                                                        .text_color(fg)
                                                        .child(format!("模型 {}", index + 1)),
                                                )
                                                .child(div().flex_1())
                                                .child(
                                                    Button::new(format!("pi-remove-custom-model-{index}"))
                                                        .ghost()
                                                        .small()
                                                        .icon(IconName::Delete)
                                                        .tooltip("移除此模型")
                                                        .on_click(move |_, _, cx| {
                                                            remove_entity.update(cx, |workspace, cx| {
                                                                workspace.remove_pi_custom_model(index, cx);
                                                            });
                                                        }),
                                                ),
                                        )
                                        .child(
                                            h_flex()
                                                .w_full()
                                                .gap_3()
                                                .items_start()
                                                .child(
                                                    v_flex()
                                                        .flex_1()
                                                        .min_w_0()
                                                        .gap_1()
                                                        .child(
                                                            div()
                                                                .text_xs()
                                                                .text_color(muted)
                                                                .child("模型 ID"),
                                                        )
                                                        .child(Input::new(&model.id)),
                                                )
                                                .child(
                                                    v_flex()
                                                        .flex_1()
                                                        .min_w_0()
                                                        .gap_1()
                                                        .child(
                                                            div()
                                                                .text_xs()
                                                                .text_color(muted)
                                                                .child("显示名称"),
                                                        )
                                                        .child(Input::new(&model.name)),
                                                ),
                                        )
                                        .child(
                                            h_flex()
                                                .w_full()
                                                .gap_3()
                                                .items_start()
                                                .child(
                                                    v_flex()
                                                        .flex_1()
                                                        .min_w_0()
                                                        .gap_1()
                                                        .child(
                                                            div()
                                                                .text_xs()
                                                                .text_color(muted)
                                                                .child("上下文长度（可选）"),
                                                        )
                                                        .child(Input::new(&model.context_window)),
                                                )
                                                .child(
                                                    v_flex()
                                                        .flex_1()
                                                        .min_w_0()
                                                        .gap_1()
                                                        .child(
                                                            div()
                                                                .text_xs()
                                                                .text_color(muted)
                                                                .child("最大输出（可选）"),
                                                        )
                                                        .child(Input::new(&model.max_tokens)),
                                                ),
                                        ),
                                );
                            }

                            content = content.child(
                                v_flex()
                                    .w_full()
                                    .gap_4()
                                    .p_4()
                                    .rounded_lg()
                                    .border_1()
                                    .border_color(border)
                                    .child(
                                        v_flex()
                                            .gap_1()
                                            .child(
                                                h_flex()
                                                    .gap_2()
                                                    .items_center()
                                                    .child(
                                                        div()
                                                            .text_sm()
                                                            .font_semibold()
                                                            .text_color(fg)
                                                            .child(if editor.previous_id.is_some() {
                                                                "编辑自定义 Provider"
                                                            } else {
                                                                "添加自定义 Provider"
                                                            }),
                                                    )
                                                    .child(
                                                        div()
                                                            .px_2()
                                                            .py_0p5()
                                                            .rounded_full()
                                                            .bg(crate::ui_theme::overlay(0x18))
                                                            .text_xs()
                                                            .text_color(muted)
                                                            .child("原生 Pi"),
                                                    ),
                                            )
                                            .child(
                                                div()
                                                    .text_xs()
                                                    .text_color(muted)
                                                    .child(credential_status),
                                            ),
                                    )
                                    .child(
                                        h_flex()
                                            .w_full()
                                            .gap_3()
                                            .items_start()
                                            .child(
                                                v_flex()
                                                    .flex_1()
                                                    .min_w_0()
                                                    .gap_1()
                                                    .child(
                                                        div()
                                                            .text_xs()
                                                            .font_semibold()
                                                            .text_color(fg)
                                                            .child("Provider ID"),
                                                    )
                                                    .child(Input::new(&editor.id))
                                                    .child(
                                                        div()
                                                            .text_xs()
                                                            .text_color(muted)
                                                            .child("字母开头，可含字母、数字和短连字符"),
                                                    ),
                                            )
                                            .child(
                                                v_flex()
                                                    .flex_1()
                                                    .min_w_0()
                                                    .gap_1()
                                                    .child(
                                                        div()
                                                            .text_xs()
                                                            .font_semibold()
                                                            .text_color(fg)
                                                            .child("显示名称（可选）"),
                                                    )
                                                    .child(Input::new(&editor.display_name)),
                                            ),
                                    )
                                    .child(
                                        h_flex()
                                            .w_full()
                                            .gap_3()
                                            .items_end()
                                            .child(
                                                v_flex()
                                                    .flex_1()
                                                    .min_w_0()
                                                    .gap_1()
                                                    .child(
                                                        div()
                                                            .text_xs()
                                                            .font_semibold()
                                                            .text_color(fg)
                                                            .child("API 地址"),
                                                    )
                                                    .child(Input::new(&editor.base_url)),
                                            )
                                            .child(
                                                v_flex()
                                                    .w(px(220.))
                                                    .gap_1()
                                                    .child(
                                                        div()
                                                            .text_xs()
                                                            .font_semibold()
                                                            .text_color(fg)
                                                            .child("协议"),
                                                    )
                                                    .child(
                                                        Button::new("pi-custom-provider-api")
                                                            .secondary()
                                                            .small()
                                                            .label(editor.api.clone())
                                                            .dropdown_menu(move |mut menu, _window, _cx| {
                                                                for api in smelt_core::provider_api::PROVIDER_API_PROTOCOLS {
                                                                    let api_entity = api_entity.clone();
                                                                    menu = menu.item(
                                                                        PopupMenuItem::new(*api).on_click(
                                                                            move |_ev, _window, cx| {
                                                                                api_entity.update(cx, |workspace, cx| {
                                                                                    workspace.set_pi_custom_api(api, cx);
                                                                                });
                                                                            },
                                                                        ),
                                                                    );
                                                                }
                                                                menu
                                                            }),
                                                    ),
                                            ),
                                    )
                                    .child(
                                        v_flex()
                                            .w_full()
                                            .gap_1()
                                            .child(
                                                div()
                                                    .text_xs()
                                                    .font_semibold()
                                                    .text_color(fg)
                                                    .child("API key"),
                                            )
                                            .child(Input::new(&editor.api_key))
                                            .child(
                                                div()
                                                    .text_xs()
                                                    .text_color(muted)
                                                    .child("凭据存放于 ~/.pi/agent/auth.json"),
                                            ),
                                    )
                                    .child(
                                        h_flex()
                                            .w_full()
                                            .items_center()
                                            .justify_between()
                                            .child(
                                                div()
                                                    .text_sm()
                                                    .font_semibold()
                                                    .text_color(fg)
                                                    .child("模型目录"),
                                            )
                                            .child(
                                                h_flex()
                                                    .gap_2()
                                                    .child(
                                                        Button::new("pi-discover-custom-models")
                                                            .secondary()
                                                            .small()
                                                            .label(if discovery_busy {
                                                                "正在读取…"
                                                            } else {
                                                                "获取模型列表"
                                                            })
                                                            .disabled(discovery_busy)
                                                            .on_click(move |_, _, cx| {
                                                                discover_entity.update(
                                                                    cx,
                                                                    |workspace, cx| {
                                                                        workspace
                                                                            .discover_pi_custom_models(cx);
                                                                    },
                                                                );
                                                            }),
                                                    )
                                                    .child(
                                                        Button::new("pi-add-custom-model")
                                                            .secondary()
                                                            .small()
                                                            .icon(IconName::Plus)
                                                            .label("添加模型")
                                                            .on_click(move |_, window, cx| {
                                                                add_model_entity.update(
                                                                    cx,
                                                                    |workspace, cx| {
                                                                        workspace
                                                                            .add_pi_custom_model(window, cx);
                                                                    },
                                                                );
                                                            }),
                                                    ),
                                            ),
                                    )
                                    .child(render_model_discovery_section(
                                        "pi",
                                        discovery_state,
                                        adopted_models,
                                        manual_rows,
                                        manage_entity.clone(),
                                        Workspace::adopt_pi_discovered_model,
                                        muted,
                                        danger_fg,
                                    ))
                                    .child(model_rows)
                                    .children(model_editor_error.clone().map(|error| {
                                        div()
                                            .text_xs()
                                            .text_color(danger_fg)
                                            .child(error)
                                    }))
                                    .child(
                                        h_flex()
                                            .w_full()
                                            .gap_2()
                                            .justify_end()
                                            .child(
                                                Button::new("pi-cancel-custom-provider")
                                                    .ghost()
                                                    .small()
                                                    .label("取消")
                                                    .on_click(move |_, _, cx| {
                                                        cancel_entity.update(
                                                            cx,
                                                            |workspace, cx| {
                                                                workspace
                                                                    .cancel_pi_custom_provider(cx);
                                                            },
                                                        );
                                                    }),
                                            )
                                            .child(
                                                Button::new("pi-save-custom-provider")
                                                    .secondary()
                                                    .small()
                                                    .label("保存")
                                                    .on_click(move |_, window, cx| {
                                                        save_entity.update(
                                                            cx,
                                                            |workspace, cx| {
                                                                workspace
                                                                    .save_pi_custom_provider(
                                                                        false, window, cx,
                                                                    );
                                                            },
                                                        );
                                                    }),
                                            )
                                            .child(
                                                Button::new("pi-save-custom-provider-default")
                                                    .small()
                                                    .label("保存并设为默认")
                                                    .on_click(move |_, window, cx| {
                                                        save_default_entity.update(
                                                            cx,
                                                            |workspace, cx| {
                                                                workspace
                                                                    .save_pi_custom_provider(
                                                                        true, window, cx,
                                                                    );
                                                            },
                                                        );
                                                    }),
                                            ),
                                    ),
                            );
                        }

                        content.into_any_element()
                    }
                })),
        )
        .group(super::pi_auth::pi_auth_group(entity, snapshot))
}
