//! 能力库「模型」页签：默认模型、Provider、原生配置文件，一列居中排下来。
//!
//! 布局跟着用户要做的事走：先看「现在用的是哪个模型、能不能用」，再管 Provider
//! （内置订阅/API key 和自定义网关合在一张表里），最后才是直接改原生文件的逃生口。
//! 编辑表单就地展开在被点的那一行下面，不追加到页底。

use super::*;
use crate::settings::workspace::PiConfigFileKind;
use smelt_core::pi_auth::PiAuthProvider;
use smelt_core::pi_model_settings::{PiCustomProvider, PiModelSettings};

/// 宽屏上标签和值隔太远就读不成一行了，正文限宽居中。
const PAGE_MAX_WIDTH: f32 = 760.;
const THINKING_LEVELS: [&str; 4] = ["off", "low", "medium", "high"];
/// 列表行标题和侧栏行同走 `text_sm`，不加粗；按钮一律 xsmall。
/// 这页原来套的是设置窗口的 14px 粗体加 small 按钮，夹在侧栏和右栏中间显得又大又重。

#[derive(Clone, Copy)]
pub(super) struct Palette {
    pub(super) fg: Hsla,
    pub(super) muted: Hsla,
    pub(super) border: Hsla,
    pub(super) danger: Hsla,
    pub(super) success: Hsla,
    pub(super) warning: Hsla,
}

impl Palette {
    pub(super) fn new(cx: &App) -> Self {
        let theme = cx.theme();
        Self {
            fg: theme.foreground,
            muted: theme.muted_foreground,
            border: theme.border,
            danger: theme.danger,
            success: theme.success,
            warning: theme.warning,
        }
    }
}

pub(super) fn pi_model_page(
    entity: Entity<Workspace>,
    snapshot: &SettingsRenderSnapshot,
    cx: &App,
) -> AnyElement {
    let palette = Palette::new(cx);
    div()
        .id("pi-model-page")
        .size_full()
        .overflow_y_scroll()
        .child(
            v_flex()
                .w_full()
                .max_w(px(PAGE_MAX_WIDTH))
                .mx_auto()
                .px_6()
                .py_6()
                .gap_8()
                .child(default_model_section(&entity, snapshot, palette))
                .child(provider_section(&entity, snapshot, palette, cx))
                .child(config_files_footer(&entity, palette)),
        )
        .into_any_element()
}

pub(super) fn section(
    title: &'static str,
    action: Option<AnyElement>,
    body: impl IntoElement,
    palette: Palette,
) -> Div {
    v_flex()
        .w_full()
        .gap_2()
        .child(
            h_flex()
                .w_full()
                .min_h(px(28.))
                .items_center()
                .justify_between()
                .child(
                    // 分组标题跟侧栏「最近对话」「项目」一个级别：小号、灰、不加粗。
                    div()
                        .text_xs()
                        .font_medium()
                        .text_color(palette.muted)
                        .child(title),
                )
                .children(action),
        )
        .child(body)
}

pub(super) fn card(palette: Palette) -> Div {
    v_flex()
        .w_full()
        .rounded_lg()
        .border_1()
        .border_color(palette.border)
}

pub(super) fn pill(label: impl Into<SharedString>, color: Hsla) -> Div {
    div()
        .flex_none()
        .px_2()
        .py_0p5()
        .rounded_full()
        .bg(color.opacity(0.14))
        .text_xs()
        .text_color(color)
        .child(label.into())
}

