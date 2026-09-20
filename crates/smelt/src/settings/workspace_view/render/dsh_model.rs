//! 设置页：模型 / DeepSeek Harness。

use super::*;

pub(super) fn dsh_model_page(
    entity: Entity<Workspace>,
    snapshot: &SettingsRenderSnapshot,
    cx: &App,
) -> SettingPage {
    let (fg, _muted, border, popover) = {
        let t = cx.theme();
        (t.foreground, t.muted_foreground, t.border, t.popover)
    };
    let btn_base = move |id: &'static str, label: String| {
        div()
            .id(id)
            .h(px(26.))
            .px_3()
            .flex()
            .flex_none()
            .items_center()
            .rounded_md()
            .cursor_pointer()
            .text_xs()
            .text_color(fg)
            .bg(popover)
            .border_1()
            .border_color(border)
            .child(label)
    };
    let _btn = move |id: &'static str, label: String| btn_base(id, label).hover(|s| s.bg(border));
    let _btn_hover = move |id: &'static str, label: String, hover_bg: Hsla| {
        btn_base(id, label).hover(move |s| s.bg(hover_bg))
    };

    // Native dsh owns providers, credentials and model settings. Keep the
    // removed editor out of compilation while the surrounding settings
    // layout remains stable.

    let (dsh_settings_path, dsh_model_status) = match &snapshot.dsh_settings {
        Ok(settings) => {
            let status = match &settings.default_model {
                Some(model) => {
                    let provider = model.provider.as_deref().unwrap_or("未指定提供方");
                    let model_name = model.model.as_deref().unwrap_or("未指定模型");
                    match model.reasoning_effort.as_deref() {
                        Some(reasoning_effort) => {
                            format!("{provider} / {model_name} · 推理强度 {reasoning_effort}")
                        }
                        None => format!("{provider} / {model_name}"),
                    }
                }
                None => "尚未设置默认模型（agent-default-model）".to_string(),
            };
            (settings.path.display().to_string(), status)
        }
        Err(error) => (
            smelt_core::agent_kind::dsh_settings_path()
                .map(|path| path.display().to_string())
                .unwrap_or_else(|| "~/.dsh/settings.yaml".into()),
            format!("无法读取模型设置：{error}"),
        ),
    };

    SettingPage::new("DeepSeek Harness")
        .default_open(true)
        .group(
            SettingGroup::new()
                .title("模型与凭据")
                .description("模型、凭据与插件由原生 DSH profile 管理；Smelt 直接使用同一份配置。")
                .item(SettingItem::new(
                    "模型设置文件",
                    SettingField::render(move |_, _, _| {
                        div()
                            .text_sm()
                            .child(dsh_settings_path.clone())
                            .into_any_element()
                    }),
                ))
                .item(SettingItem::new(
                    "当前默认模型",
                    SettingField::render(move |_, _, _| {
                        div()
                            .text_sm()
                            .child(dsh_model_status.clone())
                            .into_any_element()
                    }),
                ))
                .item(SettingItem::render({
                    let manage_entity = entity;
                    let model_editor = snapshot.dsh_model_editor.clone();
                    let effort_view = snapshot.dsh_effort_view.clone();
                    let model_choices_view = snapshot.dsh_model_choices_view.clone();
                    let custom_editor = snapshot.dsh_custom_provider_editor.clone();
                    let model_editor_error = snapshot.dsh_model_editor_error.clone();
                    let native_model_settings = snapshot.native_dsh_model_settings.clone();
                    let plugin_manager = snapshot.dsh_plugin_manager.clone();
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
                        let profiles = smelt_core::agent_kind::native_dsh_profiles();
                        let profile_names = smelt_core::agent_kind::dsh_profile_names();
                        let missing_profiles = profile_names
                            .into_iter()
                            .filter(|name| {
                                !profiles.iter().any(|profile| profile.workspace_dir == *name)
                            })
                            .collect::<Vec<_>>();
                        let settings_entity = manage_entity.clone();
                        let configure_model_entity = manage_entity.clone();
                        let add_custom_entity = manage_entity.clone();
                        let credentials_entity = manage_entity.clone();
                        let refresh_entity = manage_entity.clone();
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
                                                                    .child("DeepSeek"),
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
                                                                    .child("官方 Provider"),
                                                            ),
                                                    )
                                                    .child(
                                                        div()
                                                            .text_xs()
                                                            .text_color(muted)
                                                            .child(
                                                                match native_model_settings.as_ref()
                                                                {
                                                                    // 读失败时别退回兜底文案：那会把
                                                                    // 「配置」「添加」为什么没反应藏起来。
                                                                    Some(Err(error)) => {
                                                                        format!("读取失败：{error}")
                                                                    }
                                                                    Some(Ok(settings)) => {
                                                                        let key_status = if settings
                                                                            .credential_configured
                                                                        {
                                                                            "凭据已配置"
                                                                        } else {
                                                                            "缺少凭据"
                                                                        };
                                                                        if settings
                                                                            .default_model
                                                                            .provider
                                                                            == "deepseek-official"
                                                                        {
                                                                            format!(
                                                                                "{} · {}",
                                                                                settings
                                                                                    .default_model
                                                                                    .model,
                                                                                key_status
                                                                            )
                                                                        } else {
                                                                            format!(
                                                                                "{} · 当前默认为 {}",
                                                                                key_status,
                                                                                settings
                                                                                    .default_model
                                                                                    .provider
                                                                            )
                                                                        }
                                                                    }
                                                                    None => "DeepSeek 官方 API"
                                                                        .to_string(),
                                                                },
                                                            ),
                                                    ),
                                            )
                                            .child(
                                                Button::new("dsh-configure-deepseek")
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
                                                                    .edit_native_dsh_model_settings(
                                                                        window, cx,
                                                                    );
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
                                                Button::new("dsh-add-custom-provider")
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
                                                                    .edit_native_dsh_custom_provider(
                                                                        None, window, cx,
                                                                    );
                                                            },
                                                        );
                                                    }),
                                            ),
                                    )
                                    .children(
                                        native_model_settings
                                            .as_ref()
                                            .and_then(|settings| settings.as_ref().ok())
                                            .into_iter()
                                            .flat_map(|settings| settings.custom_providers.iter())
                                            .map(|provider| {
                                                let edit_entity = manage_entity.clone();
                                                let provider_id = provider.id.clone();
                                                let is_default =
                                                    native_model_settings
                                                        .as_ref()
                                                        .and_then(|settings| {
                                                            settings.as_ref().ok()
                                                        })
                                                        .is_some_and(|settings| {
                                                            settings.default_model.provider
                                                                == provider.id
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
                                                        Button::new(format!(
                                                            "dsh-edit-custom-provider-{}",
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
                                                                    workspace.edit_native_dsh_custom_provider(
                                                                        Some(provider_id),
                                                                        window,
                                                                        cx,
                                                                    );
                                                                },
                                                            );
                                                        }),
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
                                        Button::new("dsh-edit-model-settings")
                                            .ghost()
                                            .small()
                                            .label("编辑原生设置文件")
                                            .on_click(move |_, window, cx| {
                                                settings_entity.update(cx, |workspace, cx| {
                                                    workspace
                                                        .open_native_dsh_config(false, window, cx);
                                                });
                                            }),
                                    )
                                    .child(
                                        Button::new("dsh-edit-credentials-file")
                                            .ghost()
                                            .small()
                                            .label("编辑原生凭据文件")
                                            .on_click(move |_, window, cx| {
                                                credentials_entity.update(cx, |workspace, cx| {
                                                    workspace
                                                        .open_native_dsh_config(true, window, cx);
                                                });
                                            }),
                                    )
                                    .child(
                                        Button::new("dsh-refresh-settings")
                                            .ghost()
                                            .small()
                                            .label("刷新")
                                            .on_click(move |_, _, cx| {
                                                refresh_entity.update(cx, |workspace, cx| {
                                                    workspace.refresh_native_dsh_settings(cx);
                                                });
                                            }),
                                    ),
                            );
                        if let Some(error) = &model_editor_error {
                            content = content.child(
                                div()
                                    .text_xs()
                                    .text_color(danger_fg)
                                    .child(format!("无法打开交互式配置：{error}")),
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
                            let reasoning_effort = editor.reasoning_effort.clone();
                            let effort_view = effort_view.clone();
                            let model_choices_view = model_choices_view.clone();
                            let model_input = editor.model.clone();
                            let current_model = editor.model.read(cx).value().trim().to_string();
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
                                                    .child("官方 DeepSeek 配置"),
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
                                                            .child("默认模型"),
                                                    )
                                                    .child(Input::new(&editor.model)),
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
                                                            .child("API 地址"),
                                                    )
                                                    .child(Input::new(&editor.base_url)),
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
                                                div().text_xs().text_color(muted).child(format!(
                                                    "凭据存放于 {}",
                                                    editor.configured_api_key_env
                                                )),
                                            ),
                                    )
                                    .children(model_choices_view.map(|view| {
                                        render_dsh_model_choices_section(
                                            view,
                                            current_model.clone(),
                                            model_input.clone(),
                                            fg,
                                            muted,
                                        )
                                    }))
                                    // 档位来自助手对这条路由的广告，不是写死的
                                    // 四个按钮：同一个模型经不同连接（直连 vs
                                    // 中转）可选档位并不一样，写死就会给出一个
                                    // 发一次失败一次的选项。
                                    //
                                    // 抽成函数而不是就地展开：这条渲染链已经长到
                                    // 让 rustc 在解析期爆栈，往里再嵌一层 match
                                    // 就编译不过了。
                                    .children(effort_view.map(|view| {
                                        render_dsh_effort_section(
                                            view,
                                            reasoning_effort.clone(),
                                            manage_entity.clone(),
                                            fg,
                                            muted,
                                            danger_fg,
                                        )
                                    }))
                                    .child(
                                        h_flex()
                                            .w_full()
                                            .gap_2()
                                            .justify_end()
                                            .child(
                                                Button::new("dsh-cancel-model-settings")
                                                    .ghost()
                                                    .small()
                                                    .label("取消")
                                                    .on_click(move |_, _, cx| {
                                                        cancel_entity.update(cx, |workspace, cx| {
                                                            workspace
                                                                .cancel_native_dsh_model_edit(cx);
                                                        });
                                                    }),
                                            )
                                            .child(
                                                Button::new("dsh-save-model-settings")
                                                    .small()
                                                    .label("保存配置")
                                                    .on_click(move |_, window, cx| {
                                                        save_entity.update(cx, |workspace, cx| {
                                                            workspace
                                                                .save_native_dsh_model_settings(
                                                                    window, cx,
                                                                );
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
                            // 引用名不给人填，所以得说清楚这把 key 会落到哪儿——凭据
                            // 文件是用户自己也会打开看的。
                            let credential_ref = Workspace::custom_provider_credential_ref(
                                &editor.configured_api_key_env,
                                editor.id.read(cx).value().trim(),
                                true,
                            );
                            let credential_ref_hint = if credential_ref.is_empty() {
                                "凭据引用名会按 Provider ID 自动生成".to_string()
                            } else if editor.configured_api_key_env.is_empty() {
                                format!("凭据将存放于 {credential_ref}")
                            } else {
                                format!("凭据存放于 {credential_ref}")
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
                                                    Button::new(format!(
                                                        "dsh-remove-custom-model-{index}"
                                                    ))
                                                    .ghost()
                                                    .small()
                                                    .icon(IconName::Delete)
                                                    .tooltip("移除此模型")
                                                    .on_click(move |_, _, cx| {
                                                        remove_entity.update(
                                                            cx,
                                                            |workspace, cx| {
                                                                workspace
                                                                    .remove_native_dsh_custom_model(
                                                                        index, cx,
                                                                    );
                                                            },
                                                        );
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
                                                                .child("上下文长度"),
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
                                                                .child("最大输出"),
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
                                                            .child("原生 DSH"),
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
                                                    .child(Input::new(&editor.id)),
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
                                                            .child("显示名称"),
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
                                                        Button::new(
                                                            "dsh-custom-provider-api",
                                                        )
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
                                                                                workspace.set_native_dsh_custom_api(api, cx);
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
                                                    .child(credential_ref_hint),
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
                                                        Button::new("dsh-discover-custom-models")
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
                                                                            .discover_native_dsh_custom_models(cx);
                                                                    },
                                                                );
                                                            }),
                                                    )
                                                    .child(
                                                        Button::new("dsh-add-custom-model")
                                                            .secondary()
                                                            .small()
                                                            .icon(IconName::Plus)
                                                            .label("添加模型")
                                                            .on_click(move |_, window, cx| {
                                                                add_model_entity.update(
                                                                    cx,
                                                                    |workspace, cx| {
                                                                        workspace
                                                                            .add_native_dsh_custom_model(
                                                                                window, cx,
                                                                            );
                                                                    },
                                                                );
                                                            }),
                                                    ),
                                            ),
                                    )
                                    .child(render_model_discovery_section(
                                        "dsh",
                                        discovery_state,
                                        adopted_models,
                                        manual_rows,
                                        manage_entity.clone(),
                                        Workspace::adopt_native_dsh_discovered_model,
                                        muted,
                                        danger_fg,
                                    ))
                                    .child(model_rows)
                                    .child(
                                        h_flex()
                                            .w_full()
                                            .gap_2()
                                            .justify_end()
                                            .child(
                                                Button::new("dsh-cancel-custom-provider")
                                                    .ghost()
                                                    .small()
                                                    .label("取消")
                                                    .on_click(move |_, _, cx| {
                                                        cancel_entity.update(
                                                            cx,
                                                            |workspace, cx| {
                                                                workspace
                                                                    .cancel_native_dsh_custom_provider(
                                                                        cx,
                                                                    );
                                                            },
                                                        );
                                                    }),
                                            )
                                            .child(
                                                Button::new("dsh-save-custom-provider")
                                                    .secondary()
                                                    .small()
                                                    .label("保存")
                                                    .on_click(move |_, window, cx| {
                                                        save_entity.update(
                                                            cx,
                                                            |workspace, cx| {
                                                                workspace
                                                                    .save_native_dsh_custom_provider(
                                                                        false, window, cx,
                                                                    );
                                                            },
                                                        );
                                                    }),
                                            )
                                            .child(
                                                Button::new("dsh-save-custom-provider-default")
                                                    .small()
                                                    .label("保存并设为默认")
                                                    .on_click(move |_, window, cx| {
                                                        save_default_entity.update(
                                                            cx,
                                                            |workspace, cx| {
                                                                workspace
                                                                    .save_native_dsh_custom_provider(
                                                                        true, window, cx,
                                                                    );
                                                            },
                                                        );
                                                    }),
                                            ),
                                    ),
                            );
                        }
                        if profiles.is_empty() && missing_profiles.is_empty() {
                            content = content.child(
                                v_flex()
                                    .w_full()
                                    .gap_2()
                                    .rounded_md()
                                    .border_1()
                                    .border_color(border)
                                    .p_3()
                                    .child(
                                        div()
                                            .text_sm()
                                            .text_color(fg)
                                            .child("尚未发现原生 DSH profile"),
                                    )
                                    .child(div().text_xs().text_color(muted).child(
                                        "请先用 DSH 创建 profile。已有 profile 后，在这里安装 Smelt Host bundle。",
                                    )),
                            );
                        } else {
                            for profile in profiles {
                                let profile_name = profile.workspace_dir.clone();
                                let toggle_entity = manage_entity.clone();
                                let open = plugin_manager
                                    .as_ref()
                                    .filter(|manager| manager.profile == profile_name)
                                    .cloned();
                                let toggle_profile = profile_name.clone();
                                let mut card = v_flex()
                                    .w_full()
                                    .gap_3()
                                    .rounded_md()
                                    .border_1()
                                    .border_color(border)
                                    .p_3()
                                    .child(
                                        h_flex()
                                            .w_full()
                                            .items_center()
                                            .justify_between()
                                            .gap_3()
                                            .child(
                                                v_flex()
                                                    .min_w_0()
                                                    .child(
                                                        div()
                                                            .text_sm()
                                                            .text_color(fg)
                                                            .child(profile.label),
                                                    )
                                                    .child(
                                                        div()
                                                            .text_xs()
                                                            .text_color(muted)
                                                            .child(format!(
                                                                "profile 目录：{profile_name}"
                                                            )),
                                                    ),
                                            )
                                            .child(
                                                Button::new(format!(
                                                    "dsh-manage-plugins-{profile_name}"
                                                ))
                                                .secondary()
                                                .small()
                                                .label(if open.is_some() {
                                                    "收起插件"
                                                } else {
                                                    "管理插件"
                                                })
                                                .on_click(move |_, window, cx| {
                                                    let profile = toggle_profile.clone();
                                                    toggle_entity.update(cx, |workspace, cx| {
                                                        workspace
                                                            .toggle_native_dsh_plugin_manager(
                                                                profile, window, cx,
                                                            );
                                                    });
                                                }),
                                            ),
                                    );

                                if let Some(manager) = open {
                                    let busy = manager.busy.clone();
                                    let mut panel = v_flex().w_full().gap_2();

                                    match &manager.plugins {
                                        Some(Ok(plugins)) if plugins.is_empty() => {
                                            panel = panel.child(
                                                div()
                                                    .text_xs()
                                                    .text_color(muted)
                                                    .child("该 profile 还没有任何插件。"),
                                            );
                                        }
                                        Some(Ok(plugins)) => {
                                            for plugin in plugins {
                                                let version = plugin
                                                    .installed
                                                    .clone()
                                                    .map(|version| format!("v{version}"))
                                                    .or_else(|| plugin.requested.clone())
                                                    .unwrap_or_else(|| "—".to_string());
                                                let detail = plugin
                                                    .description
                                                    .clone()
                                                    .unwrap_or_else(|| plugin.name.clone());
                                                let healthy = plugin.is_active();
                                                let mut row = h_flex()
                                                    .w_full()
                                                    .items_center()
                                                    .justify_between()
                                                    .gap_2()
                                                    .rounded_md()
                                                    .border_1()
                                                    .border_color(border)
                                                    .px_2()
                                                    .py_1p5()
                                                    .child(
                                                        v_flex()
                                                            .min_w_0()
                                                            .flex_1()
                                                            .child(
                                                                h_flex()
                                                                    .gap_2()
                                                                    .items_center()
                                                                    .child(
                                                                        div()
                                                                            .text_sm()
                                                                            .text_color(fg)
                                                                            .child(
                                                                                plugin.name.clone(),
                                                                            ),
                                                                    )
                                                                    .child(
                                                                        div()
                                                                            .text_xs()
                                                                            .text_color(muted)
                                                                            .child(version),
                                                                    ),
                                                            )
                                                            .child(
                                                                div()
                                                                    .text_xs()
                                                                    .text_color(if healthy {
                                                                        muted
                                                                    } else {
                                                                        danger_fg
                                                                    })
                                                                    .child(format!(
                                                                        "{} · {detail}",
                                                                        plugin.status()
                                                                    )),
                                                            ),
                                                    );

                                                // dsh 自带的 bundle 不归 profile 管，连更新入口都不该给。
                                                if !plugin.builtin {
                                                    let update_entity = manage_entity.clone();
                                                    let update_name = plugin.name.clone();
                                                    row = row
                                                        .child(
                                                            Button::new(format!(
                                                                "dsh-plugin-update-{profile_name}-{}",
                                                                plugin.name
                                                            ))
                                                            .ghost()
                                                            .small()
                                                            .label("更新")
                                                            .disabled(busy.is_some())
                                                            .on_click(move |_, window, cx| {
                                                                let name = update_name.clone();
                                                                update_entity.update(
                                                                    cx,
                                                                    |workspace, cx| {
                                                                        workspace
                                                                        .reinstall_native_dsh_plugin(
                                                                            name, window, cx,
                                                                        );
                                                                    },
                                                                );
                                                            }),
                                                        );
                                                }

                                                // Smelt Host 只给更新不给卸载：它是本 profile 跟
                                                // Smelt 之间唯一的 ACP 通道，删掉会话就起不来了。
                                                if plugin.is_removable() {
                                                    let remove_entity = manage_entity.clone();
                                                    let remove_name = plugin.name.clone();
                                                    row = row.child(
                                                        Button::new(format!(
                                                            "dsh-plugin-remove-{profile_name}-{}",
                                                            plugin.name
                                                        ))
                                                        .ghost()
                                                        .small()
                                                        .danger()
                                                        .label("卸载")
                                                        .disabled(busy.is_some())
                                                        .on_click(move |_, window, cx| {
                                                            let name = remove_name.clone();
                                                            remove_entity.update(
                                                                cx,
                                                                |workspace, cx| {
                                                                    workspace
                                                                        .remove_native_dsh_plugin(
                                                                            name, window, cx,
                                                                        );
                                                                },
                                                            );
                                                        }),
                                                    );
                                                }
                                                panel = panel.child(row);
                                            }
                                        }
                                        Some(Err(error)) => {
                                            panel = panel.child(
                                                div()
                                                    .text_xs()
                                                    .text_color(danger_fg)
                                                    .child(format!("读取插件列表失败：{error}")),
                                            );
                                        }
                                        None => {}
                                    }

                                    let install_entity = manage_entity.clone();
                                    let refresh_entity = manage_entity.clone();
                                    panel = panel.child(
                                        h_flex()
                                            .w_full()
                                            .gap_2()
                                            .items_center()
                                            .child(
                                                div()
                                                    .flex_1()
                                                    .min_w(px(200.))
                                                    .child(Input::new(&manager.spec)),
                                            )
                                            .child(
                                                Button::new(format!(
                                                    "dsh-plugin-install-{profile_name}"
                                                ))
                                                .small()
                                                .label("安装")
                                                .loading(busy.is_some())
                                                .disabled(busy.is_some())
                                                .on_click(move |_, window, cx| {
                                                    install_entity.update(cx, |workspace, cx| {
                                                        workspace
                                                            .install_native_dsh_plugin(window, cx);
                                                    });
                                                }),
                                            )
                                            .child(
                                                Button::new(format!(
                                                    "dsh-plugin-refresh-{profile_name}"
                                                ))
                                                .ghost()
                                                .small()
                                                .label("刷新")
                                                .disabled(busy.is_some())
                                                .on_click(move |_, _, cx| {
                                                    refresh_entity.update(cx, |workspace, cx| {
                                                        workspace.reload_native_dsh_plugins(cx);
                                                    });
                                                }),
                                            ),
                                    );

                                    if let Some(busy) = &busy {
                                        panel = panel.child(
                                            div()
                                                .text_xs()
                                                .text_color(muted)
                                                .child(busy.clone()),
                                        );
                                    } else if let Some((message, ok)) = &manager.message {
                                        panel = panel.child(
                                            div()
                                                .text_xs()
                                                .text_color(if *ok { muted } else { danger_fg })
                                                .child(message.clone()),
                                        );
                                    }

                                    card = card.child(panel);
                                }

                                content = content.child(card);
                            }
                            for profile_name in missing_profiles {
                                let install_entity = manage_entity.clone();
                                let install_hint = if smelt_core::agent_kind::dsh_bridge_entry(
                                    &profile_name,
                                )
                                .is_some()
                                {
                                    "检测到旧版或未注册的 Smelt Host；安装会升级并注册它。"
                                } else {
                                    "尚未安装 Smelt Host bundle"
                                };
                                let pending = plugin_manager
                                    .as_ref()
                                    .filter(|manager| manager.profile == profile_name)
                                    .cloned();
                                let busy = pending
                                    .as_ref()
                                    .and_then(|manager| manager.busy.clone());
                                let result = pending
                                    .as_ref()
                                    .and_then(|manager| manager.message.clone());
                                let install_profile = profile_name.clone();
                                let mut card = v_flex()
                                    .w_full()
                                    .gap_2()
                                    .rounded_md()
                                    .border_1()
                                    .border_color(border)
                                    .p_3()
                                    .child(
                                        h_flex()
                                            .w_full()
                                            .items_center()
                                            .justify_between()
                                            .gap_3()
                                            .child(
                                                v_flex()
                                                    .min_w_0()
                                                    .child(
                                                        div()
                                                            .text_sm()
                                                            .text_color(fg)
                                                            .child(format!(
                                                                "DeepSeek Harness · {profile_name}"
                                                            )),
                                                    )
                                                    .child(
                                                        div()
                                                            .text_xs()
                                                            .text_color(muted)
                                                            .child(install_hint),
                                                    ),
                                            )
                                            .child(
                                                Button::new(format!(
                                                    "dsh-install-smelt-host-{profile_name}"
                                                ))
                                                .small()
                                                .label("安装/更新 Smelt Host")
                                                .loading(busy.is_some())
                                                .disabled(busy.is_some())
                                                .on_click(move |_, window, cx| {
                                                    let profile = install_profile.clone();
                                                    install_entity.update(cx, |workspace, cx| {
                                                        workspace.install_native_dsh_smelt_host(
                                                            profile, window, cx,
                                                        );
                                                    });
                                                }),
                                            ),
                                    );
                                if let Some(busy) = busy {
                                    card = card.child(
                                        div().text_xs().text_color(muted).child(busy),
                                    );
                                } else if let Some((message, ok)) = result {
                                    card = card.child(
                                        div()
                                            .text_xs()
                                            .text_color(if ok { muted } else { danger_fg })
                                            .child(message),
                                    );
                                }
                                card = card.child(
                                    div().text_xs().text_color(muted).child(
                                        "安装在应用内完成，无需终端。DSH 插件管理需要 pnpm（npm install -g pnpm）；安装完成后请重启 Smelt。",
                                    ),
                                );
                                content = content.child(card);
                            }
                        }
                        content.into_any_element()
                    }
                })),
        )
}
