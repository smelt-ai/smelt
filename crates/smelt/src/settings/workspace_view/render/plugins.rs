//! 设置页：bundled 插件。每个插件一页，开关在第一项，配置跟在后面。

use super::*;

pub(super) fn plugin_page(
    entity: Entity<Workspace>,
    _snapshot: &SettingsRenderSnapshot,
    plugin_id: &str,
    catalog: &[InstalledPlugin],
    cx: &App,
) -> SettingPage {
    let state = cx
        .try_global::<PluginEnablementState>()
        .cloned()
        .unwrap_or_default();
    let plugin = catalog
        .iter()
        .find(|plugin| plugin.id == plugin_id)
        .cloned()
        .or_else(|| {
            state
                .catalog
                .iter()
                .find(|plugin| plugin.id == plugin_id)
                .cloned()
        });
    let Some(plugin) = plugin else {
        return empty_plugins_page(entity, cx);
    };

    let enabled = state.is_enabled(&plugin.id);
    let id = plugin.id.clone();
    let id_for_set = id.clone();
    let mut items = vec![
        SettingItem::new(
            "启用",
            SettingField::switch(
                move |cx: &App| {
                    cx.try_global::<PluginEnablementState>()
                        .map(|state| state.is_enabled(&id))
                        .unwrap_or_else(|| {
                            smelt_core::plugin_enablement::PluginEnablement::default()
                                .is_enabled(&id)
                        })
                },
                move |on: bool, cx: &mut App| {
                    apply_plugin_enabled(&id_for_set, on, cx);
                },
            ),
        )
        .description("关闭后停止插件进程，并隐藏下面的配置。"),
        SettingItem::new(
            "运行状态",
            SettingField::render({
                let id = plugin.id.clone();
                let load_error = plugin.load_error.clone();
                move |_, _, cx: &mut App| {
                    if let Some(error) = &load_error {
                        return div()
                            .text_xs()
                            .text_color(cx.theme().danger)
                            .child(format!("拒绝加载：{error}"))
                            .into_any_element();
                    }
                    refresh_plugin_statuses_throttled(cx);
                    let snapshot = cx.try_global::<PluginRuntimeStatuses>().cloned();
                    let (text, dim) = match snapshot {
                        // 连不上守护本身也要说出来，否则和"插件没起来"分不清。
                        Some(snapshot) => match snapshot.error {
                            Some(error) => (format!("无法查询：{error}"), true),
                            None => (
                                describe_plugin_status(snapshot.by_id.get(&id)),
                                !matches!(
                                    snapshot.by_id.get(&id).map(|status| &status.state),
                                    Some(smelt_plugin_host::PluginLifecycleState::Ready { .. })
                                ),
                            ),
                        },
                        None => ("查询中…".to_string(), true),
                    };
                    div()
                        .text_xs()
                        .text_color(if dim {
                            cx.theme().muted_foreground
                        } else {
                            cx.theme().foreground
                        })
                        .child(text)
                        .into_any_element()
                }
            }),
        )
        .description("插件进程在守护里的实际状态。起不来时这里会说明原因。"),
    ];
    if plugin.user_installed {
        let uninstall_entity = entity;
        let plugin_id = plugin.id.clone();
        let plugin_name = plugin.name.clone();
        items.push(
            SettingItem::new(
                "卸载",
                SettingField::render(move |_, _, _| {
                    let entity = uninstall_entity.clone();
                    let id = plugin_id.clone();
                    let name = plugin_name.clone();
                    Button::new(format!("uninstall-plugin-{id}"))
                        .danger()
                        .small()
                        .label("卸载插件")
                        .on_click(move |_, window, cx| {
                            entity.update(cx, |workspace, cx| {
                                workspace.uninstall_user_plugin(
                                    id.clone(),
                                    name.clone(),
                                    window,
                                    cx,
                                );
                            });
                        })
                        .into_any_element()
                }),
            )
            .description("删除 ~/.smelt/plugins 下的整包并停止对应进程。"),
        );
    }
    if enabled {
        crate::plugin_ui::refresh_presentations(cx);
        items.extend(plugin_contribution_items(&plugin.id, cx));
    }

    SettingPage::new(plugin.name.clone())
        .description(format!(
            "{} · v{} · {}。关闭后不会启动对应进程，其配置也会隐藏。",
            plugin.id,
            plugin.version,
            if plugin.user_installed {
                "用户安装"
            } else {
                "随应用分发"
            }
        ))
        .group(SettingGroup::new().items(items))
}