/// 「默认模型」卡片。
///
/// `settings.json` 里没写默认模型时，`load_pi_model_settings` 会填上 Pi 的兜底值
/// （deepseek / deepseek-chat）。两处数据要一起看：标题显示实际会用的模型，状态行
/// 说明它是用户选的还是兜底值，免得「尚未设置」和一个具体模型名同时出现、互相矛盾。
fn default_model_section(
    entity: &Entity<Workspace>,
    snapshot: &SettingsRenderSnapshot,
    palette: Palette,
) -> Div {
    let explicit = explicit_default(snapshot);
    let effective = snapshot.pi_model_settings.as_ref().ok();
    let current = effective.map(|settings| {
        (
            settings.default_model.provider.clone(),
            settings.default_model.model.clone(),
        )
    });
    let title = match (&snapshot.pi_model_settings, &current) {
        (_, Some((provider, model))) => format!("{provider} / {model}"),
        (Err(_), _) => "无法读取模型配置".to_string(),
        _ => String::new(),
    };

    let mut status = h_flex().gap_2().items_center().flex_wrap();
    match &snapshot.pi_settings {
        Err(error) => {
            status = status.child(pill(
                format!("无法读取 settings.json：{error}"),
                palette.danger,
            ));
        }
        Ok(_) if explicit.is_none() => {
            status = status.child(
                div()
                    .text_xs()
                    .text_color(palette.muted)
                    .child("尚未选择，正在用 Pi 的内置默认值"),
            );
        }
        Ok(_) => {}
    }
    match &snapshot.pi_model_settings {
        Ok(settings) if settings.credential_configured => {
            status = status.child(pill("凭据已配置", palette.success));
        }
        Ok(_) => {
            status = status.child(pill("缺少凭据", palette.warning));
        }
        Err(error) => {
            status = status.child(
                div()
                    .text_xs()
                    .text_color(palette.danger)
                    .child(error.clone()),
            );
        }
    }

    let editing = snapshot.pi_model_editor.is_some();
    let change = change_model_button(entity, current.clone(), editing);
    let thinking = thinking_level_row(entity, snapshot, explicit.is_some(), palette);

    let mut body = card(palette)
        .p_4()
        .gap_3()
        .child(
            h_flex()
                .w_full()
                .items_center()
                .justify_between()
                .gap_3()
                .child(
                    v_flex()
                        .flex_1()
                        .min_w_0()
                        .gap_1()
                        .child(
                            div()
                                .truncate()
                                .text_sm()
                                .font_medium()
                                .text_color(palette.fg)
                                .child(title),
                        )
                        .child(status),
                )
                .child(change),
        )
        .child(thinking);

    // 自定义 Provider 表单打开时，它的错误显示在表单里，这里不重复。
    if snapshot.pi_custom_provider_editor.is_none()
        && let Some(error) = &snapshot.pi_model_editor_error
    {
        body = body.child(
            div()
                .text_xs()
                .text_color(palette.danger)
                .child(format!("操作失败：{error}")),
        );
    }
    if let Some(editor) = &snapshot.pi_model_editor {
        body = body.child(default_model_form(
            entity.clone(),
            editor,
            palette.fg,
            palette.muted,
            palette.border,
        ));
    }
    section("默认模型", None, body, palette)
}

/// `settings.json` 里用户真的选过的默认模型；没选过返回 `None`。
fn explicit_default(
    snapshot: &SettingsRenderSnapshot,
) -> Option<&smelt_core::pi_model_settings::PiDefaultModelSummary> {
    snapshot
        .pi_settings
        .as_ref()
        .ok()
        .and_then(|settings| settings.default_model.as_ref())
        .filter(|model| {
            model
                .provider
                .as_deref()
                .is_some_and(|v| !v.trim().is_empty())
                && model.model.as_deref().is_some_and(|v| !v.trim().is_empty())
        })
}