fn plugin_action_button(
    plugin_id: String,
    section_id: String,
    action: smelt_plugin_api::SettingsActionView,
    cx: &App,
) -> Button {
    let pending =
        crate::plugin_ui::settings_action_pending(cx, &plugin_id, &section_id, action.id.as_str());
    let mut button = Button::new(format!(
        "plugin-setting-action-{plugin_id}-{section_id}-{}",
        action.id
    ))
    .small()
    .max_w(px(180.))
    .overflow_hidden()
    .label(action.label.clone())
    .tooltip(action.label.clone())
    .loading(pending)
    .disabled(action.disabled || pending);
    button = match action.style {
        smelt_plugin_api::SettingsActionStyle::Primary => button.primary(),
        smelt_plugin_api::SettingsActionStyle::Danger => button.danger(),
        smelt_plugin_api::SettingsActionStyle::Default => button,
    };
    let action_for_click = action;
    button.on_click(move |_, _, cx| {
        crate::plugin_ui::run_settings_action(
            plugin_id.clone(),
            section_id.clone(),
            action_for_click.clone(),
            None,
            cx,
        );
    })
}

fn plugin_tone_color(tone: smelt_plugin_api::SettingsTone, cx: &App) -> Hsla {
    match tone {
        smelt_plugin_api::SettingsTone::Neutral => cx.theme().muted_foreground,
        smelt_plugin_api::SettingsTone::Positive => rgb(crate::ui_theme::green()).into(),
        smelt_plugin_api::SettingsTone::Warning => cx.theme().warning,
        smelt_plugin_api::SettingsTone::Negative => cx.theme().danger,
    }
}

fn plugin_account_item(
    plugin_id: &str,
    section_id: &str,
    item: &smelt_plugin_api::SettingsItemView,
) -> SettingItem {
    let smelt_plugin_api::SettingsItemView::Account {
        id,
        title,
        signed_in,
        display_name,
        detail,
        message,
        tone,
        avatar,
        actions,
    } = item
    else {
        unreachable!("plugin_account_item only accepts account items");
    };
    let plugin_id = plugin_id.to_string();
    let section_id = section_id.to_string();
    let item_id = id.as_str().to_string();
    let title = title.clone();
    let signed_in = *signed_in;
    let display_name = display_name.clone();
    let detail = detail.clone();
    let message = message.clone();
    let tone = *tone;
    let avatar = avatar.clone();
    let actions = actions.clone();
    let keyword_title = title.clone();
    SettingItem::render(move |_, _, cx: &mut App| {
        let foreground = cx.theme().foreground;
        let muted = cx.theme().muted_foreground;
        let tone_color = plugin_tone_color(tone, cx);
        let image = avatar
            .as_ref()
            .and_then(|avatar| crate::plugin_ui::settings_avatar(cx, &plugin_id, avatar));
        let identity = if signed_in {
            display_name.clone()
        } else {
            "未登录".to_string()
        };
        let initial = display_name.chars().next().unwrap_or('?').to_string();
        let avatar_element = image
            .map(|image| img(image).size(px(28.)).rounded_full().into_any_element())
            .unwrap_or_else(|| {
                div()
                    .size(px(28.))
                    .rounded_full()
                    .bg(crate::ui_theme::tint(crate::ui_theme::blue(), 0x30))
                    .flex()
                    .items_center()
                    .justify_center()
                    .text_xs()
                    .font_semibold()
                    .text_color(rgb(crate::ui_theme::blue()))
                    .child(initial)
                    .into_any_element()
            });
        let mut row = h_flex()
            .w_full()
            .items_center()
            .gap_3()
            .flex_wrap()
            .child(avatar_element)
            .child(
                v_flex()
                    .flex_1()
                    .min_w_0()
                    .gap_0p5()
                    .child(
                        div()
                            .text_sm()
                            .font_medium()
                            .text_color(foreground)
                            .child(identity),
                    )
                    .when_some(detail.clone(), |column, detail| {
                        column.child(div().text_xs().text_color(muted).child(detail))
                    }),
            );
        for action in actions.clone() {
            row = row.child(plugin_action_button(
                plugin_id.clone(),
                section_id.clone(),
                action,
                cx,
            ));
        }
        v_flex()
            .id(format!(
                "plugin-setting-account-{plugin_id}-{section_id}-{item_id}"
            ))
            .w_full()
            .gap_2()
            .child(
                div()
                    .text_xs()
                    .font_semibold()
                    .text_color(muted)
                    .child(title.clone()),
            )
            .child(row)
            .when_some(message.clone(), |column, message| {
                column.child(div().text_xs().text_color(tone_color).child(message))
            })
    })
    .keywords([keyword_title])
}

fn plugin_status_item(
    plugin_id: &str,
    section_id: &str,
    item: &smelt_plugin_api::SettingsItemView,
) -> SettingItem {
    let smelt_plugin_api::SettingsItemView::Status {
        id,
        title,
        text,
        tone,
        description,
        actions,
    } = item
    else {
        unreachable!("plugin_status_item only accepts status items");
    };
    let plugin_id = plugin_id.to_string();
    let section_id = section_id.to_string();
    let field_id = id.as_str().to_string();
    let text = text.clone();
    let tone = *tone;
    let actions = actions.clone();
    let mut setting = SettingItem::new(
        title.clone(),
        SettingField::render(move |_, _, cx: &mut App| {
            let color = plugin_tone_color(tone, cx);
            let mut row = h_flex()
                .id(format!(
                    "plugin-setting-status-{plugin_id}-{section_id}-{field_id}"
                ))
                .items_center()
                .justify_end()
                .gap_2()
                .flex_wrap()
                .child(div().size(px(6.)).rounded_full().bg(color))
                .child(div().text_xs().text_color(color).child(text.clone()));
            for action in actions.clone() {
                row = row.child(plugin_action_button(
                    plugin_id.clone(),
                    section_id.clone(),
                    action,
                    cx,
                ));
            }
            row.into_any_element()
        }),
    );
    if let Some(description) = description {
        setting = setting.description(description.clone());
    }
    setting
}

fn plugin_text_item(item: &smelt_plugin_api::SettingsItemView) -> SettingItem {
    let smelt_plugin_api::SettingsItemView::Text {
        id,
        title,
        value,
        description,
        copyable,
    } = item
    else {
        unreachable!("plugin_text_item only accepts text items");
    };
    let field_id = id.as_str().to_string();
    let value = value.clone();
    let copyable = *copyable;
    let mut setting = SettingItem::new(
        title.clone(),
        SettingField::render(move |_, _window, cx: &mut App| {
            let copy_value = value.clone();
            h_flex()
                .items_center()
                .justify_end()
                .gap_2()
                .child(
                    div()
                        .max_w(px(360.))
                        .overflow_hidden()
                        .whitespace_nowrap()
                        .text_ellipsis_middle()
                        .text_xs()
                        .text_color(cx.theme().muted_foreground)
                        .child(value.clone()),
                )
                .when(copyable, |row| {
                    row.child(
                        Button::new(format!("copy-plugin-setting-{field_id}"))
                            .small()
                            .ghost()
                            .icon(IconName::Copy)
                            .tooltip("复制")
                            .on_click(move |_, _window, cx| {
                                cx.write_to_clipboard(ClipboardItem::new_string(
                                    copy_value.clone(),
                                ));
                            }),
                    )
                })
                .into_any_element()
        }),
    );
    if let Some(description) = description {
        setting = setting.description(description.clone());
    }
    setting
}

fn plugin_toggle_item(
    plugin_id: &str,
    section_id: &str,
    item: &smelt_plugin_api::SettingsItemView,
) -> SettingItem {
    let smelt_plugin_api::SettingsItemView::Toggle {
        id,
        title,
        value,
        description,
        action,
        disabled,
    } = item
    else {
        unreachable!("plugin_toggle_item only accepts toggle items");
    };
    let plugin_id = plugin_id.to_string();
    let section_id = section_id.to_string();
    let field_id = id.as_str().to_string();
    let action = action.clone();
    let action_id = action.id.as_str().to_string();
    let value = *value;
    let disabled = *disabled || action.disabled;
    let mut setting = SettingItem::new(
        title.clone(),
        SettingField::render(move |_, _, cx: &mut App| {
            let pending =
                crate::plugin_ui::settings_action_pending(cx, &plugin_id, &section_id, &action_id);
            let action_for_click = action.clone();
            let plugin_for_click = plugin_id.clone();
            let section_for_click = section_id.clone();
            Switch::new(format!(
                "plugin-setting-toggle-{plugin_id}-{section_id}-{field_id}"
            ))
            .checked(value)
            .disabled(disabled || pending)
            .on_click(move |checked, _, cx| {
                crate::plugin_ui::run_settings_action(
                    plugin_for_click.clone(),
                    section_for_click.clone(),
                    action_for_click.clone(),
                    Some(*checked),
                    cx,
                );
            })
            .into_any_element()
        }),
    );
    if let Some(description) = description {
        setting = setting.description(description.clone());
    }
    setting
}