/// 「更换模型」：列出已登录 / 已配置 Provider 的模型，选中即设为默认。
/// 目录在菜单打开时才读盘，不进每次刷新的快照。
fn change_model_button(
    entity: &Entity<Workspace>,
    current: Option<(String, String)>,
    editing: bool,
) -> impl IntoElement {
    let entity = entity.clone();
    Button::new("pi-change-default-model")
        .xsmall()
        .label("更换模型")
        .disabled(editing)
        .dropdown_menu(move |mut menu, window, cx| {
            let groups = smelt_core::pi_model_settings::list_pi_model_choice_groups();
            if groups.is_empty() {
                menu = menu.item(
                    PopupMenuItem::new("还没有可选的模型：先在下面登录或添加 Provider")
                        .disabled(true),
                );
            }
            for group in groups {
                let group_entity = entity.clone();
                let current = current.clone();
                let models = group.models;
                menu = menu.submenu(
                    group.provider_name.clone(),
                    window,
                    cx,
                    move |mut sub, _, _| {
                        for choice in &models {
                            let checked = current.as_ref().is_some_and(|(provider, model)| {
                                provider == &choice.provider && model == &choice.id
                            });
                            let item_entity = group_entity.clone();
                            let provider = choice.provider.clone();
                            let model = choice.id.clone();
                            sub = sub.item(
                                PopupMenuItem::new(choice.short_label().to_string())
                                    .checked(checked)
                                    .on_click(move |_, _, cx| {
                                        let (provider, model) = (provider.clone(), model.clone());
                                        item_entity.update(cx, |workspace, cx| {
                                            workspace.set_pi_default_model(provider, model, cx);
                                        });
                                    }),
                            );
                        }
                        sub.scrollable(true)
                    },
                );
            }
            let manual_entity = entity.clone();
            menu.separator()
                .item(PopupMenuItem::new("手动填写 Provider 和模型…").on_click(
                    move |_, window, cx| {
                        manual_entity.update(cx, |workspace, cx| {
                            workspace.edit_pi_model_settings(window, cx);
                        });
                    },
                ))
        })
}

/// 推理强度直接写盘。没选过默认模型时禁用：写它会顺带把 Pi 的兜底模型固化成
/// 用户的选择。
fn thinking_level_row(
    entity: &Entity<Workspace>,
    snapshot: &SettingsRenderSnapshot,
    enabled: bool,
    palette: Palette,
) -> Div {
    let current = explicit_default(snapshot)
        .and_then(|model| model.thinking_level.clone())
        .or_else(|| {
            snapshot
                .pi_model_settings
                .as_ref()
                .ok()
                .map(|settings| settings.default_model.thinking_level.clone())
        })
        .unwrap_or_default();
    h_flex()
        .w_full()
        .items_center()
        .gap_3()
        .child(
            div()
                .flex_none()
                .text_xs()
                .text_color(palette.muted)
                .child("推理强度"),
        )
        .child(
            h_flex()
                .gap_1()
                .children(THINKING_LEVELS.into_iter().map(|level| {
                    let level_entity = entity.clone();
                    let button = Button::new(format!("pi-default-thinking-{level}"))
                        .xsmall()
                        .label(level)
                        .disabled(!enabled)
                        .on_click(move |_, _, cx| {
                            level_entity.update(cx, |workspace, cx| {
                                workspace.set_pi_default_thinking_level(level.to_string(), cx);
                            });
                        });
                    if current == level {
                        button.primary()
                    } else {
                        button.ghost()
                    }
                })),
        )
        .children((!enabled).then(|| {
            div()
                .text_xs()
                .text_color(palette.muted)
                .child("先选一个默认模型")
        }))
}

/// 一行 Provider：内置的（Pi 认识、走登录）或自定义的（models.json 里的网关）。
#[derive(Debug, PartialEq, Eq)]
enum ProviderEntry<'a> {
    Builtin(&'a PiAuthProvider),
    Custom(&'a PiCustomProvider),
}

impl ProviderEntry<'_> {
    fn id(&self) -> &str {
        match self {
            Self::Builtin(provider) => &provider.id,
            Self::Custom(provider) => &provider.id,
        }
    }

    fn configured(&self) -> bool {
        match self {
            Self::Builtin(provider) => provider.status.configured(),
            // 自定义 Provider 是用户自己加的，不管有没有 key 都算「在用」。
            Self::Custom(_) => true,
        }
    }
}