fn plugin_actions_item(
    plugin_id: &str,
    section_id: &str,
    item: &smelt_plugin_api::SettingsItemView,
) -> SettingItem {
    let smelt_plugin_api::SettingsItemView::Actions {
        id,
        title,
        description,
        actions,
    } = item
    else {
        unreachable!("plugin_actions_item only accepts action items");
    };
    let plugin_id = plugin_id.to_string();
    let section_id = section_id.to_string();
    let field_id = id.as_str().to_string();
    let actions = actions.clone();
    let mut setting = SettingItem::new(
        title.clone().unwrap_or_else(|| "操作".to_string()),
        SettingField::render(move |_, _, cx: &mut App| {
            let mut row = h_flex()
                .id(format!(
                    "plugin-setting-actions-{plugin_id}-{section_id}-{field_id}"
                ))
                .items_center()
                .justify_end()
                .gap_2()
                .flex_wrap();
            for action in actions.clone() {
                row = row.child(plugin_action_button(
                    plugin_id.clone(),
                    section_id.clone(),
                    action,
                    cx,
                ));
            }
            row.into_any_element()
        }),
    );
    if let Some(description) = description {
        setting = setting.description(description.clone());
    }
    setting
}

fn plugin_contribution_items(plugin_id: &str, cx: &App) -> Vec<SettingItem> {
    let mut items = Vec::new();
    for section in crate::plugin_ui::settings_sections(plugin_id) {
        let snapshot = crate::plugin_ui::settings_section_snapshot(
            cx,
            &section.plugin_id,
            &section.contribution_id,
        );
        let title = section.title.clone();
        let description = section.description.clone();
        items.push(
            SettingItem::render(move |_, _, cx: &mut App| {
                v_flex()
                    .w_full()
                    .gap_1()
                    .child(
                        div()
                            .text_sm()
                            .font_semibold()
                            .text_color(cx.theme().foreground)
                            .child(title.clone()),
                    )
                    .when_some(description.clone(), |column, description| {
                        column.child(
                            div()
                                .text_xs()
                                .text_color(cx.theme().muted_foreground)
                                .child(description),
                        )
                    })
            })
            .keywords([section.title.clone()]),
        );
        if let Some(view) = snapshot.view {
            for item in &view.items {
                match item {
                    smelt_plugin_api::SettingsItemView::Header { title, .. } => {
                        let title = title.clone();
                        let keyword_title = title.clone();
                        items.push(
                            SettingItem::render(move |_, _, cx: &mut App| {
                                div()
                                    .w_full()
                                    .pt_3()
                                    .mt_1()
                                    .border_t_1()
                                    .border_color(cx.theme().border)
                                    .text_xs()
                                    .font_semibold()
                                    .text_color(cx.theme().muted_foreground)
                                    .child(title.clone())
                            })
                            .keywords([keyword_title]),
                        );
                    }
                    smelt_plugin_api::SettingsItemView::Account { .. } => items.push(
                        plugin_account_item(&section.plugin_id, &section.contribution_id, item),
                    ),
                    smelt_plugin_api::SettingsItemView::Status { .. } => items.push(
                        plugin_status_item(&section.plugin_id, &section.contribution_id, item),
                    ),
                    smelt_plugin_api::SettingsItemView::Text { .. } => {
                        items.push(plugin_text_item(item));
                    }
                    smelt_plugin_api::SettingsItemView::Toggle { .. } => items.push(
                        plugin_toggle_item(&section.plugin_id, &section.contribution_id, item),
                    ),
                    smelt_plugin_api::SettingsItemView::Actions { .. } => items.push(
                        plugin_actions_item(&section.plugin_id, &section.contribution_id, item),
                    ),
                }
            }
        } else {
            items.push(SettingItem::render(move |_, _, cx: &mut App| {
                div()
                    .text_xs()
                    .text_color(cx.theme().muted_foreground)
                    .child("正在读取插件设置...")
            }));
        }
        if let Some(error) = snapshot.error {
            items.push(SettingItem::render(move |_, _, cx: &mut App| {
                div()
                    .text_xs()
                    .text_color(cx.theme().danger)
                    .child(format!("插件设置不可用：{error}"))
            }));
        }
        if let Some(error) = snapshot.action_error {
            items.push(SettingItem::render(move |_, _, cx: &mut App| {
                div()
                    .text_xs()
                    .text_color(cx.theme().danger)
                    .child(format!("操作失败：{error}"))
            }));
        }
    }
    items
}