/// 合并内置与自定义 Provider，返回要显示的行和被折叠掉的内置 Provider 数。
///
/// 排序：当前默认 → 已配置/自定义 → 其余。同一档内保持原有顺序。自定义 Provider
/// 的 id 若也出现在内置列表里（Pi 会把 models.json 里的 provider 一起报上来），只
/// 留自定义那一行：编辑入口在那边。`show_all` 为假时只显示能订阅登录或已配置的内置项，
/// Pi 内置四十来个 provider，用户真正要动的只有这几行。
fn merge_providers<'a>(
    builtin: &'a [PiAuthProvider],
    custom: &'a [PiCustomProvider],
    default_provider: &str,
    show_all: bool,
) -> (Vec<ProviderEntry<'a>>, usize) {
    let mut hidden = 0;
    let mut entries: Vec<ProviderEntry<'a>> = custom.iter().map(ProviderEntry::Custom).collect();
    for provider in builtin {
        if custom.iter().any(|custom| custom.id == provider.id) {
            continue;
        }
        let visible = show_all
            || provider.supports_oauth
            || provider.status.configured()
            || provider.id == default_provider;
        if visible {
            entries.push(ProviderEntry::Builtin(provider));
        } else {
            hidden += 1;
        }
    }
    entries.sort_by_key(|entry| {
        if entry.id() == default_provider {
            0
        } else if entry.configured() {
            1
        } else {
            2
        }
    });
    (entries, hidden)
}

fn provider_section(
    entity: &Entity<Workspace>,
    snapshot: &SettingsRenderSnapshot,
    palette: Palette,
    cx: &App,
) -> Div {
    let settings: Option<&PiModelSettings> = snapshot.pi_model_settings.as_ref().ok();
    let default_provider = settings
        .map(|settings| settings.default_model.provider.as_str())
        .unwrap_or_default();
    let current_default_model = settings.map(|settings| settings.default_model.model.as_str());
    let custom: &[PiCustomProvider] = settings
        .map(|settings| settings.custom_providers.as_slice())
        .unwrap_or_default();
    let builtin: &[PiAuthProvider] = match &snapshot.pi_auth_providers {
        Some(PiAuthProvidersState::Ready(list)) => list,
        _ => &[],
    };
    let (entries, hidden) =
        merge_providers(builtin, custom, default_provider, snapshot.pi_auth_show_all);
    let custom_editor = snapshot.pi_custom_provider_editor.as_ref();
    let login = snapshot.pi_login.as_ref();
    let picker = snapshot.pi_auth_model_picker.as_ref();
    let login_busy = login.is_some();

    let add_entity = entity.clone();
    let add = Button::new("pi-add-custom-provider")
        .ghost()
        .xsmall()
        .icon(IconName::Plus)
        .label("自定义 Provider")
        .tooltip("OpenAI / Anthropic 兼容网关、自托管服务或其他模型端点")
        .disabled(custom_editor.is_some())
        .on_click(move |_, window, cx| {
            add_entity.update(cx, |workspace, cx| {
                workspace.edit_pi_custom_provider(None, window, cx);
            });
        })
        .into_any_element();

    let mut body = v_flex().w_full().gap_2();
    if let Some(error) = &snapshot.pi_auth_error {
        body = body.child(
            div()
                .text_xs()
                .text_color(palette.danger)
                .child(format!("操作失败：{error}")),
        );
    }
    // 新增表单放在表头，编辑表单展开在对应那一行下面。
    if let Some(editor) = custom_editor.filter(|editor| editor.previous_id.is_none()) {
        body = body.child(custom_provider_form(
            entity.clone(),
            editor,
            snapshot.pi_model_editor_error.clone(),
            palette,
            cx,
        ));
    }

    let mut list = card(palette);
    let mut rendered_login = false;
    let mut rendered_picker = false;
    for (index, entry) in entries.iter().enumerate() {
        let mut row = v_flex().w_full().px_4().py_3().gap_3();
        if index > 0 {
            row = row.border_t_1().border_color(palette.border);
        }
        match entry {
            ProviderEntry::Builtin(provider) => {
                row = row.child(super::pi_auth::builtin_provider_row(
                    entity.clone(),
                    provider,
                    provider.id == default_provider,
                    login_busy,
                    palette,
                ));
                if let Some(view) = login.filter(|view| view.provider_id == provider.id) {
                    rendered_login = true;
                    row = row.child(super::pi_auth::login_panel(
                        entity.clone(),
                        view,
                        current_default_model,
                        cx,
                    ));
                }
                if let Some(picker) = picker.filter(|picker| picker.provider_id == provider.id) {
                    rendered_picker = true;
                    row = row.child(super::pi_auth::model_picker_panel(
                        entity.clone(),
                        picker,
                        current_default_model,
                        palette.fg,
                        palette.muted,
                        palette.border,
                    ));
                }
            }
            ProviderEntry::Custom(provider) => {
                row = row.child(custom_provider_row(
                    entity,
                    provider,
                    provider.id == default_provider,
                    custom_editor.is_some(),
                    palette,
                ));
                if let Some(editor) = custom_editor
                    .filter(|editor| editor.previous_id.as_deref() == Some(provider.id.as_str()))
                {
                    row = row.child(custom_provider_form(
                        entity.clone(),
                        editor,
                        snapshot.pi_model_editor_error.clone(),
                        palette,
                        cx,
                    ));
                }
            }
        }
        list = list.child(row);
    }

    // 登录 / 选模型的那个 Provider 可能被折叠了（比如刚切回「只看常用」），面板不能跟着消失。
    let mut orphans = v_flex().w_full().gap_2();
    if let Some(view) = login.filter(|_| !rendered_login) {
        orphans = orphans.child(super::pi_auth::login_panel(
            entity.clone(),
            view,
            current_default_model,
            cx,
        ));
    }
    if let Some(picker) = picker.filter(|_| !rendered_picker) {
        orphans = orphans.child(super::pi_auth::model_picker_panel(
            entity.clone(),
            picker,
            current_default_model,
            palette.fg,
            palette.muted,
            palette.border,
        ));
    }

    let footer = provider_list_footer(entity, snapshot, hidden, !entries.is_empty(), palette);
    list = list.child(footer);
    body = body.child(orphans).child(list);
    section("Provider", Some(add), body, palette)
}

/// 表尾：读取状态，以及「显示全部内置 Provider」开关。
fn provider_list_footer(
    entity: &Entity<Workspace>,
    snapshot: &SettingsRenderSnapshot,
    hidden: usize,
    has_rows: bool,
    palette: Palette,
) -> Div {
    let mut footer = h_flex()
        .w_full()
        .min_h(px(36.))
        .px_4()
        .py_1()
        .gap_2()
        .items_center()
        .justify_between();
    if has_rows {
        footer = footer.border_t_1().border_color(palette.border);
    }
    let retry_entity = entity.clone();
    let retry = move |label: &'static str| {
        let entity = retry_entity.clone();
        Button::new("pi-auth-load")
            .ghost()
            .xsmall()
            .label(label)
            .on_click(move |_, _, cx| {
                entity.update(cx, |workspace, cx| workspace.refresh_pi_auth_providers(cx));
            })
    };
    match &snapshot.pi_auth_providers {
        // 进页签时会自动读；这里只是读之前的一帧。
        None | Some(PiAuthProvidersState::Loading) => footer.child(
            div()
                .text_xs()
                .text_color(palette.muted)
                .child("正在读取内置 Provider 的登录状态…首次需要准备 Pi 运行时，可能要等一会"),
        ),
        Some(PiAuthProvidersState::Failed(error)) => footer
            .child(
                div()
                    .min_w_0()
                    .truncate()
                    .text_xs()
                    .text_color(palette.danger)
                    .child(format!("读取内置 Provider 失败：{error}")),
            )
            .child(retry("重试")),
        Some(PiAuthProvidersState::Ready(_)) => {
            let show_all = snapshot.pi_auth_show_all;
            let toggle_entity = entity.clone();
            let toggle = (show_all || hidden > 0).then(|| {
                Button::new("pi-auth-toggle-all")
                    .ghost()
                    .xsmall()
                    .label(if show_all {
                        "只看常用".to_string()
                    } else {
                        format!("显示全部内置 Provider（另有 {hidden} 个）")
                    })
                    .on_click(move |_, _, cx| {
                        toggle_entity.update(cx, |workspace, cx| {
                            workspace.toggle_pi_auth_show_all(cx);
                        });
                    })
            });
            footer
                .child(h_flex().children(toggle))
                .child(retry("刷新状态"))
        }
    }
}