fn empty_plugins_page(entity: Entity<Workspace>, cx: &App) -> SettingPage {
    let state = cx
        .try_global::<PluginEnablementState>()
        .cloned()
        .unwrap_or_default();
    let mut items = vec![
        SettingItem::new(
            "安装插件",
            SettingField::render({
                let entity = entity.clone();
                move |_, _, _| {
                    let entity = entity.clone();
                    Button::new("install-user-plugin")
                        .primary()
                        .small()
                        .icon(IconName::Plus)
                        .label("选择插件包目录…")
                        .on_click(move |_, window, cx| {
                            entity.update(cx, |workspace, cx| {
                                workspace.install_user_plugin(window, cx);
                            });
                        })
                        .into_any_element()
                }
            }),
        )
        .description("选择一个内含 plugin.json 的完整 package 目录。"),
    ];
    let pending = state.pending_install;
    if let Some(plan) = pending {
        let mut detail = format!("{} v{}\n{}", plan.name, plan.version, plan.id);
        if let Some(previous) = &plan.previous_version {
            detail.push_str(&format!("\n更新：v{previous} → v{}", plan.version));
        } else {
            detail.push_str("\n首次安装：你将允许这份第三方代码在本机运行");
        }
        if plan.requested_capabilities.is_empty() {
            detail.push_str("\n\n请求的 Smelt 权限：无");
        } else {
            detail.push_str("\n\n请求的 Smelt 权限：");
            for capability in &plan.requested_capabilities {
                let (name, description) = PluginEnablementState::describe_capability(capability);
                detail.push_str(&format!("\n• {name}（{capability}）：{description}"));
            }
        }
        if !plan.added_capabilities.is_empty() && plan.previous_version.is_some() {
            detail.push_str(&format!(
                "\n\n新增权限：{}",
                plan.added_capabilities.join("、")
            ));
        }
        if !plan.removed_capabilities.is_empty() {
            detail.push_str(&format!(
                "\n已移除权限：{}",
                plan.removed_capabilities.join("、")
            ));
        }
        // 权限清单只覆盖宿主替插件做的事。插件进程本身以你的身份运行，能读写文件、
        // 联网、起子进程——不说清楚的话，"请求权限：无"会被读成"这插件干不了什么"。
        detail.push_str(
            "\n\n注意：上面只是插件向 Smelt 申请的权限。插件进程以你的身份运行，\
             可以读写你的文件、访问网络并启动其他程序，这部分不受上述权限限制。\
             请只安装你信任来源的插件。",
        );
        items.push(
            SettingItem::new(
                "等待确认",
                SettingField::render(move |_, _, cx: &mut App| {
                    div()
                        .text_xs()
                        .text_color(cx.theme().foreground)
                        .child(detail.clone())
                        .into_any_element()
                }),
            )
            .description("确认后会重新校验整包摘要和权限集合；审阅期间包有变化则拒绝安装。"),
        );
        let confirm_entity = entity.clone();
        let cancel_entity = entity;
        items.push(SettingItem::new(
            "安装决定",
            SettingField::render(move |_, _, _| {
                let confirm_entity = confirm_entity.clone();
                let cancel_entity = cancel_entity.clone();
                div()
                    .flex()
                    .gap_2()
                    .child(
                        Button::new("confirm-user-plugin-install")
                            .primary()
                            .small()
                            .label("确认并安装")
                            .on_click(move |_, window, cx| {
                                confirm_entity.update(cx, |workspace, cx| {
                                    workspace.confirm_user_plugin_install(window, cx);
                                });
                            }),
                    )
                    .child(
                        Button::new("cancel-user-plugin-install")
                            .small()
                            .label("取消")
                            .on_click(move |_, _, cx| {
                                cancel_entity.update(cx, |workspace, cx| {
                                    workspace.cancel_user_plugin_install(cx);
                                });
                            }),
                    )
                    .into_any_element()
            }),
        ));
    }
    items.push(
        SettingItem::new(
            "安装位置",
            SettingField::render(move |_, _, cx: &mut App| {
                div()
                    .text_xs()
                    .text_color(cx.theme().muted_foreground)
                    .child("用户插件位于 ~/.smelt/plugins；安装后会在左侧列出。")
                    .into_any_element()
            }),
        )
        .description("应用更新不会覆盖用户插件；整包内容变化会被摘要校验拦截。"),
    );
    SettingPage::new("插件")
        .description("安装第三方 bun 插件，或管理随应用分发的插件。")
        .group(SettingGroup::new().items(items))
}