fn custom_provider_row(
    entity: &Entity<Workspace>,
    provider: &PiCustomProvider,
    is_default: bool,
    editor_open: bool,
    palette: Palette,
) -> Div {
    let edit_entity = entity.clone();
    let delete_entity = entity.clone();
    let provider_id = provider.id.clone();
    let delete_id = provider.id.clone();
    let name = if provider.display_name.trim().is_empty() {
        provider.id.clone()
    } else {
        provider.display_name.clone()
    };
    let credential = if provider.credential_configured {
        "凭据已配置"
    } else {
        "无凭据"
    };
    h_flex()
        .w_full()
        .items_center()
        .justify_between()
        .gap_3()
        .child(
            v_flex()
                .flex_1()
                .min_w_0()
                .gap_0p5()
                .child(
                    h_flex()
                        .gap_2()
                        .items_center()
                        .child(
                            div()
                                .truncate()
                                .text_sm()
                                .text_color(palette.fg)
                                .child(name),
                        )
                        .child(pill("自定义", palette.muted))
                        .children(is_default.then(|| pill("默认", palette.success))),
                )
                .child(
                    div()
                        .truncate()
                        .text_xs()
                        .text_color(palette.muted)
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
                .flex_none()
                .gap_1()
                .child(
                    Button::new(format!("pi-edit-custom-provider-{}", provider.id))
                        .ghost()
                        .xsmall()
                        .label("编辑")
                        .disabled(editor_open)
                        .on_click(move |_, window, cx| {
                            let provider_id = provider_id.clone();
                            edit_entity.update(cx, |workspace, cx| {
                                workspace.edit_pi_custom_provider(Some(provider_id), window, cx);
                            });
                        }),
                )
                .child(
                    Button::new(format!("pi-delete-custom-provider-{delete_id}"))
                        .ghost()
                        .xsmall()
                        .icon(crate::trash_icon())
                        .tooltip("删除此 Provider")
                        .disabled(editor_open)
                        .on_click(move |_, window, cx| {
                            let delete_id = delete_id.clone();
                            delete_entity.update(cx, |workspace, cx| {
                                workspace.remove_pi_custom_provider(delete_id, window, cx);
                            });
                        }),
                ),
        )
}

/// 直接改 Pi 原生文件的逃生口。平时用不到，压成一行放在页底。
fn config_files_footer(entity: &Entity<Workspace>, palette: Palette) -> Div {
    let file_button = |id: &'static str,
                       label: &'static str,
                       kind: PiConfigFileKind,
                       path: std::path::PathBuf| {
        let entity = entity.clone();
        Button::new(id)
            .ghost()
            .xsmall()
            .label(label)
            .tooltip(path.display().to_string())
            .on_click(move |_, window, cx| {
                entity.update(cx, |workspace, cx| {
                    workspace.open_pi_config(kind, window, cx);
                });
            })
    };
    let refresh_entity = entity.clone();
    h_flex()
        .w_full()
        .items_center()
        .gap_1()
        .pt_3()
        .border_t_1()
        .border_color(palette.border)
        .child(
            div()
                .flex_none()
                .mr_2()
                .text_xs()
                .text_color(palette.muted)
                .child("原生配置文件"),
        )
        .child(file_button(
            "pi-edit-settings-file",
            "settings.json",
            PiConfigFileKind::Settings,
            smelt_core::pi_model_settings::pi_settings_path(),
        ))
        .child(file_button(
            "pi-edit-models-file",
            "models.json",
            PiConfigFileKind::Models,
            smelt_core::pi_model_settings::pi_models_path(),
        ))
        .child(file_button(
            "pi-edit-credentials-file",
            "auth.json",
            PiConfigFileKind::Credentials,
            smelt_core::pi_model_settings::pi_auth_path(),
        ))
        .child(div().flex_1())
        .child(
            Button::new("pi-refresh-settings")
                .ghost()
                .xsmall()
                .icon(IconName::Redo)
                .tooltip("重新读取 ~/.pi/agent/ 下的配置")
                .on_click(move |_, _, cx| {
                    refresh_entity.update(cx, |workspace, cx| {
                        workspace.refresh_pi_settings(cx);
                        workspace.refresh_pi_auth_providers(cx);
                    });
                }),
        )
}

/// 「手动填写」：直接填 provider / 模型 ID / 地址 / key。给菜单里找不到的模型用。
fn default_model_form(
    manage_entity: Entity<Workspace>,
    editor: &PiModelEditor,
    fg: Hsla,
    muted: Hsla,
    border: Hsla,
) -> AnyElement {
    let save_entity = manage_entity.clone();
    let cancel_entity = manage_entity.clone();
    let credential_status = if editor.credential_configured {
        "当前 API key 已配置；留空不会改动它。"
    } else {
        "尚未检测到 API key；请在下方输入。"
    };

    let form = v_flex()
        .w_full()
        .gap_4()
        .pt_4()
        .border_t_1()
        .border_color(border)
        .child(
            v_flex()
                .gap_1()
                .child(
                    div()
                        .text_sm()
                        .font_semibold()
                        .text_color(fg)
                        .child("手动填写 Provider 和模型"),
                )
                .child(div().text_xs().text_color(muted).child(credential_status)),
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
        );
    form.into_any_element()
}

fn custom_provider_form(
    manage_entity: Entity<Workspace>,
    editor: &PiCustomProviderEditor,
    model_editor_error: Option<String>,
    palette: Palette,
    cx: &App,
) -> AnyElement {
    let Palette {
        fg,
        muted,
        border,
        danger: danger_fg,
        ..
    } = palette;
    let api_entity = manage_entity.clone();
    let add_model_entity = manage_entity.clone();
    let discover_entity = manage_entity.clone();
    let discovery_state = editor.discovery.clone();
    let manual_rows = editor.models.len();
    let discovery_busy = matches!(discovery_state, Some(ModelDiscoveryState::Loading));
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
                                .icon(crate::trash_icon())
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
                                .child(div().text_xs().text_color(muted).child("模型 ID"))
                                .child(Input::new(&model.id)),
                        )
                        .child(
                            v_flex()
                                .flex_1()
                                .min_w_0()
                                .gap_1()
                                .child(div().text_xs().text_color(muted).child("显示名称"))
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
                                .child(div().text_xs().text_color(muted).child("最大输出（可选）"))
                                .child(Input::new(&model.max_tokens)),
                        ),
                ),
        );
    }

    let form = v_flex()
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
                        .child(div().text_sm().font_semibold().text_color(fg).child(
                            if editor.previous_id.is_some() {
                                "编辑自定义 Provider"
                            } else {
                                "添加自定义 Provider"
                            },
                        ))
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
                .child(div().text_xs().text_color(muted).child(credential_status)),
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
                        .child(div().text_xs().font_semibold().text_color(fg).child("协议"))
                        .child(
                            Button::new("pi-custom-provider-api")
                                .secondary()
                                .small()
                                .label(editor.api.clone())
                                .dropdown_menu(move |mut menu, _window, _cx| {
                                    for api in smelt_core::provider_api::PROVIDER_API_PROTOCOLS {
                                        let api_entity = api_entity.clone();
                                        menu = menu.item(PopupMenuItem::new(*api).on_click(
                                            move |_ev, _window, cx| {
                                                api_entity.update(cx, |workspace, cx| {
                                                    workspace.set_pi_custom_api(api, cx);
                                                });
                                            },
                                        ));
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
                                    discover_entity.update(cx, |workspace, cx| {
                                        workspace.discover_pi_custom_models(cx);
                                    });
                                }),
                        )
                        .child(
                            Button::new("pi-add-custom-model")
                                .secondary()
                                .small()
                                .icon(IconName::Plus)
                                .label("添加模型")
                                .on_click(move |_, window, cx| {
                                    add_model_entity.update(cx, |workspace, cx| {
                                        workspace.add_pi_custom_model(window, cx);
                                    });
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
        .children(
            model_editor_error
                .clone()
                .map(|error| div().text_xs().text_color(danger_fg).child(error)),
        )
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
                            cancel_entity.update(cx, |workspace, cx| {
                                workspace.cancel_pi_custom_provider(cx);
                            });
                        }),
                )
                .child(
                    Button::new("pi-save-custom-provider")
                        .secondary()
                        .small()
                        .label("保存")
                        .on_click(move |_, window, cx| {
                            save_entity.update(cx, |workspace, cx| {
                                workspace.save_pi_custom_provider(false, window, cx);
                            });
                        }),
                )
                .child(
                    Button::new("pi-save-custom-provider-default")
                        .small()
                        .label("保存并设为默认")
                        .on_click(move |_, window, cx| {
                            save_default_entity.update(cx, |workspace, cx| {
                                workspace.save_pi_custom_provider(true, window, cx);
                            });
                        }),
                ),
        );
    form.into_any_element()
}

#[cfg(test)]
mod tests {
    // 不用 `super::*`：那条 glob 链会带进 `gpui::*` 里的 `test` 属性宏，把这里的
    // `#[test]` 换成 gpui 的版本，展开时撞上递归上限。
    use super::{ProviderEntry, merge_providers};
    use smelt_core::pi_auth::{PiAuthProvider, PiAuthStatus};
    use smelt_core::pi_model_settings::PiCustomProvider;

    fn builtin(id: &str, oauth: bool, status: PiAuthStatus) -> PiAuthProvider {
        PiAuthProvider {
            id: id.into(),
            name: id.into(),
            supports_oauth: oauth,
            status,
            ..Default::default()
        }
    }

    fn custom(id: &str) -> PiCustomProvider {
        PiCustomProvider {
            id: id.into(),
            ..Default::default()
        }
    }

    fn ids(entries: &[ProviderEntry<'_>]) -> Vec<String> {
        entries.iter().map(|entry| entry.id().to_string()).collect()
    }

    #[test]
    fn merged_providers_put_default_then_configured_first() {
        let builtin = vec![
            builtin("copilot", true, PiAuthStatus::None),
            builtin("openai", false, PiAuthStatus::ApiKey),
            builtin("anthropic", true, PiAuthStatus::Oauth),
        ];
        let custom = vec![custom("gateway")];
        let (entries, hidden) = merge_providers(&builtin, &custom, "anthropic", false);
        assert_eq!(ids(&entries), ["anthropic", "gateway", "openai", "copilot"]);
        assert_eq!(hidden, 0);
    }

    #[test]
    fn rarely_used_builtins_fold_until_show_all() {
        let builtin = vec![
            builtin("groq", false, PiAuthStatus::None),
            builtin("copilot", true, PiAuthStatus::None),
        ];
        let (entries, hidden) = merge_providers(&builtin, &[], "", false);
        assert_eq!(ids(&entries), ["copilot"]);
        assert_eq!(hidden, 1);

        let (entries, hidden) = merge_providers(&builtin, &[], "", true);
        assert_eq!(ids(&entries), ["groq", "copilot"]);
        assert_eq!(hidden, 0);
    }

    #[test]
    fn default_builtin_stays_visible_even_when_folded() {
        let builtin = vec![builtin("deepseek", false, PiAuthStatus::None)];
        let (entries, hidden) = merge_providers(&builtin, &[], "deepseek", false);
        assert_eq!(
            ids(&entries),
            ["deepseek"],
            "兜底的默认 Provider 缺凭据时，用户得能在列表里找到它去填 key"
        );
        assert_eq!(hidden, 0);
    }

    #[test]
    fn custom_provider_shadows_builtin_with_same_id() {
        let builtin = vec![builtin("gateway", false, PiAuthStatus::ApiKey)];
        let custom = vec![custom("gateway")];
        let (entries, _) = merge_providers(&builtin, &custom, "", false);
        assert_eq!(entries, vec![ProviderEntry::Custom(&custom[0])]);
    }
}
